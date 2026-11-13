//! Start the app again after a crash while it was mining.
//!
//! A panic on the main thread (the window) ends the program. If the app was
//! mining, the panic hook starts a new copy before the old one goes down, and
//! the new copy starts mining again on the same GPU. The new copy first waits
//! for the old one to exit, so two copies never use the data folder at once:
//! it reads its standard input, a pipe whose other end only the old copy
//! holds, until the pipe closes. A copy that was itself started this way does
//! not restart again within `QUIET`, so a crash at startup cannot loop.

use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The command-line argument that marks a copy started after a crash.
const AFTER_CRASH: &str = "--after-crash";
/// The GPU the crashed copy mined on: an adapter index or `auto`.
const ADAPTER_ENV: &str = "JCOIN_APP_RESUME_ADAPTER";
/// When the copy before this one crashed, in seconds since 1970.
const CRASHED_AT_ENV: &str = "JCOIN_APP_CRASHED_AT";
/// No second restart within this time of the last one.
const QUIET: Duration = Duration::from_secs(600);
/// The longest the new copy waits for the old one to exit.
const WAIT_AT_MOST: Duration = Duration::from_secs(60);

/// `Some(adapter)` while mining (`adapter` as `Backend::start_mining` takes
/// it), `None` otherwise.
static MINING: Mutex<Option<Option<usize>>> = Mutex::new(None);

/// Record whether the app mines, and on which GPU. Called by the backend
/// when mining starts and stops.
pub fn set_mining(adapter: Option<Option<usize>>) {
    if let Ok(mut m) = MINING.lock() {
        *m = adapter;
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Whether a crash at `now` should start a new copy: only when the copy
/// before was not itself started by a crash less than `QUIET` ago.
fn may_restart(previous_crash: Option<u64>, now: u64) -> bool {
    previous_crash.is_none_or(|t| now.saturating_sub(t) >= QUIET.as_secs())
}

/// Add the restart to the panic hook. Call after `telemetry::init`, so the
/// crash is reported first. Only a release build restarts.
pub fn install_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        if cfg!(debug_assertions) || std::thread::current().name() != Some("main") {
            return;
        }
        let Some(adapter) = MINING.lock().ok().and_then(|m| *m) else {
            return;
        };
        let crashed_before = std::env::var(CRASHED_AT_ENV).ok().and_then(|v| v.parse().ok());
        if !may_restart(crashed_before, now()) {
            jcoin::log_error!(
                [("event", "crash_restart_skipped".to_string())];
                "[app] crashed while mining again within {} minutes; not starting again", QUIET.as_secs() / 60
            );
            return;
        }
        match start_new_copy(adapter) {
            Ok(()) => jcoin::log_info!(
                [("event", "crash_restart".to_string())];
                "[app] crashed while mining; a new copy starts when this one exits"
            ),
            Err(e) => jcoin::log_error!(
                [("event", "crash_restart_failed".to_string()), ("reason", e.clone())];
                "[app] crashed while mining; cannot start a new copy: {e}"
            ),
        }
        crate::telemetry::flush();
    }));
}

fn start_new_copy(adapter: Option<usize>) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let child = std::process::Command::new(exe)
        .arg(AFTER_CRASH)
        .env(ADAPTER_ENV, adapter.map_or("auto".to_string(), |i| i.to_string()))
        .env(CRASHED_AT_ENV, now().to_string())
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    // Keep the pipe's write end open until this process exits; its closing
    // is the new copy's signal to go on.
    std::mem::forget(child);
    Ok(())
}

/// In a copy started after a crash: wait for the old copy to exit, then
/// return the GPU to mine on. `None` in a copy started normally.
pub fn after_crash() -> Option<Option<usize>> {
    if !std::env::args().any(|a| a == AFTER_CRASH) {
        return None;
    }
    let (done, waited) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut sink);
        let _ = done.send(());
    });
    let _ = waited.recv_timeout(WAIT_AT_MOST);
    let adapter = std::env::var(ADAPTER_ENV).ok()?;
    Some(adapter.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crash_restarts_unless_the_last_restart_was_recent() {
        assert!(may_restart(None, 1_000));
        assert!(!may_restart(Some(1_000), 1_000 + 599));
        assert!(may_restart(Some(1_000), 1_000 + 600));
    }
}
