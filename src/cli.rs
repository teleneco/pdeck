use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand, ValueEnum};

const DEFAULT_TARGETS_FILE: &str = "targets.txt";

#[derive(Parser, Debug, Clone)]
#[command(
    name = "pdeck",
    version,
    about = "Probe deck for terminal network monitoring",
    disable_version_flag = true
)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(short = 'i', default_value = "1s")]
    pub interval: DurationArg,

    #[arg(short = 't', default_value = "3s")]
    pub timeout: DurationArg,

    #[arg(short = 'f', default_value = DEFAULT_TARGETS_FILE)]
    pub file: PathBuf,

    #[arg(short = 'A')]
    pub arp_entries: bool,

    #[arg(short = 'c', long, default_value_t = 16)]
    pub concurrency: usize,

    #[arg(long, value_enum, default_value_t = IcmpBackendArg::Auto)]
    pub icmp_backend: IcmpBackendArg,

    #[arg(long, num_args = 0..=1, value_name = "FILE")]
    pub record: Option<Option<PathBuf>>,

    #[arg(long, hide = true)]
    pub record_overwrite: bool,

    #[arg(long, default_value = "0", value_name = "SIZE")]
    pub record_size_limit: SizeArg,

    #[arg(long)]
    pub no_tui: bool,

    #[arg(long, hide = true)]
    pub replay: Option<PathBuf>,

    #[arg(long, hide = true)]
    pub log: Option<PathBuf>,

    #[arg(long, num_args = 0..=1, value_name = "FILE", hide = true)]
    pub stats: Option<Option<PathBuf>>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    #[command(about = "Replay a recorded JSONL session in the TUI")]
    Replay {
        #[arg(value_name = "FILE", help = "Recorded JSONL file to replay")]
        file: Option<PathBuf>,

        #[arg(
            long,
            value_name = "FILE",
            help = "Replay only this JSONL file without v2 session discovery"
        )]
        only: Option<PathBuf>,
    },
    #[command(about = "Convert a recorded JSONL session to per-host CSV statistics")]
    Stats {
        #[arg(value_name = "FILE", help = "Recorded JSONL file to convert")]
        file: Option<PathBuf>,

        #[arg(short = 'o', long, value_name = "FILE", help = "Output CSV path")]
        output: Option<PathBuf>,

        #[arg(
            long,
            value_name = "FILE",
            help = "Convert only this JSONL file without v2 session discovery"
        )]
        only: Option<PathBuf>,
    },
    #[command(about = "Convert a recorded JSONL session to the text log format")]
    Log {
        #[arg(value_name = "FILE", help = "Recorded JSONL file to convert")]
        file: Option<PathBuf>,

        #[arg(short = 'o', long, value_name = "FILE", help = "Output text log path")]
        output: Option<PathBuf>,

        #[arg(
            long,
            value_name = "FILE",
            help = "Convert only this JSONL file without v2 session discovery"
        )]
        only: Option<PathBuf>,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum IcmpBackendArg {
    Auto,
    Exec,
    Api,
}

#[derive(Clone, Debug)]
pub struct DurationArg(pub Duration);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SizeArg(pub u64);

impl FromStr for DurationArg {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        parse_duration(s).map(DurationArg)
    }
}

impl FromStr for SizeArg {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        parse_size(s).map(SizeArg)
    }
}

fn parse_duration(input: &str) -> Result<Duration> {
    let input = input.trim();
    if let Some(ms) = input.strip_suffix("ms") {
        let value = ms.parse::<u64>()?;
        return Ok(Duration::from_millis(value));
    }
    if let Some(sec) = input.strip_suffix('s') {
        let value = sec.parse::<u64>()?;
        return Ok(Duration::from_secs(value));
    }
    Err(anyhow!("duration must end with ms or s"))
}

fn parse_size(input: &str) -> Result<u64> {
    let input = input.trim();
    if input.is_empty() {
        return Err(anyhow!("size cannot be empty"));
    }

    let split_at = input
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(input.len());
    let (digits, suffix) = input.split_at(split_at);
    if digits.is_empty() {
        return Err(anyhow!("size must start with a number"));
    }
    let value = digits.parse::<u64>()?;
    let suffix = suffix.trim().to_ascii_lowercase();
    let multiplier = match suffix.as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1_000,
        "m" | "mb" => 1_000_000,
        "g" | "gb" => 1_000_000_000,
        "t" | "tb" => 1_000_000_000_000,
        "kib" => 1024,
        "mib" => 1024 * 1024,
        "gib" => 1024 * 1024 * 1024,
        "tib" => 1024_u64.pow(4),
        _ => {
            return Err(anyhow!(
                "size suffix must be one of b, kb, mb, gb, tb, kib, mib, gib, or tib"
            ));
        }
    };
    value
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow!("size is too large"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clap::Parser;

    use super::{Args, parse_duration, parse_size};

    #[test]
    fn parses_size_units() {
        assert_eq!(parse_size("0").unwrap(), 0);
        assert_eq!(parse_size("123").unwrap(), 123);
        assert_eq!(parse_size("1kb").unwrap(), 1_000);
        assert_eq!(parse_size("1KB").unwrap(), 1_000);
        assert_eq!(parse_size("2mb").unwrap(), 2_000_000);
        assert_eq!(parse_size("3gb").unwrap(), 3_000_000_000);
        assert_eq!(parse_size("4kib").unwrap(), 4096);
        assert_eq!(parse_size("5MiB").unwrap(), 5 * 1024 * 1024);
    }

    #[test]
    fn rejects_invalid_size_units() {
        assert!(parse_size("").is_err());
        assert!(parse_size("mb").is_err());
        assert!(parse_size("1xb").is_err());
    }

    #[test]
    fn parses_duration_units() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("3s").unwrap(), Duration::from_secs(3));
    }

    #[test]
    fn defaults_interval_to_one_second_and_timeout_to_three_seconds() {
        let args = Args::parse_from(["pdeck"]);

        assert_eq!(args.interval.0, Duration::from_secs(1));
        assert_eq!(args.timeout.0, Duration::from_secs(3));
    }
}
