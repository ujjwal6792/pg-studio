//! Minimal terminal spinner: animated frames on stderr while a blocking
//! operation runs, so users can tell the program is alive and what it is
//! doing. Falls back to silence when stderr is not a terminal (pipes, CI).

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(80);

pub struct Spinner {
    msg: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    animated: bool,
}

impl Spinner {
    /// Starts animating `msg` on stderr.
    pub fn start(msg: impl Into<String>) -> Self {
        let animated = std::io::stderr().is_terminal();
        let spinner = Self {
            msg: Arc::new(Mutex::new(msg.into())),
            stop: Arc::new(AtomicBool::new(false)),
            handle: None,
            animated,
        };
        if !animated {
            return spinner;
        }
        let msg = spinner.msg.clone();
        let stop = spinner.stop.clone();
        let handle = std::thread::spawn(move || {
            let stderr = std::io::stderr();
            let mut frame = 0usize;
            while !stop.load(Ordering::Relaxed) {
                let text = msg.lock().map(|m| m.clone()).unwrap_or_default();
                {
                    let mut err = stderr.lock();
                    let _ = write!(err, "\r\x1b[2K{} {text}", FRAMES[frame % FRAMES.len()]);
                    let _ = err.flush();
                }
                frame += 1;
                std::thread::sleep(TICK);
            }
            // Erase the spinner line so the final message starts clean.
            let mut err = stderr.lock();
            let _ = write!(err, "\r\x1b[2K");
            let _ = err.flush();
        });
        Self {
            msg: spinner.msg.clone(),
            stop: spinner.stop.clone(),
            handle: Some(handle),
            animated: spinner.animated,
        }
    }

    /// Updates the text shown next to the spinner.
    pub fn set_message(&self, msg: impl Into<String>) {
        if let Ok(mut m) = self.msg.lock() {
            *m = msg.into();
        }
    }

    /// Stops the animation and erases the line; print the outcome yourself.
    pub fn finish(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_and_finish_without_terminal_is_silent() {
        let mut sp = Spinner::start("working...");
        sp.set_message("still working...");
        sp.finish();
    }
}
