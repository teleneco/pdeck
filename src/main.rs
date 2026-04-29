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
use clap::{CommandFactory, FromArgMatches};

use crate::cli::{Args, Command, ConfigCommand};
use crate::config::{
    ArgSources, ConfigSetStatus, ConfigUpdate, RuntimeConfig, build_status_line, ensure_record_dir,
    load_and_apply_config, resolve_record_path, set_config, show_config, verify_config,
};
use crate::live::{run_app, run_no_tui_app};
use crate::log::{resolve_log_path, write_log_from_record};
use crate::model::App;
use crate::record::{SessionReadMode, init_record_file, read_session_events_with_mode};
use crate::replay::run_replay;
use crate::stats::{resolve_stats_path, write_stats_from_record};

const DEFAULT_INTERVAL_MS: u64 = 500;
const MAX_CONCURRENCY: usize = 1024;

#[tokio::main]
async fn main() -> Result<()> {
    let matches = Args::command().get_matches();
    let sources = ArgSources::from_matches(&matches);
    let mut args = Args::from_arg_matches(&matches)?;
    let runtime_config = if args.command.is_none() {
        load_and_apply_config(&mut args, &sources)?
    } else {
        RuntimeConfig::default()
    };
    validate_args(&args)?;

    match args.command.clone() {
        Some(Command::Config { command }) => run_config_command(command),
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
        None => run_live(&mut args, runtime_config).await,
    }
}

fn run_config_command(command: Option<ConfigCommand>) -> Result<()> {
    match command.unwrap_or(ConfigCommand::Show) {
        ConfigCommand::Set {
            record,
            record_dir,
            record_size_limit,
            always_use_record_dir,
        } => {
            let result = set_config(ConfigUpdate {
                record,
                record_dir,
                record_size_limit,
                always_use_record_dir,
            })?;
            match result.status {
                ConfigSetStatus::Initialized => {
                    println!("config initialized: {}", result.config_path.display());
                }
                ConfigSetStatus::Updated => {
                    println!("config updated: {}", result.config_path.display());
                }
                ConfigSetStatus::Unchanged => {
                    println!("config unchanged: {}", result.config_path.display());
                }
            }
            println!("records directory: {}", result.record_dir.display());
        }
        ConfigCommand::Show => {
            let (path, contents) = show_config()?;
            println!("# {}", path.display());
            print!("{contents}");
        }
        ConfigCommand::Verify => {
            let result = verify_config()?;
            println!("config ok: {}", result.config_path.display());
            println!("records directory: {}", result.record_dir.display());
        }
    }
    Ok(())
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
    if args.record_overwrite {
        bail!("--record-overwrite is not supported for rotated v2 records");
    }
    if args.no_record && args.record.is_some() {
        bail!("--record and --no-record cannot be used together");
    }
    if args.record_size_limit.0 > 0 && args.record.is_none() {
        bail!("--record-size-limit requires --record");
    }
    Ok(())
}

async fn run_replay_command(args: Args, replay_path: PathBuf, mode: SessionReadMode) -> Result<()> {
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

async fn run_live(args: &mut Args, runtime_config: RuntimeConfig) -> Result<()> {
    let targets = probe::parse_targets(&args.file, args.arp_entries)
        .with_context(|| format!("failed to read {}", args.file.display()))?;
    if targets.is_empty() {
        bail!("no targets found in {}", args.file.display());
    }

    let generated_record_dir = runtime_config
        .always_use_record_dir
        .then_some(runtime_config.record_dir.as_ref())
        .flatten();
    if matches!(args.record, Some(None)) && generated_record_dir.is_some() {
        ensure_record_dir(generated_record_dir)?;
    }
    let record_path = args.record.as_ref().map(|record_arg| {
        resolve_record_path(&args.file, record_arg.as_ref(), generated_record_dir)
    });
    let status_line = build_status_line(args, record_path.as_ref());
    let mut app = App::new(args.clone(), targets, status_line);

    if args.no_tui {
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
    run_app(
        terminal_guard.terminal(),
        &mut app,
        record_file.as_mut(),
        None,
    )
    .await
}
