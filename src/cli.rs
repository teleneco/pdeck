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

    #[arg(short = 'i', default_value = "500ms")]
    pub interval: DurationArg,

    #[arg(short = 't', default_value = "3s")]
    pub timeout: DurationArg,

    #[arg(short = 'f', default_value = DEFAULT_TARGETS_FILE)]
    pub file: PathBuf,

    #[arg(short = 'A')]
    pub arp_entries: bool,

    #[arg(short = 'V')]
    pub vi_mode: bool,

    #[arg(short = 'c', long, default_value_t = 16)]
    pub concurrency: usize,

    #[arg(long, value_enum, default_value_t = IcmpBackendArg::Auto)]
    pub icmp_backend: IcmpBackendArg,

    #[arg(long, num_args = 0..=1, value_name = "FILE")]
    pub record: Option<Option<PathBuf>>,

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
        #[arg(value_name = "FILE")]
        file: PathBuf,
    },
    #[command(about = "Convert a recorded JSONL session to per-host CSV statistics")]
    Stats {
        #[arg(value_name = "FILE")]
        file: PathBuf,

        #[arg(short = 'o', long, value_name = "FILE")]
        output: Option<PathBuf>,
    },
    #[command(about = "Convert a recorded JSONL session to the text log format")]
    Log {
        #[arg(value_name = "FILE")]
        file: PathBuf,

        #[arg(short = 'o', long, value_name = "FILE")]
        output: Option<PathBuf>,
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

impl FromStr for DurationArg {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        parse_duration(s).map(DurationArg)
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
