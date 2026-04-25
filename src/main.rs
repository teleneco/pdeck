mod cli;
mod config;
mod live;
mod log;
mod model;
mod probe;
mod record;
mod replay;
mod stats;
mod ui;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::cli::{Args, Command};
use crate::config::{build_status_line, open_editor_with_tempfile, resolve_record_path};
use crate::live::run_app;
use crate::log::{init_text_log_file, resolve_log_path, write_log_from_record};
use crate::model::App;
use crate::record::{init_record_file, read_session_header};
use crate::replay::run_replay;
use crate::stats::{resolve_stats_path, write_stats_from_record};

const DEFAULT_INTERVAL_MS: u64 = 500;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = Args::parse();
    validate_args(&args)?;

    match args.command.clone() {
        Some(Command::Replay { file }) => run_replay_command(args, file).await,
        Some(Command::Stats { file, output }) => {
            let stats_path = resolve_stats_path(&file, output.as_ref());
            write_stats_from_record(&file, &stats_path)
        }
        Some(Command::Log { file, output }) => {
            let log_path = resolve_log_path(&file, output.as_ref());
            write_log_from_record(&file, &log_path)
        }
        None => run_legacy_or_live(&mut args).await,
    }
}

fn validate_args(args: &Args) -> Result<()> {
    if args.interval.0 < Duration::from_millis(DEFAULT_INTERVAL_MS) {
        bail!("interval must be at least 500ms");
    }
    if args.replay.is_some() && args.record.is_some() {
        bail!("--record and --replay cannot be used together");
    }
    if args.stats.is_some() && args.replay.is_none() {
        bail!("--stats requires --replay <FILE>");
    }
    if args.command.is_some() && (args.replay.is_some() || args.stats.is_some()) {
        bail!("subcommands cannot be combined with legacy --replay or --stats options");
    }
    Ok(())
}

async fn run_replay_command(mut args: Args, replay_path: PathBuf) -> Result<()> {
    args.replay = Some(replay_path.clone());
    let targets = read_session_header(&replay_path)?;
    if targets.is_empty() {
        bail!("no targets found in {}", replay_path.display());
    }

    let status_line = build_status_line(&args, None);
    let mut app = App::new(args, targets, status_line);
    let mut terminal = ui::init_terminal()?;
    let result = run_replay(&mut terminal, &mut app, &replay_path, None).await;
    ui::restore_terminal()?;
    result
}

async fn run_legacy_or_live(args: &mut Args) -> Result<()> {
    let mut temp_file = None;
    if args.vi_mode {
        let guard = open_editor_with_tempfile().context("failed to create temporary ping list")?;
        args.file = guard.path().clone();
        temp_file = Some(guard);
    }

    let targets = if let Some(replay_path) = &args.replay {
        read_session_header(replay_path)?
    } else {
        probe::parse_targets(&args.file, args.arp_entries)
            .with_context(|| format!("failed to read {}", args.file.display()))?
    };
    if targets.is_empty() {
        bail!("no targets found in {}", args.file.display());
    }

    let record_path = args
        .record
        .as_ref()
        .map(|record_arg| resolve_record_path(&args.file, record_arg.as_ref()));
    let status_line = build_status_line(args, record_path.as_ref());
    let mut app = App::new(args.clone(), targets, status_line);

    if let Some(stats_arg) = &args.stats {
        let Some(replay_path) = &args.replay else {
            bail!("--stats requires --replay <FILE>");
        };
        let stats_path = resolve_stats_path(replay_path, stats_arg.as_ref());
        return write_stats_from_record(replay_path, &stats_path);
    }

    let mut terminal = ui::init_terminal()?;
    let result = if let Some(replay_path) = &args.replay {
        let mut log_file = if let Some(log_path) = &args.log {
            Some(init_text_log_file(log_path)?)
        } else {
            None
        };
        run_replay(&mut terminal, &mut app, replay_path, log_file.as_mut()).await
    } else {
        let mut record_file = if let Some(record_path) = &record_path {
            Some(init_record_file(record_path, &app.targets)?)
        } else {
            None
        };
        let mut log_file = if let Some(log_path) = &args.log {
            Some(init_text_log_file(log_path)?)
        } else {
            None
        };
        run_app(
            &mut terminal,
            &mut app,
            record_file.as_mut(),
            log_file.as_mut(),
        )
        .await
    };
    ui::restore_terminal()?;
    drop(temp_file);

    result
}
