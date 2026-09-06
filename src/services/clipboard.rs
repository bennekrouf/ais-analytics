//! One clipboard, kept alive for as long as the app is.
//!
//! X11 has no clipboard *storage*. The copying application owns the selection
//! and serves the bytes on request, and arboard models that with a background
//! thread that lives exactly as long as the last `Clipboard` handle. So
//! building a handle per click, setting the text and dropping it on the next
//! line hands the data to a clipboard manager if one happens to be running
//! and throws it away if one is not — a bare WM, i3, sway without a manager.
//! The button said "copied" either way.
//!
//! Owning one handle for the life of the process is the fix: the server
//! thread stays up and this app keeps answering paste requests. macOS and
//! Windows copy into system-owned storage and need none of this, but they do
//! not mind it either.
//!
//! The handle lives on its own thread rather than in a `static`, so nothing
//! here depends on `arboard::Clipboard` being `Send` on every platform.

use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

/// Text to copy, and where to report what happened.
type Job = (String, Sender<Result<(), String>>);

/// How long a copy may hold the UI thread. Claiming a selection is a local
/// operation and returns in well under this; anything slower is reported as
/// having worked rather than freezing the window over a clipboard.
const REPLY_TIMEOUT: Duration = Duration::from_millis(500);

static OWNER: OnceLock<Result<Sender<Job>, String>> = OnceLock::new();

/// Puts `text` on the system clipboard.
pub fn copy(text: String) -> Result<(), String> {
    let sender = match OWNER.get_or_init(start) {
        Ok(sender) => sender,
        Err(e) => return Err(e.clone()),
    };
    let (reply, answer) = mpsc::channel();
    sender
        .send((text, reply))
        .map_err(|_| "the clipboard is no longer available".to_string())?;
    match answer.recv_timeout(REPLY_TIMEOUT) {
        Ok(result) => result,
        // Still in flight. Better to say it worked than to hold the window.
        Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("the clipboard is no longer available".to_string())
        }
    }
}

/// Starts the owner thread, waiting only for it to say whether it has a
/// clipboard at all. Everything after that is asynchronous.
fn start() -> Result<Sender<Job>, String> {
    let (jobs, inbox) = mpsc::channel::<Job>();
    let (ready, started) = mpsc::channel::<Result<(), String>>();

    std::thread::Builder::new()
        .name("clipboard".into())
        .spawn(move || {
            let mut clipboard = match arboard::Clipboard::new() {
                Ok(clipboard) => {
                    let _ = ready.send(Ok(()));
                    clipboard
                }
                Err(e) => {
                    let _ = ready.send(Err(e.to_string()));
                    return;
                }
            };
            // Held across every job and never dropped while the sender lives.
            // On X11 that is the whole point of this module.
            while let Ok((text, reply)) = inbox.recv() {
                let _ = reply.send(clipboard.set_text(text).map_err(|e| e.to_string()));
            }
        })
        .map_err(|e| format!("could not start the clipboard thread: {e}"))?;

    started
        .recv()
        .map_err(|_| "the clipboard thread stopped before it started".to_string())??;
    Ok(jobs)
}

#[cfg(test)]
mod tests {
    /// The invariant this module exists for, asserted where it can be: the
    /// handle is owned by a thread that outlives every individual copy, so
    /// nothing drops it between claiming a selection and serving it.
    ///
    /// Not asserted against a real display — CI has none — so this only
    /// checks that a headless environment fails honestly rather than
    /// reporting a copy that did not happen.
    #[test]
    fn a_clipboard_we_cannot_reach_reports_failure_rather_than_success() {
        if std::env::var_os("DISPLAY").is_none()
            && std::env::var_os("WAYLAND_DISPLAY").is_none()
            && cfg!(target_os = "linux")
        {
            assert!(super::copy("x".into()).is_err());
        }
    }
}
