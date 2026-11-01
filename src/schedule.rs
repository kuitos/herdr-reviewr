//! Refresh pacing: watcher batches gather for a debounce, then wait out a budget of 20 times the
//! last background refresh's time. A hidden pane gathers for 30s; the reviewer's refreshes never wait.

use std::time::{Duration, Instant};

use crate::world::Refresh;

/// How long the first change of a burst waits for the rest, while the pane is on screen.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// The same wait while the pane is hidden: it refreshes slowly, and only after a change.
const HIDDEN_DEBOUNCE: Duration = Duration::from_secs(30);
/// The next background refresh waits this many times as long as the last one took, which keeps
/// its git under 1/21 of a core.
const BUDGET_FACTOR: u32 = 20;
/// While the watcher cannot run, a full refresh stands in for it at most this often.
const STAND_IN_EVERY: Duration = Duration::from_secs(5);

/// The first retry after a failure, doubling up to [`RETRY_MAX`].
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_mins(1);

/// A retry's waits: [`RETRY_FIRST`], doubling up to [`RETRY_MAX`]. A fresh one starts over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    wait: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self { wait: RETRY_FIRST }
    }
}

impl Backoff {
    /// The next wait, doubling the one after it.
    pub fn next_wait(&mut self) -> Duration {
        let wait = self.wait;
        self.wait = (wait * 2).min(RETRY_MAX);
        wait
    }
}

/// The pacing state. Time is passed in, so every rule is testable without a clock.
#[derive(Debug)]
pub struct Pacer {
    /// What the next background refresh re-reads, and when its oldest change arrived.
    pending: Option<(Refresh, Instant)>,
    /// No background refresh before this: the budget gap after the last one.
    gap_until: Option<Instant>,
    /// A failure being retried; `None` once a refresh lands.
    retry: Option<Retry>,
    /// The job in flight is background work, so its landing spends the budget.
    charging: bool,
    /// While the watcher cannot run, the next stand-in full refresh.
    stand_in: Option<Instant>,
    visible: bool,
}

/// A failed refresh's retry: not before `at`, its waits doubling. A hidden pane retries only
/// once something changed since the failure.
#[derive(Clone, Copy, Debug)]
struct Retry {
    at: Instant,
    backoff: Backoff,
    changed: bool,
}

impl Default for Pacer {
    fn default() -> Self {
        Self {
            pending: None,
            gap_until: None,
            retry: None,
            charging: false,
            stand_in: None,
            visible: true,
        }
    }
}

impl Pacer {
    /// A watcher batch's refresh joins what is pending; one that re-reads nothing arms nothing.
    pub fn on_batch(&mut self, refresh: Refresh, now: Instant) {
        if refresh.is_empty() {
            return;
        }
        self.pend(refresh, now);
        if let Some(retry) = self.retry.as_mut() {
            retry.changed = true;
        }
    }

    fn pend(&mut self, refresh: Refresh, now: Instant) {
        match self.pending.as_mut() {
            Some((pending, _)) => pending.absorb(refresh),
            None => self.pending = Some((refresh, now)),
        }
    }

    /// A refresh went out: background work spends the budget when it lands, the reviewer's never.
    pub fn on_dispatched(&mut self, background: bool) {
        self.charging = background;
    }

    /// A refresh landed: any landing ends a backoff, and background work that took `took` (wall
    /// time, which bounds its CPU) spends the budget.
    pub fn on_landed(&mut self, took: Duration, now: Instant) {
        if std::mem::take(&mut self.charging) {
            self.gap_until = Some(now + took * BUDGET_FACTOR);
        }
        self.retry = None;
    }

    /// A refresh failed: its paths go back to pending, retried on a doubling backoff.
    pub fn on_failed(&mut self, refresh: Refresh, now: Instant) {
        self.pend(refresh, now);
        let mut backoff = self.retry.map(|r| r.backoff).unwrap_or_default();
        self.retry = Some(Retry { at: now + backoff.next_wait(), backoff, changed: false });
    }

    /// The watcher stopped or recovered: a full refresh stands in for it every [`STAND_IN_EVERY`].
    pub fn set_watcher_down(&mut self, down: bool, now: Instant) {
        self.stand_in = down.then(|| self.stand_in.unwrap_or(now));
    }

    /// Whether a full refresh stands in for a watcher that cannot run.
    pub fn watcher_down(&self) -> bool {
        self.stand_in.is_some()
    }

    /// Whether the pane is on screen: a hidden one gathers longer, retries nothing and stands in for nothing.
    pub fn set_visible(&mut self, visible: bool) {
        // Shown, what it gathered is due at once: the budget yields, a job in flight's included.
        if visible && !self.visible {
            self.gap_until = None;
            self.charging = false;
        }
        self.visible = visible;
    }

    /// When the pending refresh may dispatch, or `None` when nothing will.
    pub fn next_deadline(&self) -> Option<Instant> {
        let debounce = if self.visible { DEBOUNCE } else { HIDDEN_DEBOUNCE };
        let held = !self.visible && self.retry.is_some_and(|r| !r.changed);
        let pending = self.pending.as_ref().filter(|_| !held).map(|(_, since)| {
            [Some(*since + debounce), self.gap_until, self.retry.map(|r| r.at)]
                .into_iter()
                .flatten()
                .max()
        });
        let stand_in = self
            .stand_in
            .filter(|_| self.visible)
            .map(|at| [Some(at), self.gap_until].into_iter().flatten().max());
        [pending.flatten(), stand_in.flatten()].into_iter().flatten().min()
    }

    /// The pending refresh, if its deadline has come.
    pub fn take_due(&mut self, now: Instant) -> Option<Refresh> {
        if self.next_deadline()? > now {
            return None;
        }
        if let Some(at) = self.stand_in.filter(|_| self.visible)
            && at.max(self.gap_until.unwrap_or(at)) <= now
        {
            self.stand_in = Some(now + STAND_IN_EVERY);
            self.pending = None;
            return Some(Refresh::Full);
        }
        self.take_now()
    }

    /// The pending refresh, at once: a refresh the reviewer asked for carries it along.
    pub fn take_now(&mut self) -> Option<Refresh> {
        self.pending.take().map(|(refresh, _)| refresh)
    }
}

#[cfg(test)]
mod tests {
    use super::{BUDGET_FACTOR, Backoff, DEBOUNCE, HIDDEN_DEBOUNCE, Pacer, RETRY_FIRST, RETRY_MAX};
    use crate::world::Refresh;
    use std::time::{Duration, Instant};

    fn paths(list: &[&str]) -> Refresh {
        Refresh::paths(list.iter().map(|p| (*p).to_string()))
    }

    fn count(refresh: &Refresh) -> Option<usize> {
        refresh.named_paths().map(std::collections::BTreeSet::len)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_burst_dispatches_once_with_every_path_after_the_debounce() {
        let t0 = Instant::now();
        let mut p = Pacer::default();
        p.on_batch(paths(&["a"]), t0);
        p.on_batch(paths(&["b"]), t0 + ms(40));
        p.on_batch(paths(&["c"]), t0 + ms(120));
        assert_eq!(p.take_due(t0 + ms(149)), None, "not before the first change has waited");
        assert_eq!(p.next_deadline(), Some(t0 + DEBOUNCE), "the burst cannot push it later");
        let due = p.take_due(t0 + DEBOUNCE).expect("due");
        assert_eq!(count(&due), Some(3), "one dispatch carries the union");
        assert_eq!(p.take_due(t0 + ms(10_000)), None, "and nothing is left");
    }

    #[test]
    fn the_next_background_refresh_waits_twenty_times_the_last_ones_cost() {
        let t0 = Instant::now();
        let mut p = Pacer::default();
        p.on_dispatched(true);
        p.on_landed(ms(30), t0);
        p.on_batch(paths(&["a"]), t0 + ms(10));
        assert_eq!(p.next_deadline(), Some(t0 + ms(30) * BUDGET_FACTOR));
        assert!(p.take_due(t0 + ms(599)).is_none());
        assert!(p.take_due(t0 + ms(600)).is_some());
    }

    #[test]
    fn a_minute_long_storm_stays_under_five_percent_of_a_core() {
        // A change every 10ms; each refresh costs 22ms (path-limited, 50k files) or 110ms (full).
        let t0 = Instant::now();
        let mut p = Pacer::default();
        let mut busy = Duration::ZERO;
        let mut i = 0_u32;
        let mut now = t0;
        // A dispatch lands only after it ran: none starts while one is in flight.
        let mut in_flight: Option<Instant> = None;
        while now < t0 + Duration::from_mins(1) {
            p.on_batch(paths(&["a"]), now);
            if in_flight.is_some_and(|done| done <= now) {
                in_flight = None;
            }
            if in_flight.is_none() && p.take_due(now).is_some() {
                let took = if i.is_multiple_of(2) { ms(22) } else { ms(110) };
                busy += took;
                i += 1;
                p.on_dispatched(true);
                p.on_landed(took, now + took);
                in_flight = Some(now + took);
            }
            now += ms(10);
        }
        let share = busy.as_secs_f64() / 60.0;
        assert!(share < 0.05, "background work took {:.1}% of the minute", share * 100.0);
        assert!(i > 10, "it still refreshed ({i} times)");
    }

    #[test]
    fn a_change_that_re_reads_nothing_is_no_refresh() {
        let mut p = Pacer::default();
        p.on_batch(Refresh::default(), Instant::now());
        assert_eq!(p.next_deadline(), None, "a config-only batch arms nothing");
    }

    #[test]
    fn a_stand_in_full_refresh_runs_while_the_watcher_is_down_and_the_pane_is_shown() {
        let t0 = Instant::now();
        let mut p = Pacer::default();
        p.set_watcher_down(true, t0);
        let first = p.take_due(t0).expect("at once");
        assert!(first.is_full(), "a full refresh");
        assert_eq!(p.take_due(t0 + ms(4_999)), None, "not before five seconds");
        p.on_dispatched(true);
        p.on_landed(ms(400), t0 + ms(100));
        assert_eq!(
            p.next_deadline(),
            Some(t0 + ms(100) + ms(400) * BUDGET_FACTOR),
            "the budget can stretch it"
        );
        p.set_visible(false);
        assert_eq!(p.next_deadline(), None, "a hidden pane stands in for nothing");
        p.set_visible(true);
        p.set_watcher_down(false, t0);
        assert_eq!(p.next_deadline(), None, "and nothing once the watcher is back");
    }

    #[test]
    fn a_reviewers_refresh_takes_the_pending_paths_at_once() {
        let t0 = Instant::now();
        let mut p = Pacer::default();
        p.on_dispatched(true);
        p.on_landed(ms(500), t0);
        p.on_batch(paths(&["a"]), t0);
        assert_eq!(p.take_now().as_ref().and_then(count), Some(1), "no debounce, no gap");
        assert_eq!(p.next_deadline(), None);
    }

    #[test]
    fn a_failed_refresh_keeps_its_paths_and_retries_on_a_doubling_backoff() {
        let t0 = Instant::now();
        let mut p = Pacer::default();
        p.on_failed(paths(&["a"]), t0);
        assert_eq!(p.next_deadline(), Some(t0 + RETRY_FIRST));
        let retry = p.take_due(t0 + RETRY_FIRST).expect("retried");
        assert!(retry.named_paths().is_some_and(|p| p.contains("a")), "with its paths");
        p.on_failed(retry, t0 + RETRY_FIRST);
        assert_eq!(p.next_deadline(), Some(t0 + RETRY_FIRST * 3), "then two seconds later");
        let mut at = t0;
        for _ in 0..10 {
            let r = p.take_now().unwrap_or_default();
            p.on_failed(r, at);
            at += Duration::from_secs(1);
        }
        let wait = p.next_deadline().unwrap() - at.checked_sub(Duration::from_secs(1)).unwrap();
        assert_eq!(wait, RETRY_MAX, "capped at a minute");
        p.on_dispatched(true);
        p.on_landed(ms(10), at);
        p.on_batch(paths(&["b"]), at);
        assert_eq!(
            p.next_deadline(),
            Some(at + DEBOUNCE.max(ms(10) * BUDGET_FACTOR)),
            "a landing resets it"
        );
    }

    #[test]
    fn a_reviewers_landing_ends_a_backoff_without_spending_the_budget() {
        let t0 = Instant::now();
        let mut p = Pacer::default();
        for i in 0..5 {
            p.on_failed(paths(&["a"]), t0 + ms(i));
        }
        p.take_now();
        p.on_dispatched(false);
        p.on_landed(ms(400), t0 + ms(10));
        p.on_batch(paths(&["b"]), t0 + ms(10));
        assert_eq!(p.next_deadline(), Some(t0 + ms(10) + DEBOUNCE), "no backoff, no budget gap");
    }

    #[test]
    fn a_hidden_pane_refreshes_thirty_seconds_after_a_change_and_never_without_one() {
        let t0 = Instant::now();
        let mut p = Pacer::default();
        p.set_visible(false);
        assert_eq!(p.next_deadline(), None, "nothing pending runs nothing");
        p.on_batch(paths(&["a"]), t0);
        p.on_batch(paths(&["b"]), t0 + Duration::from_secs(10));
        assert_eq!(p.next_deadline(), Some(t0 + HIDDEN_DEBOUNCE), "armed by the first change");
        let due = p.take_due(t0 + HIDDEN_DEBOUNCE).expect("one refresh");
        assert_eq!(count(&due), Some(2), "over every changed path");
        assert_eq!(p.next_deadline(), None, "and nothing after it");
    }

    #[test]
    fn showing_the_pane_makes_what_it_gathered_due_at_once_past_the_budget() {
        let t0 = Instant::now();
        let mut p = Pacer::default();
        p.set_visible(false);
        // A 2s refresh: its 40s budget outlasts the 30s hidden wait.
        p.on_dispatched(true);
        p.on_landed(Duration::from_secs(2), t0);
        p.on_batch(paths(&["a"]), t0);
        assert_eq!(p.next_deadline(), Some(t0 + BUDGET_FACTOR * Duration::from_secs(2)));
        p.set_visible(true);
        assert_eq!(p.next_deadline(), Some(t0 + DEBOUNCE), "no 30s wait and no budget once shown");
        assert_eq!(p.take_due(t0 + DEBOUNCE).as_ref().and_then(count), Some(1));
        // A background job sent while hidden lands after the pane is shown: it charges nothing.
        p.set_visible(false);
        p.on_dispatched(true);
        p.on_batch(paths(&["b"]), t0 + ms(200));
        p.set_visible(true);
        p.on_landed(Duration::from_secs(2), t0 + ms(300));
        assert_eq!(
            p.next_deadline(),
            Some(t0 + ms(200) + DEBOUNCE),
            "the edits after it run at once"
        );
    }

    #[test]
    fn a_hidden_pane_retries_a_failure_only_once_shown_or_changed() {
        let t0 = Instant::now();
        let mut p = Pacer::default();
        p.set_visible(false);
        p.on_failed(paths(&["a"]), t0);
        assert_eq!(p.next_deadline(), None, "a hidden failure waits");
        p.set_visible(true);
        assert_eq!(p.next_deadline(), Some(t0 + RETRY_FIRST), "shown, it retries");
        p.set_visible(false);
        p.on_batch(paths(&["b"]), t0 + ms(10));
        assert_eq!(p.next_deadline(), Some(t0 + HIDDEN_DEBOUNCE), "a change re-arms it");
    }

    #[test]
    fn a_backoff_doubles_from_a_second_to_a_minute() {
        let mut backoff = Backoff::default();
        let waits: Vec<u64> = (0..8).map(|_| backoff.next_wait().as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(
            Backoff::default().next_wait(),
            Duration::from_secs(1),
            "a fresh one starts over"
        );
    }
}
