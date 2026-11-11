//! The app's log: every line the node, the miner, and the app report goes to a
//! file in the data folder and to a short in-memory tail shown in the window.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Name of the log file in the data folder. The previous run's oversized log
/// is kept once as `jeffycoin.log.old`.
const LOG_FILE: &str = "jeffycoin.log";
/// Start a new file once the log passes this size (checked at startup).
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
/// Lines kept in memory for the window.
const TAIL_LINES: usize = 500;

#[derive(Clone)]
pub struct LogLine {
    /// Unix seconds.
    pub time: u64,
    pub error: bool,
    pub text: String,
}

pub struct AppLog {
    pub path: PathBuf,
    file: Mutex<Option<File>>,
    tail: Mutex<VecDeque<LogLine>>,
}

impl AppLog {
    /// Open the log in `data_dir`. If the file cannot be opened the log still
    /// works in memory.
    pub fn open(data_dir: &Path) -> Arc<AppLog> {
        let path = data_dir.join(LOG_FILE);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
            let _ = std::fs::rename(&path, data_dir.join(format!("{LOG_FILE}.old")));
        }
        let file = OpenOptions::new().create(true).append(true).open(&path).ok();
        Arc::new(AppLog {
            path,
            file: Mutex::new(file),
            tail: Mutex::new(VecDeque::new()),
        })
    }

    pub fn push(&self, error: bool, text: &str) {
        let time = now();
        if let Ok(mut file) = self.file.lock() {
            if let Some(file) = file.as_mut() {
                let level = if error { "ERROR" } else { "info " };
                let _ = writeln!(file, "{} {level} {text}", utc(time));
            }
        }
        if let Ok(mut tail) = self.tail.lock() {
            if tail.len() == TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(LogLine {
                time,
                error,
                text: text.to_string(),
            });
        }
        // Also report the line, filed by component and event.
        crate::telemetry::log(error, text);
    }

    /// The most recent lines, oldest first.
    pub fn tail(&self) -> Vec<LogLine> {
        self.tail.lock().map(|t| t.iter().cloned().collect()).unwrap_or_default()
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `time` (Unix seconds) as `YYYY-MM-DD HH:MM:SS` in UTC.
pub fn utc(time: u64) -> String {
    let (days, secs) = (time / 86_400, time % 86_400);
    // Civil date from a day count (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// The time-of-day part of `utc`.
pub fn utc_clock(time: u64) -> String {
    utc(time)[11..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_formats_known_instants() {
        assert_eq!(utc(0), "1970-01-01 00:00:00");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(utc(1_794_358_345), "2026-11-11 00:52:25");
        assert_eq!(utc_clock(1_794_358_345), "00:52:25");
    }

    #[test]
    fn lines_reach_the_file_and_the_tail() {
        let dir = std::env::temp_dir().join(format!("jcoin-applog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = AppLog::open(&dir);
        log.push(false, "started");
        log.push(true, "it broke");
        let tail = log.tail();
        assert_eq!(tail.len(), 2);
        assert!(tail[1].error && tail[1].text == "it broke");
        let text = std::fs::read_to_string(&log.path).unwrap();
        assert!(text.contains("info  started") && text.contains("ERROR it broke"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
