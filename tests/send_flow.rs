//! End-to-end send dispatch through a fake herdr binary. This file is its own test process, so the HERDR_* environment it
//! sets can never leak into another test binary, and no real herdr pane is ever addressed.
#![cfg(unix)]

mod common;

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::{Repo, app_on};
use herdr_reviewr::app::{App, Focus, Mode};
use herdr_reviewr::keymap::Keymap;
use herdr_reviewr::selection::{Point, Surface, TextDrag};
use herdr_reviewr::ui;
use herdr_reviewr::{handle_key, handle_mouse};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

// `cwd` rides every real `agent list` entry (api notes). Send ignores it and resolves from
// the workspace, so it is here to keep the fixture honest rather than to steer the send.
const TWO_AGENTS: &str = r#"{"result":{"agents":[
  {"agent":"claude","agent_status":"idle","pane_id":"w8:p1","tab_id":"w8:t1","workspace_id":"w8","cwd":"/w/one"},
  {"agent":"codex","agent_status":"working","pane_id":"w8:p2","tab_id":"w8:t1","workspace_id":"w8","cwd":"/w/two"}
]}}"#;
const ONE_AGENT: &str = r#"{"result":{"agents":[
  {"agent":"claude","agent_status":"idle","pane_id":"w8:p1","tab_id":"w8:t1","workspace_id":"w8","cwd":"/w/one"}
]}}"#;

/// A fake herdr: answers `agent list` from `agents.json`, `agent get` from `agent.json`,
/// `pane read` from `screen.txt`, `tab list` with one label, logs every invocation, and
/// succeeds at everything else (`pane send-text`, `pane send-keys`, `agent focus`). It fails
/// whatever `fail` holds, so a dead pane and a broken enumeration both have a shape.
fn write_fake_herdr(dir: &Path) -> PathBuf {
    let script = dir.join("herdr");
    fs::write(
        &script,
        "#!/bin/sh\n\
         dir=$(dirname \"$0\")\n\
         echo \"$@\" >> \"$dir/log\"\n\
         case \"$*\" in\n\
           $(cat \"$dir/fail\" 2>/dev/null || echo __none__)*)\n\
             echo '{\"error\":{\"code\":\"pane_not_found\",\"message\":\"pane w8:p1 not found\"},\"id\":\"cli:request\"}' >&2\n\
             exit 1 ;;\n\
         esac\n\
         case \"$1 $2\" in\n\
           \"agent list\") cat \"$dir/agents.json\" ;;\n\
           \"agent get\") cat \"$dir/agent.json\" ;;\n\
           \"pane read\") cat \"$dir/screen.txt\" ;;\n\
           \"tab list\") echo '{\"result\":{\"tabs\":[{\"tab_id\":\"w8:t1\",\"label\":\"Grip\"}]}}' ;;\n\
           *) : ;;\n\
         esac\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// Make the fake herdr exit non-zero for every invocation starting with `prefix`.
fn fail_on(dir: &Path, prefix: &str) {
    fs::write(dir.join("fail"), prefix).unwrap();
}

fn fail_on_nothing(dir: &Path) {
    let _ = fs::remove_file(dir.join("fail"));
}

fn log(dir: &Path) -> String {
    fs::read_to_string(dir.join("log")).unwrap_or_default()
}

/// What `agent get` reports for the chosen pane, and what its visible screen shows.
fn agent_shows(dir: &Path, status: &str, screen: &str) {
    let json = format!(
        r#"{{"id":"cli:agent:get","result":{{"agent":{{"agent":"claude","agent_status":"{status}","pane_id":"w8:p1","tab_id":"w8:t1","workspace_id":"w8"}},"type":"agent_info"}}}}"#
    );
    fs::write(dir.join("agent.json"), json).unwrap();
    fs::write(dir.join("screen.txt"), screen).unwrap();
}

/// An idle agent resting at an empty prompt: ready for text.
fn agent_ready(dir: &Path) {
    agent_shows(dir, "idle", "╭────╮\n│ >  │\n╰────╯\n  ? for shortcuts\n");
}

/// The crate forbids `unsafe`, which rules out in-process `env::set_var`, so the parent
/// run re-executes the named test in a child process with the HERDR_* seam applied at
/// spawn — env applied to a child is safe, and the child alone runs the body. Returns the
/// fake herdr's directory in the child, and `None` in the parent once the child passed.
///
/// libtest exits 0 when `--exact` matches nothing, so the status alone cannot tell a passing
/// body from a filter that selected no test: the parent also requires `proof` in the fake
/// herdr's log, the evidence the body actually ran.
fn in_child(test: &str, proof: &str) -> Option<PathBuf> {
    if env::var("SEND_FLOW_CHILD").is_ok() {
        return Some(PathBuf::from(env::var("FAKE_HERDR_DIR").expect("set by the parent run")));
    }
    let staging = tempfile::TempDir::new().expect("tempdir");
    let script = write_fake_herdr(staging.path());
    let out = std::process::Command::new(env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("SEND_FLOW_CHILD", "1")
        .env("FAKE_HERDR_DIR", staging.path())
        .env("HERDR_BIN_PATH", &script)
        .env("HERDR_WORKSPACE_ID", "w8")
        .env("HERDR_PANE_ID", "w8:p9")
        .output()
        .expect("re-exec the test with the fake herdr env");
    assert!(
        out.status.success(),
        "child run failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        log(staging.path()).contains(proof),
        "the child never reached {proof:?} — did the test name and the `--exact` filter drift apart?\n{}",
        String::from_utf8_lossy(&out.stdout),
    );
    None
}

/// A repo with one added line, `beta` on line 2 of `a.rs`, and an app open on it.
fn one_added_line() -> (Repo, App) {
    let r = Repo::init();
    r.write("a.rs", "alpha\n");
    r.commit_all("init");
    r.write("a.rs", "alpha\nbeta\n");
    let app = app_on(&r);
    (r, app)
}

/// Save one comment on the first added line, so `Send` has something to deliver.
fn write_comment(app: &mut App, text: &str) {
    app.focus = Focus::Diff;
    app.diff_cursor = app.visible.iter().position(|r| r.marker() == '+').unwrap();
    app.start_comment();
    app.input = text.to_string();
    app.submit_comment();
}

fn press(app: &mut App, code: KeyCode, area: Rect, keymap: &Keymap) {
    handle_key(app, KeyEvent::from(code), area, keymap).unwrap();
}

#[test]
fn send_dispatches_one_agent_directly_and_several_through_the_picker() {
    let Some(fake_dir) = in_child(
        "send_dispatches_one_agent_directly_and_several_through_the_picker",
        "pane send-text",
    ) else {
        return;
    };
    let (_r, mut app) = one_added_line();
    agent_ready(&fake_dir);
    let keymap = Keymap::default();
    let area = Rect::new(0, 0, 80, 24);

    // Several agents: `s` opens the picker over both rows, labelled from `tab list`, and with
    // nothing sent yet the highlight arms on the first row.
    fs::write(fake_dir.join("agents.json"), TWO_AGENTS).unwrap();
    write_comment(&mut app, "one");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(app.mode, Mode::Picker, "several agents open the picker");
    assert_eq!(app.picker_rows.len(), 2);
    assert_eq!(app.picker_rows[0].tab, "Grip", "the tab label joins on tab_id");
    assert_eq!(app.picker_cursor, 0, "nothing sent this session arms the first row");

    // A chosen pane that closed while the picker was open fails the send, and every comment
    // stays. Nothing arms, since nothing was delivered.
    fail_on(&fake_dir, "pane send-text w8:p1");
    press(&mut app, KeyCode::Enter, area, &keymap);
    assert_eq!(app.mode, Mode::Normal, "the picker closes whatever the outcome");
    assert_eq!(app.store.len(), 1, "a failed send keeps every comment");
    // One short sentence a reviewer can read. herdr's own wording is a JSON envelope around a
    // pane id, and the argv it came from carries the whole review in its last argument — both
    // would fill a 40-column footer without naming anything.
    assert_eq!(app.status, "agent not found");
    assert_eq!(app.last_sent_pane, None, "a failed send arms nothing");
    fail_on_nothing(&fake_dir);

    // One agent: `s` sends straight through, no picker frame in between — and the direct
    // send arms its agent like a picker send.
    fs::write(fake_dir.join("agents.json"), ONE_AGENT).unwrap();
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(app.mode, Mode::Normal, "one agent sends directly");
    assert!(app.store.is_empty(), "a successful send consumes the whole set");
    assert_eq!(app.status, "added 1 comment to claude");
    assert_eq!(app.last_sent_pane.as_deref(), Some("w8:p1"));

    // `enter` sends to the digit-selected agent and consumes the set.
    fs::write(fake_dir.join("agents.json"), TWO_AGENTS).unwrap();
    write_comment(&mut app, "two");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    press(&mut app, KeyCode::Char('2'), area, &keymap);
    press(&mut app, KeyCode::Enter, area, &keymap);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.store.is_empty(), "a successful send consumes the whole set");
    assert_eq!(app.status, "added 1 comment to codex");
    assert_eq!(app.last_sent_pane.as_deref(), Some("w8:p2"));
    assert!(log(&fake_dir).contains("pane send-text w8:p2"), "log: {}", log(&fake_dir));
    // The start marker opens the payload at the CLI boundary; `pasted()` owns the rationale.
    assert!(
        log(&fake_dir).contains("pane send-text w8:p2 \u{1b}[200~"),
        "the send is framed as a bracketed paste: {}",
        log(&fake_dir)
    );
    // The batch's last bytes are the comment text "two", so this pins the terminator to the
    // end of a delivered payload.
    assert!(
        log(&fake_dir).contains("two\u{1b}[201~"),
        "the frame terminator closes the batch: {}",
        log(&fake_dir)
    );
    assert!(log(&fake_dir).contains("agent focus w8:p2"), "a send focuses its pane");
    // Every send asks first whether the chosen agent can take text.
    assert!(log(&fake_dir).contains("agent get w8:p2"), "log: {}", log(&fake_dir));
    assert!(log(&fake_dir).contains("pane read w8:p2 --source visible"));

    // Several again: the last-sent agent outranks the first row, and a first click on that
    // armed row sends immediately.
    write_comment(&mut app, "three");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(app.mode, Mode::Picker);
    assert_eq!(app.picker_cursor, 1, "the last-sent agent outranks the first row");
    let (col, row) = (0..area.height)
        .flat_map(|y| (0..area.width).map(move |x| (x, y)))
        .find(|&(x, y)| ui::hit_picker_row(area, &app, x, y) == Some(1))
        .expect("the armed row is clickable");
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        },
        area,
        &[],
        &keymap,
        &herdr_reviewr::export::Clipboard,
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Normal, "a first click on the armed row sends");
    assert!(app.store.is_empty());
    let sends = log(&fake_dir).matches("pane send-text w8:p2").count();
    assert_eq!(
        sends,
        2,
        "the digit-selected send and the armed-row click addressed the same pane: {}",
        log(&fake_dir)
    );

    // No agent, and an enumeration herdr never answered, both refuse and name the clipboard —
    // and neither opens a picker.
    fs::write(fake_dir.join("agents.json"), r#"{"result":{"agents":[]}}"#).unwrap();
    write_comment(&mut app, "four");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(app.mode, Mode::Normal, "an empty workspace opens no picker");
    assert_eq!(app.store.len(), 1, "a refusal keeps every comment");
    assert_eq!(app.status, "no agent here — copy to the clipboard instead");

    fail_on(&fake_dir, "agent list");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(app.mode, Mode::Normal, "a failed enumeration opens no picker");
    assert_eq!(app.store.len(), 1, "a refusal keeps every comment");
    // A failed enumeration says so rather than claiming a count. The argv and herdr's stderr go
    // to the log, so the sentence still fits a 40-column footer.
    assert_eq!(app.status, "herdr did not answer — copy to the clipboard instead");
}

/// How many times the fake herdr's log holds `needle`.
fn count(dir: &Path, needle: &str) -> usize {
    log(dir).matches(needle).count()
}

#[test]
fn a_send_waits_for_an_agent_that_is_blocked_or_showing_a_dialog() {
    let Some(fake_dir) =
        in_child("a_send_waits_for_an_agent_that_is_blocked_or_showing_a_dialog", "agent get")
    else {
        return;
    };
    let (_r, mut app) = one_added_line();
    let keymap = Keymap::default();
    let area = Rect::new(0, 0, 80, 24);
    fs::write(fake_dir.join("agents.json"), ONE_AGENT).unwrap();
    write_comment(&mut app, "one");

    // Waiting on a permission prompt: the batch would answer it, so nothing is written.
    agent_shows(&fake_dir, "blocked", "Do you want to proceed?\n❯ 1. Yes\n  2. No\n");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(app.status, "agent is waiting for a confirmation — comments kept");
    assert_eq!(app.store.len(), 1, "a refused send keeps every comment");
    assert_eq!(app.last_sent_pane, None, "a refused send arms nothing");
    assert_eq!(count(&fake_dir, "pane send-text"), 0, "log: {}", log(&fake_dir));

    // herdr says `idle`, but the screen shows a picker's key hints (Codex's model switch).
    agent_shows(
        &fake_dir,
        "idle",
        "Select model\n› gpt-5\n  o3\n\n  Enter to confirm · Esc to cancel\n",
    );
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(app.status, "agent has a dialog open — comments kept");
    assert_eq!(app.store.len(), 1);
    assert_eq!(count(&fake_dir, "pane send-text"), 0, "log: {}", log(&fake_dir));

    // A working agent takes typing into its input box: the spinner's `esc to interrupt` is
    // not a dialog.
    agent_shows(&fake_dir, "working", "✻ Thinking… (8s · esc to interrupt)\n╭──╮\n│ > │\n╰──╯\n");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(app.status, "added 1 comment to claude");
    assert!(app.store.is_empty());
    assert_eq!(count(&fake_dir, "pane send-text w8:p1"), 1);

    // A picker row is frozen when it opens, so the pick checks again before it sends.
    fs::write(fake_dir.join("agents.json"), TWO_AGENTS).unwrap();
    write_comment(&mut app, "two");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(app.mode, Mode::Picker);
    agent_shows(&fake_dir, "blocked", "Allow edit? (y/n)\n");
    press(&mut app, KeyCode::Enter, area, &keymap);
    assert_eq!(app.mode, Mode::Normal, "the picker closes whatever the outcome");
    assert_eq!(app.status, "agent is waiting for a confirmation — comments kept");
    assert_eq!(app.store.len(), 1);
    assert_eq!(count(&fake_dir, "pane send-text"), 1, "nothing more was written");
}

/// Switch the app to `deliver = "immediate"` through a real config file.
fn deliver_immediately(app: &mut App) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    fs::write(dir.path().join("config.toml"), "deliver = \"immediate\"\n").unwrap();
    app.set_plugin_config(herdr_reviewr::config::plugin_config_in(dir.path()).unwrap());
    dir
}

#[test]
fn immediate_delivery_quotes_each_saved_comment_into_the_agent_input() {
    let Some(fake_dir) =
        in_child("immediate_delivery_quotes_each_saved_comment_into_the_agent_input", "send-keys")
    else {
        return;
    };
    let (_r, mut app) = one_added_line();
    let _config = deliver_immediately(&mut app);
    let keymap = Keymap::default();
    let area = Rect::new(0, 0, 80, 24);
    agent_ready(&fake_dir);

    // One agent: a comment on a character span lands the moment it is saved, as a quote line
    // wrapped in newlines. No Enter, no focus.
    fs::write(fake_dir.join("agents.json"), ONE_AGENT).unwrap();
    let row = app.visible.iter().position(|r| r.marker() == '+').unwrap();
    let span = TextDrag {
        surface: Surface::Read,
        anchor: Point { row, chr: 0 },
        extent: Point { row, chr: 3 },
    };
    app.start_comment_with(Some(span));
    app.input = "why beta?".to_string();
    app.submit_comment();
    assert_eq!(app.status, "sent to claude");
    assert!(app.store.is_empty(), "a delivered comment leaves the store");
    assert_eq!(app.last_sent_pane.as_deref(), Some("w8:p1"));
    assert_eq!(app.mode, Mode::Normal);
    let sent = log(&fake_dir);
    let open = sent.find("pane send-keys w8:p1 ctrl+j").expect("a newline opens the quote");
    let body = sent
        .find("pane send-text w8:p1 \u{1b}[200~> a.rs:2「beta」\nwhy beta?\u{1b}[201~")
        .unwrap_or_else(|| panic!("the quote is one pasted line then the text: {sent}"));
    let close = sent.rfind("pane send-keys w8:p1 ctrl+j").unwrap();
    assert!(open < body && body < close, "newline, quote, newline: {sent}");
    assert!(!sent.contains("agent focus"), "the reviewer keeps reviewing: {sent}");
    assert!(!sent.contains("Enter") && !sent.contains("enter"), "nothing submits: {sent}");

    // Several agents with one used this session: it takes the next comment, no picker. A line
    // comment quotes its snippet.
    fs::write(fake_dir.join("agents.json"), TWO_AGENTS).unwrap();
    write_comment(&mut app, "line note");
    assert_eq!(app.mode, Mode::Normal, "the agent used last takes it without asking");
    assert!(app.store.is_empty());
    assert!(
        log(&fake_dir).contains("pane send-text w8:p1 \u{1b}[200~> a.rs:2\n> +beta\nline note"),
        "log: {}",
        log(&fake_dir)
    );

    // An agent that cannot take text keeps the comment, and nothing is written.
    let before = count(&fake_dir, "pane send-");
    agent_shows(&fake_dir, "blocked", "Allow edit? (y/n)\n");
    write_comment(&mut app, "kept");
    assert_eq!(app.status, "agent is waiting for a confirmation — comment kept");
    assert_eq!(app.store.len(), 1, "the comment stays for `s`");
    assert_eq!(count(&fake_dir, "pane send-"), before, "log: {}", log(&fake_dir));
    agent_ready(&fake_dir);

    // Editing a comment sends nothing: the agent may already hold it.
    app.start_edit();
    assert!(app.composing(), "the cursor sits on the kept comment");
    app.input = "kept, edited".to_string();
    app.submit_comment();
    assert_eq!(app.status, "comment updated");
    assert_eq!(app.store.len(), 1);
    assert_eq!(count(&fake_dir, "pane send-"), before, "an edit sends nothing");

    // Several agents and none used yet: the picker asks, and the pick sends only the comment
    // just saved — the kept one waits for `s`.
    app.last_sent_pane = None;
    write_comment(&mut app, "fresh");
    assert_eq!(app.mode, Mode::Picker, "a first delivery with several agents asks");
    assert_eq!(app.store.len(), 2);
    press(&mut app, KeyCode::Char('2'), area, &keymap);
    press(&mut app, KeyCode::Enter, area, &keymap);
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.status, "sent to codex");
    assert_eq!(app.last_sent_pane.as_deref(), Some("w8:p2"));
    let left: Vec<&str> = app.store.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(left, ["kept, edited"], "only the new comment went out");
    assert_eq!(count(&fake_dir, "pane send-text w8:p2"), 1);
    assert!(!log(&fake_dir).contains("kept"), "the kept comment was never written");
    assert!(log(&fake_dir).contains("fresh\u{1b}[201~"));

    // No agent at all: the comment stays and the line says so.
    fs::write(fake_dir.join("agents.json"), r#"{"result":{"agents":[]}}"#).unwrap();
    write_comment(&mut app, "nowhere");
    assert_eq!(app.status, "no agent here — comment kept");
    assert_eq!(app.store.len(), 2);
    assert!(!log(&fake_dir).contains("agent focus"), "immediate delivery never focuses");
}
