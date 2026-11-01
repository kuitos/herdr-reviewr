//! The Windows console reader: VT input mode, so `ConPTY` passes bracketed pastes through.

use std::io::{self, Write};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crossterm_winapi::{Console, ConsoleMode, Handle, InputRecord};
use ratatui::crossterm::Command;
use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use windows_sys::Win32::System::Console::ENABLE_VIRTUAL_TERMINAL_INPUT;

use super::vt::{Console as VtConsole, Record, VtInput};
use crate::wake::{Wake, Woke};

/// The console and parser state; `None` while released, so an editor's leftovers never parse.
static READER: Mutex<Option<Reader>> = Mutex::new(None);

struct Reader {
    console: WinConsole,
    vt: VtInput,
}

/// The console input handle, waited on and read as [`Record`]s.
struct WinConsole {
    handle: Handle,
    console: Console,
}

/// The console for one wait that a [`Wake`] can end too.
struct Waking<'a> {
    console: &'a mut WinConsole,
    wake: &'a Wake,
}

/// VT input, plus SGR mouse reports and disambiguated keys, after crossterm's own claims.
pub(crate) fn claim() {
    set_vt_input(true);
    let mut sequence = String::new();
    let _ = EnableMouseCapture.write_ansi(&mut sequence);
    let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
    let _ = PushKeyboardEnhancementFlags(flags).write_ansi(&mut sequence);
    write_out(&sequence);
}

/// Undo [`claim`], before crossterm's mouse release restores the console mode it found.
pub(crate) fn release() {
    let mut sequence = String::new();
    let _ = PopKeyboardEnhancementFlags.write_ansi(&mut sequence);
    let _ = DisableMouseCapture.write_ansi(&mut sequence);
    write_out(&sequence);
    set_vt_input(false);
    *READER.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

/// Whether an event is ready within `timeout`, as `crossterm::event::poll` answers it.
pub(crate) fn poll(timeout: Duration) -> io::Result<bool> {
    let deadline = Some(Instant::now() + timeout);
    with_reader(|reader| Ok(reader.vt.poll(&mut reader.console, deadline)? == Woke::Input))
}

/// Sleep until an event is ready, `wake` is woken, or `timeout` (`None` waits for ever).
pub(crate) fn wait(wake: &Wake, timeout: Option<Duration>) -> io::Result<Woke> {
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    with_reader(|reader| {
        let mut waking = Waking { console: &mut reader.console, wake };
        reader.vt.poll(&mut waking, deadline)
    })
}

/// The next event, blocking until there is one, as `crossterm::event::read` answers it.
pub(crate) fn read() -> io::Result<Event> {
    with_reader(|reader| {
        loop {
            if let Some(event) = reader.vt.pop() {
                return Ok(event);
            }
            reader.vt.poll(&mut reader.console, None)?;
        }
    })
}

fn with_reader<T>(f: impl FnOnce(&mut Reader) -> io::Result<T>) -> io::Result<T> {
    let mut guard = READER.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.is_none() {
        let handle = Handle::current_in_handle()?;
        let console = Console::from(handle.clone());
        let console = WinConsole { handle, console };
        *guard = Some(Reader { console, vt: VtInput::default() });
    }
    f(guard.as_mut().expect("opened above"))
}

impl VtConsole for WinConsole {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Woke> {
        Ok(match crate::wake::sys::wait_any(&[self.raw()], timeout)? {
            Some(_) => Woke::Input,
            None => Woke::Deadline,
        })
    }

    fn read(&mut self) -> io::Result<Vec<Record>> {
        let records = self.console.read_console_input()?;
        Ok(records
            .into_iter()
            .filter_map(|record| match record {
                InputRecord::KeyEvent(key) => key_unit(key.key_down, key.u_char),
                InputRecord::WindowBufferSizeEvent(size) => Some(resized(size.size.x, size.size.y)),
                _ => None,
            })
            .collect())
    }
}

impl WinConsole {
    fn raw(&self) -> std::os::windows::io::RawHandle {
        (*self.handle).cast()
    }
}

impl VtConsole for Waking<'_> {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Woke> {
        let handles = [self.console.raw(), self.wake.sys().handle()];
        // The console comes first: the lowest signaled handle wins, so input reads before a wake.
        Ok(match crate::wake::sys::wait_any(&handles, timeout)? {
            Some(0) => Woke::Input,
            Some(_) => Woke::Woken,
            None => Woke::Deadline,
        })
    }

    fn read(&mut self) -> io::Result<Vec<Record>> {
        self.console.read()
    }
}

/// A key record's byte: only a press carries one, and a zero unit is a key sent as no byte.
fn key_unit(key_down: bool, u_char: u16) -> Option<Record> {
    (key_down && u_char != 0).then_some(Record::Unit(u_char))
}

/// The buffer size counts from zero, and crossterm adds one to match unix.
fn resized(x: i16, y: i16) -> Record {
    Record::Resize((i32::from(x) + 1) as u16, (i32::from(y) + 1) as u16)
}

fn set_vt_input(on: bool) {
    let Ok(handle) = Handle::current_in_handle() else { return };
    let mode = ConsoleMode::from(handle);
    if let Ok(before) = mode.mode() {
        let after = if on {
            before | ENABLE_VIRTUAL_TERMINAL_INPUT
        } else {
            before & !ENABLE_VIRTUAL_TERMINAL_INPUT
        };
        let set = mode.set_mode(after);
        crate::logln!("console input mode {before:#x} -> {after:#x} {set:?}");
    }
}

/// Write escape sequences crossterm would route to the console API on Windows.
fn write_out(sequence: &str) {
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(sequence.as_bytes());
    let _ = stdout.flush();
}

#[cfg(test)]
mod tests {
    use super::{Record, key_unit, resized};

    #[test]
    fn a_console_record_maps_to_its_byte_or_size() {
        assert_eq!(key_unit(true, u16::from(b'a')), Some(Record::Unit(u16::from(b'a'))));
        assert_eq!(key_unit(false, u16::from(b'a')), None, "a release would type it twice");
        assert_eq!(key_unit(true, 0), None, "a bare modifier sends no byte");
        assert_eq!(resized(79, 23), Record::Resize(80, 24));
    }
}
