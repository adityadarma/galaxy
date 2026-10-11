//! File logging for the core.
//!
//! Logs go to `<dir>/core.log` with daily rotation. The UI passes
//! `~/Library/Logs/Galaxy`. Initialization is idempotent.

use std::backtrace::Backtrace;
use std::panic;
use std::path::Path;
use std::sync::OnceLock;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

static GUARD: OnceLock<WorkerGuard> = OnceLock::new();
static CRASH_HOOK: OnceLock<()> = OnceLock::new();

/// Initializes logging. Returns `false` if logging was already set up.
///
/// `filter` is a level name: `error`, `warn`, `info`, `debug`, or `trace`.
pub fn init(dir: &Path, filter: &str) -> std::io::Result<bool> {
    if GUARD.get().is_some() {
        return Ok(false);
    }
    std::fs::create_dir_all(dir)?;

    let level = parse_level(filter);
    let appender = tracing_appender::rolling::daily(dir, "core.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let layer = fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_thread_names(true)
        .with_filter(tracing_subscriber::filter::LevelFilter::from_level(level));

    if tracing_subscriber::registry()
        .with(layer)
        .try_init()
        .is_err()
    {
        // Another subscriber (e.g. in tests) is already installed.
        return Ok(false);
    }
    let _ = GUARD.set(guard);
    install_panic_hook(dir);
    tracing::info!(version = crate::VERSION, "core logging initialized");
    Ok(true)
}

fn install_panic_hook(dir: &Path) {
    if CRASH_HOOK.set(()).is_err() {
        return;
    }
    let path = dir.join("crash-report.log");
    let _ = std::fs::create_dir_all(dir);
    panic::set_hook(Box::new(move |info| {
        let backtrace = Backtrace::force_capture();
        let location = info
            .location()
            .map(|location| location.to_string())
            .unwrap_or_else(|| "unknown".into());
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("non-string panic");
        let report = format!(
            "Galaxy Rust panic\nlocation: {location}\nmessage: {message}\nbacktrace:\n{backtrace}\n\n"
        );
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = file.write_all(report.as_bytes());
        }
    }));
}

fn parse_level(s: &str) -> tracing::Level {
    match s.trim().to_ascii_lowercase().as_str() {
        "error" => tracing::Level::ERROR,
        "warn" => tracing::Level::WARN,
        "debug" => tracing::Level::DEBUG,
        "trace" => tracing::Level::TRACE,
        _ => tracing::Level::INFO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_levels_with_info_fallback() {
        assert_eq!(parse_level("WARN"), tracing::Level::WARN);
        assert_eq!(parse_level(" trace "), tracing::Level::TRACE);
        assert_eq!(parse_level("nonsense"), tracing::Level::INFO);
    }

    #[test]
    fn init_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let first = init(dir.path(), "info").unwrap();
        let second = init(dir.path(), "info").unwrap();
        assert!(first);
        assert!(!second);
    }
}
