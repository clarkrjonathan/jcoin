//! Log output for the library.
//!
//! `log_info!` and `log_error!` print as `println!` and `eprintln!` do, and
//! also pass the line to a sink if the embedding program set one. The desktop
//! app sets a sink to write a log file and show the log in its window; the
//! command-line tools set none and only print.
//!
//! A line may carry structured fields for the sink to file under
//! (`log_info!([("peer", id), ("height", h.to_string())]; "...")`). The fields
//! are for reporting only: they do not change the printed or stored text. The
//! app turns them into Sentry log attributes; the command-line tools ignore
//! them.

use std::sync::OnceLock;

/// How serious a log line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Error,
}

/// A structured field on a log line: a fixed key and its value. The value is
/// owned so a sink may keep it past the call.
pub type Field = (&'static str, String);

type Sink = Box<dyn Fn(Level, &str, &[Field]) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

/// Send every later log line to `sink` as well. Only the first call has an
/// effect. The sink runs on the logging thread, so it must be quick.
pub fn set_sink(sink: impl Fn(Level, &str, &[Field]) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
}

/// Print `line` and pass it, with any `fields`, to the sink. Use the macros
/// instead of calling this.
pub fn emit(level: Level, line: &str, fields: &[Field]) {
    match level {
        Level::Info => println!("{line}"),
        Level::Error => eprintln!("{line}"),
    }
    if let Some(sink) = SINK.get() {
        sink(level, line, fields);
    }
}

/// Log a line of normal progress, optionally with structured fields:
/// `log_info!([("peer", id)]; "connected to {id}")`.
#[macro_export]
macro_rules! log_info {
    ([$(($k:expr, $v:expr)),* $(,)?]; $($arg:tt)*) => {
        $crate::log::emit($crate::log::Level::Info, &format!($($arg)*), &[$(($k, $v)),*])
    };
    ($($arg:tt)*) => {
        $crate::log::emit($crate::log::Level::Info, &format!($($arg)*), &[])
    };
}

/// Log a failure, optionally with structured fields:
/// `log_error!([("peer", id), ("reason", r)]; "dial to {id} failed")`.
#[macro_export]
macro_rules! log_error {
    ([$(($k:expr, $v:expr)),* $(,)?]; $($arg:tt)*) => {
        $crate::log::emit($crate::log::Level::Error, &format!($($arg)*), &[$(($k, $v)),*])
    };
    ($($arg:tt)*) => {
        $crate::log::emit($crate::log::Level::Error, &format!($($arg)*), &[])
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // The sink is global and set once, so one test owns it. Lines are filtered
    // by a marker so unrelated lines from other tests are ignored.
    static CAPTURED: Mutex<Vec<(String, Vec<Field>)>> = Mutex::new(Vec::new());

    #[test]
    fn macros_carry_fields_to_the_sink() {
        set_sink(|_level, line, fields| {
            if line.contains("ZZMARKER") {
                CAPTURED.lock().unwrap().push((line.to_string(), fields.to_vec()));
            }
        });
        crate::log_info!("ZZMARKER plain");
        crate::log_error!(
            [("peer", "abc123".to_string()), ("reason", "timeout".to_string())];
            "ZZMARKER with {} fields", 2
        );

        let cap = CAPTURED.lock().unwrap();
        let plain = cap.iter().find(|(l, _)| l.contains("plain")).expect("plain line");
        assert!(plain.1.is_empty(), "a plain line carries no fields");
        let fielded = cap.iter().find(|(l, _)| l.contains("with 2 fields")).expect("fielded line");
        assert_eq!(
            fielded.1,
            vec![("peer", "abc123".to_string()), ("reason", "timeout".to_string())]
        );
    }
}
