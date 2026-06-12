//! Minimal terminal spinner with no external dependencies.
//!
//! A background thread redraws a Braille-frame animation on stderr every
//! `TICK`. When stderr is not a terminal (pipes, CI logs) no thread is
//! spawned and no animation is drawn: `finish_with_message` and `println`
//! degrade to plain line output, the rest become no-ops.

use crate::utils::style::Stylize;
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const TICK: Duration = Duration::from_millis(80);
/// `\r` returns the cursor to column 0, `ESC[2K` erases the whole line.
const CLEAR_LINE: &str = "\r\x1b[2K";

/// Animated terminal spinner with an updatable message.
///
/// Cloning yields another handle to the same spinner, so it can be
/// shared with reader threads.
/// The animation stops on any `finish_*` call; if the last handle is
/// dropped without finishing, the line is cleared and the ticker thread
/// stopped, so no garbage is left on the terminal.
pub struct Spinner {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<State>,
    /// Wakes the ticker thread early when the spinner is finished.
    ticker_wakeup: Condvar,
    /// Ticker thread handle, joined on finish for deterministic cleanup.
    ticker: Mutex<Option<JoinHandle<()>>>,
    /// Number of live `Spinner` handles (the ticker thread is not counted).
    handles: AtomicUsize,
}

struct State {
    message: String,
    frame: usize,
    finished: bool,
    /// Whether to animate (true only when the output is a terminal).
    animated: bool,
    out: Box<dyn Write + Send>,
}

/// What to leave on screen when the spinner stops.
enum Finish {
    /// Erase the line, print nothing.
    Clear,
    /// Erase the line, print a final message.
    Message(String),
    /// Keep the current frame + message visible (error context).
    Abandon,
}

impl Spinner {
    /// Start a spinner with the given message.
    pub fn new(message: &str) -> Self {
        let animated = io::stderr().is_terminal();
        Self::start(message, animated, Box::new(io::stderr()))
    }

    /// Start a spinner with explicit animation mode and output sink.
    /// Split out from `new` so tests can run without a real terminal.
    fn start(message: &str, animated: bool, out: Box<dyn Write + Send>) -> Self {
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                message: message.to_string(),
                frame: 0,
                finished: false,
                animated,
                out,
            }),
            ticker_wakeup: Condvar::new(),
            ticker: Mutex::new(None),
            handles: AtomicUsize::new(1),
        });
        if animated {
            let thread_inner = Arc::clone(&inner);
            let handle = std::thread::spawn(move || tick_loop(&thread_inner));
            *inner.ticker.lock().unwrap() = Some(handle);
        }
        Self { inner }
    }

    /// Replace the message; the ticker picks it up on the next frame.
    pub fn set_message(&self, message: impl Into<String>) {
        self.lock_state().message = message.into();
    }

    /// Print a line "above" the spinner: the animation line is erased, the
    /// text is printed, and the spinner is redrawn underneath. The line is
    /// printed even without a terminal, so piped output (e.g. `docker
    /// compose` logs in CI) is never lost.
    pub fn println(&self, line: impl AsRef<str>) {
        let mut st = self.lock_state();
        let active = st.animated && !st.finished;
        if active {
            let _ = write!(st.out, "{CLEAR_LINE}");
        }
        let _ = writeln!(st.out, "{}", line.as_ref());
        if active {
            let frame_line = render_line(&st);
            let _ = write!(st.out, "{frame_line}");
        }
        let _ = st.out.flush();
    }

    /// Stop the animation, erase the line, and print the final message.
    pub fn finish_with_message(&self, message: impl Into<String>) {
        self.finish(Finish::Message(message.into()));
    }

    /// Stop the animation and erase the line, leaving no output behind.
    pub fn finish_and_clear(&self) {
        self.finish(Finish::Clear);
    }

    /// Stop the animation, keeping the current frame and message visible.
    /// Used on errors so the in-progress line stays as context.
    pub fn abandon(&self) {
        self.finish(Finish::Abandon);
    }

    fn finish(&self, mode: Finish) {
        {
            let mut st = self.lock_state();
            if st.finished {
                return;
            }
            st.finished = true;
            if st.animated {
                let _ = write!(st.out, "{CLEAR_LINE}");
            }
            match mode {
                Finish::Clear => {}
                // Printed even in non-animated mode: callers rely on the
                // final message reaching the log (pipes, CI).
                Finish::Message(msg) => {
                    let _ = writeln!(st.out, "{msg}");
                }
                Finish::Abandon => {
                    if st.animated {
                        let frame_line = render_line(&st);
                        let _ = writeln!(st.out, "{frame_line}");
                    }
                }
            }
            let _ = st.out.flush();
            self.inner.ticker_wakeup.notify_all();
        }
        // Join outside the state lock — the ticker needs it to see `finished`.
        if let Some(handle) = self.inner.ticker.lock().unwrap().take() {
            let _ = handle.join();
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock would only poison terminal drawing
        // state, so recovering from poisoning is safe here.
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Clone for Spinner {
    fn clone(&self) -> Self {
        self.inner.handles.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Drop for Spinner {
    /// Last-handle guard: a spinner dropped without `finish_*` must not
    /// leave a stale animation line (or a running thread) behind.
    fn drop(&mut self) {
        if self.inner.handles.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.finish(Finish::Clear);
        }
    }
}

/// Background loop: redraw the current frame every `TICK` until finished.
fn tick_loop(inner: &Inner) {
    let mut st = inner
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    while !st.finished {
        let line = render_line(&st);
        let _ = write!(st.out, "{CLEAR_LINE}{line}");
        let _ = st.out.flush();
        st.frame = (st.frame + 1) % FRAMES.len();
        // Sleep on the condvar so `finish` can wake us immediately.
        st = inner
            .ticker_wakeup
            .wait_timeout(st, TICK)
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .0;
    }
}

/// Render the "frame message" line (no clearing, no newline). The green
/// frame respects the global `style` override set in `Output::new`.
fn render_line(st: &State) -> String {
    format!("{} {}", FRAMES[st.frame].to_string().green(), st.message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Write` sink that captures spinner output for assertions.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        fn contents(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn finish_stops_ticker_and_prints_final_message() {
        let cap = Capture::default();
        let sp = Spinner::start("working", true, Box::new(cap.clone()));
        std::thread::sleep(TICK * 2);
        sp.finish_with_message("done");
        // `finish` joins the ticker thread, so no frames may appear afterwards.
        let after_finish = cap.contents();
        std::thread::sleep(TICK * 3);
        assert_eq!(cap.contents(), after_finish);
        assert!(after_finish.contains("working"));
        assert!(after_finish.ends_with("done\n"));
    }

    #[test]
    fn set_message_is_picked_up_by_next_frame() {
        let cap = Capture::default();
        let sp = Spinner::start("first", true, Box::new(cap.clone()));
        sp.set_message("second");
        std::thread::sleep(TICK * 3);
        sp.finish_and_clear();
        let out = cap.contents();
        assert!(out.contains("second"));
        assert!(out.ends_with(CLEAR_LINE));
    }

    #[test]
    fn non_terminal_mode_prints_plain_lines_only() {
        let cap = Capture::default();
        let sp = Spinner::start("quiet", false, Box::new(cap.clone()));
        sp.set_message("still quiet");
        sp.println("a log line");
        sp.finish_with_message("all done");
        // No frames, no ANSI escapes — only the plain lines.
        assert_eq!(cap.contents(), "a log line\nall done\n");
    }

    #[test]
    fn dropping_last_handle_clears_the_line() {
        let cap = Capture::default();
        let sp = Spinner::start("temp", true, Box::new(cap.clone()));
        let clone = sp.clone();
        drop(sp); // one handle remains — the spinner must keep running
        std::thread::sleep(TICK * 2);
        assert!(cap.contents().contains("temp"));
        drop(clone); // last handle gone — line must be cleared
        assert!(cap.contents().ends_with(CLEAR_LINE));
    }
}
