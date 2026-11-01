//! The event loop's input: crossterm's own on unix, the console in VT mode on Windows.

#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod vt;
#[cfg(windows)]
mod windows;

use std::io;

use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
#[cfg(not(windows))]
use ratatui::crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;

#[cfg(not(windows))]
pub(crate) use ratatui::crossterm::event::{poll, read};
#[cfg(windows)]
pub(crate) use windows::{poll, read, wait};

/// Sleep until the terminal has input, `wake` is woken, or `timeout` (`None` waits for ever).
/// crossterm may hold input it already read, so a caller asks [`poll`] with no wait first.
#[cfg(not(windows))]
pub(crate) fn wait(
    wake: &crate::wake::Wake,
    timeout: Option<std::time::Duration>,
) -> io::Result<crate::wake::Woke> {
    crate::wake::sys::wait(&[terminal_input()?], wake.sys(), timeout)
}

/// The descriptor crossterm reads events from: standard input when it is a terminal, else
/// `/dev/tty`, opened once.
#[cfg(not(windows))]
fn terminal_input() -> io::Result<std::os::fd::BorrowedFd<'static>> {
    use std::os::fd::{AsFd, OwnedFd};
    static TTY: std::sync::OnceLock<OwnedFd> = std::sync::OnceLock::new();
    if let Some(fd) = TTY.get() {
        return Ok(fd.as_fd());
    }
    let stdin = io::stdin();
    let fd: OwnedFd = if rustix::termios::isatty(stdin.as_fd()) {
        stdin.as_fd().try_clone_to_owned()?
    } else {
        std::fs::File::open("/dev/tty")?.into()
    };
    Ok(TTY.get_or_init(|| fd).as_fd())
}

/// Whether the terminal speaks the kitty protocol, asked once; it reports Ctrl/Alt+arrows.
#[cfg(not(windows))]
fn kitty() -> bool {
    static KITTY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *KITTY.get_or_init(|| {
        let kitty = ratatui::crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
        crate::logln!("keyboard enhancement supported={kitty}");
        kitty
    })
}

/// Mouse capture, bracketed paste, and the kitty protocol where the terminal has it.
pub(crate) fn claim() {
    let _ = execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    #[cfg(not(windows))]
    if kitty() {
        let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
        let _ = execute!(io::stdout(), PushKeyboardEnhancementFlags(flags));
    }
    // Last, since the mouse capture above rewrites the console mode it builds on.
    #[cfg(windows)]
    windows::claim();
}

/// Release what [`claim`] claimed, in reverse.
pub(crate) fn release() {
    #[cfg(windows)]
    windows::release();
    #[cfg(not(windows))]
    if kitty() {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
}
