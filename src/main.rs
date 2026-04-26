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
use crate::config::{build_status_line, resolve_record_path};
use crate::live::{run_app, run_no_tui_app};
use crate::log::{init_text_log_file, resolve_log_path, write_log_from_record};
use crate::model::App;
use crate::record::{SessionReadMode, init_record_file, read_session_events_with_mode};
use crate::replay::run_replay;
use crate::stats::{resolve_stats_path, write_stats_from_record};

const DEFAULT_INTERVAL_MS: u64 = 500;
const MAX_CONCURRENCY: usize = 1024;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = Args::parse();
    validate_args(&args)?;

    match args.command.clone() {
        Some(Command::Replay { file, only }) => {
            let (file, mode) = resolve_session_input(file, only)?;
            run_replay_command(args, file, mode).await
        }
        Some(Command::Stats { file, output, only }) => {
            let (file, mode) = resolve_session_input(file, only)?;
            let stats_path = resolve_stats_path(&file, output.as_ref(), mode);
            write_stats_from_record(&file, &stats_path, mode)
        }
        Some(Command::Log { file, output, only }) => {
            let (file, mode) = resolve_session_input(file, only)?;
            let log_path = resolve_log_path(&file, output.as_ref(), mode);
            write_log_from_record(&file, &log_path, mode)
        }
        None => run_legacy_or_live(&mut args).await,
    }
}

fn resolve_session_input(
    file: Option<PathBuf>,
    only: Option<PathBuf>,
) -> Result<(PathBuf, SessionReadMode)> {
    match (file, only) {
        (Some(file), None) => Ok((file, SessionReadMode::Auto)),
        (None, Some(file)) => Ok((file, SessionReadMode::Only)),
        (Some(_), Some(_)) => bail!("provide either <FILE> or --only <FILE>, not both"),
        (None, None) => bail!("missing replay file"),
    }
}

fn validate_args(args: &Args) -> Result<()> {
    if args.interval.0 < Duration::from_millis(DEFAULT_INTERVAL_MS) {
        bail!("interval must be at least 500ms");
    }
    if args.concurrency == 0 || args.concurrency > MAX_CONCURRENCY {
        bail!("concurrency must be between 1 and {MAX_CONCURRENCY}");
    }
    if args.replay.is_some() && args.record.is_some() {
        bail!("--record and --replay cannot be used together");
    }
    if args.record_overwrite {
        bail!("--record-overwrite is not supported for rotated v2 records");
    }
    if args.record_size_limit.0 > 0 && args.record.is_none() {
        bail!("--record-size-limit requires --record");
    }
    if args.stats.is_some() && args.replay.is_none() {
        bail!("--stats requires --replay <FILE>");
    }
    if args.command.is_some() && (args.replay.is_some() || args.stats.is_some()) {
        bail!("subcommands cannot be combined with legacy --replay or --stats options");
    }
    Ok(())
}

async fn run_replay_command(
    mut args: Args,
    replay_path: PathBuf,
    mode: SessionReadMode,
) -> Result<()> {
    args.replay = Some(replay_path.clone());
    let session = read_session_events_with_mode(&replay_path, mode)?;
    let targets = session.targets.clone();
    if targets.is_empty() {
        bail!("no targets found in {}", replay_path.display());
    }

    let status_line = build_status_line(&args, None);
    let mut app = App::new(args, targets, status_line);
    let mut terminal_guard = ui::TerminalGuard::new()?;
    run_replay(terminal_guard.terminal(), &mut app, session, None).await
}

async fn run_legacy_or_live(args: &mut Args) -> Result<()> {
    let targets = if let Some(replay_path) = &args.replay {
        read_session_events_with_mode(replay_path, SessionReadMode::Auto)?.targets
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
        let stats_path = resolve_stats_path(replay_path, stats_arg.as_ref(), SessionReadMode::Auto);
        return write_stats_from_record(replay_path, &stats_path, SessionReadMode::Auto);
    }

    if args.no_tui && args.replay.is_none() {
        let mut record_file = if let Some(record_path) = &record_path {
            Some(init_record_file(
                record_path,
                &app.targets,
                args.record_overwrite,
                matches!(args.record, Some(None)),
                args.record_size_limit.0,
            )?)
        } else {
            None
        };
        if let Some(record_file) = &record_file {
            app.status_line = build_status_line(args, Some(&record_file.path().to_path_buf()));
        }
        return run_no_tui_app(&mut app, record_file.as_mut()).await;
    }

    let mut terminal_guard = ui::TerminalGuard::new()?;
    if let Some(replay_path) = &args.replay {
        let mut log_file = if let Some(log_path) = &args.log {
            Some(init_text_log_file(log_path)?)
        } else {
            None
        };
        run_replay(
            terminal_guard.terminal(),
            &mut app,
            read_session_events_with_mode(replay_path, SessionReadMode::Auto)?,
            log_file.as_mut(),
        )
        .await
    } else {
        let mut record_file = if let Some(record_path) = &record_path {
            Some(init_record_file(
                record_path,
                &app.targets,
                args.record_overwrite,
                matches!(args.record, Some(None)),
                args.record_size_limit.0,
            )?)
        } else {
            None
        };
        if let Some(record_file) = &record_file {
            app.status_line = build_status_line(args, Some(&record_file.path().to_path_buf()));
        }
        let mut log_file = if let Some(log_path) = &args.log {
            Some(init_text_log_file(log_path)?)
        } else {
            None
        };
        run_app(
            terminal_guard.terminal(),
            &mut app,
            record_file.as_mut(),
            log_file.as_mut(),
        )
        .await
    }
}
