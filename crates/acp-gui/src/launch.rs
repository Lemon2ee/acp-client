//! What the app lacks when Finder opens it rather than a terminal. LaunchServices starts it
//! in `/` with launchd's PATH (`/usr/bin:/bin:/usr/sbin:/sbin`), so the adapters' `npx` from
//! Homebrew or nvm isn't found and new sessions would work in `/`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Long enough for a slow `.zshrc` (nvm alone can take a second), short enough that a shell
/// stuck on a prompt doesn't hold the window back for long.
const SHELL_TIMEOUT: Duration = Duration::from_secs(5);

/// Brackets PATH in the shell's output, as interactive startup files may print around it.
const MARKER: &str = "__acp_client_path__";

/// The folder new sessions start in: the current one from a terminal, the home folder when
/// Finder opened the app, which then also takes PATH from the user's login shell.
///
/// Call it before any other thread starts, as it sets PATH for the whole process.
pub(crate) fn session_folder() -> PathBuf {
    let cwd = std::env::current_dir().expect("no current directory");
    if cwd != Path::new("/") {
        return cwd;
    }
    if let Some(path) = login_shell_path() {
        // SAFETY: no other thread is running yet to read the environment.
        unsafe { std::env::set_var("PATH", path) };
    }
    dirs::home_dir().unwrap_or(cwd)
}

/// PATH as an interactive login shell sets it: nvm and the like add to it from `.zshrc`,
/// which only interactive shells read. `printenv` rather than `$PATH` so fish, whose
/// `"$PATH"` is space-separated, gives the same colon-separated list.
fn login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let script =
        format!("/usr/bin/printf {MARKER}; /usr/bin/printenv PATH; /usr/bin/printf {MARKER}");
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read up to the second marker on a thread rather than to the end: a job the startup
    // files leave in the background keeps the pipe open after the shell exits.
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        let mut chunk = [0; 4096];
        while marker_count(&output) < 2 {
            match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => output.extend_from_slice(&chunk[..read]),
            }
        }
        let _ = sender.send(output);
    });
    let output = receiver.recv_timeout(SHELL_TIMEOUT);
    let _ = child.kill();
    let _ = child.wait();
    // On time the reader has finished; joined so no other thread runs when PATH is set.
    // Late, it may stay blocked on the pipe, and PATH is left alone.
    let output = output.ok()?;
    reader.join().ok()?;
    let output = String::from_utf8_lossy(&output);
    // Both markers, so a shell that died halfway gives nothing rather than a partial PATH.
    let mut parts = output.split(MARKER);
    let path = parts.nth(1)?.trim();
    parts.next()?;
    (!path.is_empty()).then(|| path.to_string())
}

fn marker_count(output: &[u8]) -> usize {
    output
        .windows(MARKER.len())
        .filter(|window| *window == MARKER.as_bytes())
        .count()
}
