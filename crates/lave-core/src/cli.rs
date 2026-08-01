//! Command line interface.

use clap::{Parser, ValueEnum};

/// A GTK GUI for Docker.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(name = "lave", version, about, long_about = None)]
pub struct Cli {
    /// Docker daemon endpoint, overriding `DOCKER_HOST` and any active Docker context.
    // The doc comment needs backticks for clippy; the help text must not show them.
    #[arg(
        long,
        value_name = "URL",
        help = "Docker daemon endpoint, overriding DOCKER_HOST and any active Docker context"
    )]
    pub docker_host: Option<String>,

    /// Logging verbosity.
    #[arg(long, value_enum, default_value_t = LogLevel::Warn)]
    pub log_level: LogLevel,

    /// Do not publish a desktop panel indicator.
    #[arg(long)]
    pub no_indicator: bool,
}

/// Verbosity accepted by `--log-level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    /// Name understood by `tracing_subscriber`'s env filter.
    #[must_use]
    pub fn as_filter(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(args)
    }

    #[test]
    fn defaults_are_conservative() {
        let cli = parse(&["lave"]).expect("bare invocation parses");
        assert_eq!(cli.docker_host, None);
        assert_eq!(cli.log_level, LogLevel::Warn);
        assert!(!cli.no_indicator);
    }

    #[test]
    fn docker_host_is_captured_verbatim() {
        let cli = parse(&["lave", "--docker-host", "unix:///run/docker.sock"])
            .expect("valid docker host parses");
        assert_eq!(cli.docker_host.as_deref(), Some("unix:///run/docker.sock"));
    }

    #[test]
    fn log_level_accepts_each_variant() {
        for (arg, expected) in [
            ("error", LogLevel::Error),
            ("warn", LogLevel::Warn),
            ("info", LogLevel::Info),
            ("debug", LogLevel::Debug),
            ("trace", LogLevel::Trace),
        ] {
            let cli = parse(&["lave", "--log-level", arg]).expect("valid level parses");
            assert_eq!(cli.log_level, expected);
            assert_eq!(cli.log_level.as_filter(), arg);
        }
    }

    #[test]
    fn no_indicator_is_a_flag() {
        let cli = parse(&["lave", "--no-indicator"]).expect("flag parses");
        assert!(cli.no_indicator);
    }

    #[test]
    fn unknown_log_level_is_rejected() {
        assert!(parse(&["lave", "--log-level", "chatty"]).is_err());
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(parse(&["lave", "--turbo"]).is_err());
    }

    #[test]
    fn docker_host_requires_a_value() {
        assert!(parse(&["lave", "--docker-host"]).is_err());
    }
}
