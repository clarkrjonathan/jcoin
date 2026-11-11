//! Log output for the library.
//!
//! `log_info!` and `log_error!` print as `println!` and `eprintln!` do, and
//! also pass the line to a sink if the embedding program set one. The desktop
//! app sets a sink to write a log file and show the log in its window; the
//! command-line tools set none and only print.

use std::sync::OnceLock;

/// How serious a log line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Error,
}

type Sink = Box<dyn Fn(Level, &str) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

/// Send every later log line to `sink` as well. Only the first call has an
/// effect. The sink runs on the logging thread, so it must be quick.
pub fn set_sink(sink: impl Fn(Level, &str) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
}

/// Print `line` and pass it to the sink. Use the macros instead of calling this.
pub fn emit(level: Level, line: &str) {
    match level {
        Level::Info => println!("{line}"),
        Level::Error => eprintln!("{line}"),
    }
    if let Some(sink) = SINK.get() {
        sink(level, line);
    }
}

/// Log a line of normal progress.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::log::emit($crate::log::Level::Info, &format!($($arg)*))
    };
}

/// Log a failure.
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::log::emit($crate::log::Level::Error, &format!($($arg)*))
    };
}
