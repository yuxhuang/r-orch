//! Structured logging and sink configuration for socket-backed `orch serve`.
//! Execution, terminal-batch decisions and peer lifecycle belong to the daemon.

pub mod logger;

pub use logger::{LogFormat, LogLevel, LogSink, StructuredLogger};
use orch_core::error::{Error, Result};

/// The tick interval, applied only when it is a positive
/// number — `--tick-interval abc` is **silently ignored**.
pub fn resolved_poll_interval(raw: Option<&str>, configured: u64) -> u64 {
    raw.and_then(|raw| raw.trim().parse::<i64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(configured, |ms| ms as u64)
}

/// Open the sinks: stdout always, plus the `--log-file` in append mode.
pub fn open_sinks(log_file: Option<&str>) -> Result<Vec<LogSink>> {
    let mut sinks = vec![LogSink::Stdout];
    if let Some(path) = log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|err| Error::io("open the serve log file", path, err))?;
        sinks.push(LogSink::Writer(Box::new(file)));
    }
    Ok(sinks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_positive_tick_interval_overrides_the_configured_one() {
        assert_eq!(resolved_poll_interval(Some("1000"), 10_000), 1_000);
        assert_eq!(resolved_poll_interval(Some(" 250 "), 10_000), 250);
    }

    #[test]
    fn a_junk_or_non_positive_tick_interval_is_silently_ignored() {
        for raw in ["abc", "", "0", "-5", "1.5"] {
            assert_eq!(
                resolved_poll_interval(Some(raw), 10_000),
                10_000,
                "--tick-interval {raw}"
            );
        }
        assert_eq!(resolved_poll_interval(None, 10_000), 10_000);
    }

    #[test]
    fn the_log_file_is_opened_in_append_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("serve.log");
        std::fs::write(&path, "existing\n").expect("seed");

        let sinks = open_sinks(Some(&path.to_string_lossy())).expect("opens");
        assert_eq!(sinks.len(), 2, "stdout plus the file");
        drop(sinks);

        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "existing\n",
            "opening must not truncate"
        );
    }

    #[test]
    fn a_log_file_that_cannot_be_opened_is_an_error() {
        let err =
            open_sinks(Some("/orch-definitely-not-writable/serve.log")).expect_err("cannot open");
        assert_eq!(err.exit_code(), 1);
    }

    #[test]
    fn stdout_is_the_only_sink_without_a_log_file() {
        let sinks = open_sinks(None).expect("opens");
        assert_eq!(sinks.len(), 1);
        assert!(matches!(sinks[0], LogSink::Stdout));
    }
}
