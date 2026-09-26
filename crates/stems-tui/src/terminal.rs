//! Terminal hygiene: raw mode + alternate screen on entry, restored on
//! normal exit (guard drop), on panic (a panic hook wrapping the previous
//! one) and on SIGTERM/SIGHUP (the runner turns those into an exit, which
//! drops the guard).

use std::io::{self, Write};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{execute, queue};

/// The sequence leaving the alternate screen (asserted by the panic test).
pub const LEAVE_ALT_SCREEN: &str = "\x1b[?1049l";

static ACTIVE: AtomicBool = AtomicBool::new(false);
static MOUSE: AtomicBool = AtomicBool::new(false);
static RAW: AtomicBool = AtomicBool::new(false);
static HOOK: Once = Once::new();

/// Write the restore sequence: mouse capture off (if it was on), leave the
/// alternate screen, show the cursor.
pub fn write_restore<W: Write>(w: &mut W, mouse: bool) -> io::Result<()> {
    if mouse {
        queue!(w, DisableMouseCapture)?;
    }
    queue!(w, LeaveAlternateScreen, Show)?;
    w.flush()
}

/// Restore the terminal if a guard is active (idempotent). Used by the
/// guard's drop and the panic hook.
pub fn restore_now<W: Write>(w: &mut W) {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        if RAW.swap(false, Ordering::SeqCst) {
            let _ = disable_raw_mode();
        }
        let _ = write_restore(w, MOUSE.load(Ordering::SeqCst));
    }
}

/// Install (once) a panic hook that restores the terminal before the
/// previous hook prints the panic.
pub fn install_panic_hook() {
    HOOK.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_now(&mut io::stdout());
            prev(info);
        }));
    });
}

/// Raw mode + alternate screen while alive.
#[derive(Debug)]
pub struct TerminalGuard {
    _priv: (),
}

impl TerminalGuard {
    /// Enter the TUI screen. With `interactive` false (stdin or stdout is
    /// not a terminal, `STEMS_TUI_FORCE=1`) raw mode is not touched: a
    /// background process group changing terminal modes would be stopped
    /// by `SIGTTOU`. The escape sequences are written regardless, so
    /// restoring stays observable.
    pub fn enter(mouse: bool, interactive: bool) -> io::Result<Self> {
        install_panic_hook();
        MOUSE.store(mouse, Ordering::SeqCst);
        RAW.store(interactive, Ordering::SeqCst);
        ACTIVE.store(true, Ordering::SeqCst);
        if interactive {
            let _ = enable_raw_mode();
        }
        let mut out = io::stdout();
        execute!(out, EnterAlternateScreen, Hide)?;
        if mouse {
            execute!(out, EnableMouseCapture)?;
        }
        Ok(Self { _priv: () })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_now(&mut io::stdout());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn restore_sequence_leaves_the_alternate_screen() {
        let mut buf = Vec::new();
        write_restore(&mut buf, true).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains(LEAVE_ALT_SCREEN), "{s:?}");
        assert!(s.contains("\x1b[?25h"), "cursor shown: {s:?}");
        assert!(s.contains("\x1b[?1000l"), "mouse off: {s:?}");
    }

    #[test]
    fn restore_now_only_when_active_and_once() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut buf = Vec::new();
        ACTIVE.store(false, Ordering::SeqCst);
        restore_now(&mut buf);
        assert!(buf.is_empty());
        ACTIVE.store(true, Ordering::SeqCst);
        restore_now(&mut buf);
        assert!(String::from_utf8_lossy(&buf).contains(LEAVE_ALT_SCREEN));
        let n = buf.len();
        restore_now(&mut buf);
        assert_eq!(buf.len(), n, "idempotent");
    }

    #[test]
    fn panic_hook_restores_before_the_previous_hook() {
        // The hook writes to the real stdout; here we check it runs and
        // clears the active flag (the e2e panic-restore scenario checks the
        // bytes on a real process's stdout).
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        install_panic_hook();
        ACTIVE.store(true, Ordering::SeqCst);
        let r = std::panic::catch_unwind(|| panic!("boom"));
        assert!(r.is_err());
        assert!(!ACTIVE.load(Ordering::SeqCst));
    }
}
