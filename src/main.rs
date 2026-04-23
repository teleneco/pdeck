mod cli;
mod model;
mod probe;
mod ui;

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Local;
use clap::Parser;
use crossterm::event::{self, Event};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use crate::cli::Args;
use crate::model::{App, ProbeEvent, Target};

const DEFAULT_INTERVAL_MS: u64 = 500;

#[derive(Serialize, Deserialize)]
struct SessionHeader {
    version: u8,
    targets: Vec<Target>,
}

struct TempFileGuard {
    path: PathBuf,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

struct SessionEventReader {
    reader: BufReader<File>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = Args::parse();
    if args.interval.0 < Duration::from_millis(DEFAULT_INTERVAL_MS) {
        bail!("interval must be at least 500ms");
    }
    if args.replay.is_some() && args.record.is_some() {
        bail!("--record and --replay cannot be used together");
    }

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

    let status_line = build_status_line(&args);
    let mut app = App::new(args.clone(), targets, status_line);
    let mut terminal = ui::init_terminal()?;
    let result = if let Some(replay_path) = &args.replay {
        let mut log_file = if let Some(log_path) = &args.log {
            Some(init_text_log_file(log_path)?)
        } else {
            None
        };
        run_replay(&mut terminal, &mut app, replay_path, log_file.as_mut()).await
    } else {
        let mut record_file = if let Some(record_path) = &args.record {
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

async fn run_app(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    mut record_file: Option<&mut File>,
    mut log_file: Option<&mut File>,
) -> Result<()> {
    const UI_TICK_MS: u64 = 33;
    let (tx, mut rx) = mpsc::channel(256);
    let (pause_tx, pause_rx) = watch::channel(false);
    let args = app.args.clone();
    let targets = app.targets.clone();
    tokio::spawn(async move {
        let _ = probe::probe_loop(args, targets, tx, pause_rx).await;
    });

    loop {
        let mut dirty = false;
        while let Ok(event) = rx.try_recv() {
            app.apply_probe_event(&event);
            if let Some(file) = record_file.as_deref_mut() {
                append_record_event(file, &event)?;
            }
            if let Some(file) = log_file.as_deref_mut() {
                append_text_log_event(file, &event)?;
            }
            dirty = true;
        }

        if dirty {
            terminal.draw(|frame| ui::draw_ui(frame, app))?;
        }

        if event::poll(Duration::from_millis(UI_TICK_MS))? {
            if let Event::Key(key) = event::read()? {
                if ui::handle_key(app, key, &pause_tx)? {
                    return Ok(());
                }
                terminal.draw(|frame| ui::draw_ui(frame, app))?;
            }
        }
    }
}

async fn run_replay(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    replay_path: &PathBuf,
    mut log_file: Option<&mut File>,
) -> Result<()> {
    const UI_TICK_MS: u64 = 33;
    let mut events = open_session_event_reader(replay_path)?;
    let (pause_tx, _pause_rx) = watch::channel(false);
    let Some(mut current_event) = events.read_next_event()? else {
        bail!("no replay events found in {}", replay_path.display());
    };
    let mut previous_ts = current_event.ts_ms;

    terminal.draw(|frame| ui::draw_ui(frame, app))?;

    loop {
        let sleep_ms = current_event.ts_ms.saturating_sub(previous_ts);
        previous_ts = current_event.ts_ms;
        let mut remaining = sleep_ms;
        while remaining > 0 {
            let step = remaining.min(UI_TICK_MS);
            tokio::time::sleep(Duration::from_millis(step)).await;
            remaining -= step;
            if event::poll(Duration::from_millis(1))? {
                if let Event::Key(key) = event::read()? {
                    if ui::handle_key(app, key, &pause_tx)? {
                        return Ok(());
                    }
                }
            }
            terminal.draw(|frame| ui::draw_ui(frame, app))?;
        }
        app.apply_probe_event(&current_event);
        if let Some(file) = log_file.as_deref_mut() {
            append_text_log_event(file, &current_event)?;
        }
        terminal.draw(|frame| ui::draw_ui(frame, app))?;

        let Some(next_event) = events.read_next_event()? else {
            break;
        };
        current_event = next_event;
    }

    loop {
        terminal.draw(|frame| ui::draw_ui(frame, app))?;
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if ui::handle_key(app, key, &pause_tx)? {
                    return Ok(());
                }
            }
        }
    }
}

fn open_editor_with_tempfile() -> Result<TempFileGuard> {
    let path =
        std::env::temp_dir().join(format!("pdeck-{}.txt", Local::now().format("%Y%m%d%H%M%S")));
    File::create(&path)?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let (program, args) = parse_editor_command(&editor)?;
    let status = std::process::Command::new(program)
        .args(args)
        .arg(&path)
        .status()?;
    if !status.success() {
        bail!("editor exited with status {status}");
    }
    Ok(TempFileGuard::new(path))
}

fn parse_editor_command(input: &str) -> Result<(String, Vec<String>)> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' => {
                escaped = true;
            }
            '\'' | '"' => {
                if let Some(active) = quote {
                    if active == ch {
                        quote = None;
                    } else {
                        current.push(ch);
                    }
                } else {
                    quote = Some(ch);
                }
            }
            c if c.is_whitespace() && quote.is_none() => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped {
        bail!("EDITOR ends with an escape character");
    }
    if quote.is_some() {
        bail!("EDITOR contains an unmatched quote");
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() {
        bail!("EDITOR must not be empty");
    }

    let program = parts.remove(0);
    Ok((program, parts))
}

fn init_record_file(path: &PathBuf, targets: &[Target]) -> Result<File> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("failed to open record file {}", path.display()))?;
    let header = SessionHeader {
        version: 1,
        targets: targets.to_vec(),
    };
    writeln!(file, "{}", serde_json::to_string(&header)?)?;
    file.flush()?;
    Ok(file)
}

fn append_record_event(file: &mut File, event: &ProbeEvent) -> Result<()> {
    writeln!(file, "{}", serde_json::to_string(event)?)?;
    file.flush()?;
    Ok(())
}

fn init_text_log_file(path: &PathBuf) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("failed to open log file {}", path.display()))
}

fn append_text_log_event(file: &mut File, event: &ProbeEvent) -> Result<()> {
    file.write_all(event.log_line.as_bytes())?;
    file.flush()?;
    Ok(())
}

fn build_status_line(args: &Args) -> String {
    let mut parts = Vec::new();
    if let Some(replay_path) = &args.replay {
        parts.push(format!("replay: {}", replay_path.display()));
    } else {
        parts.push("live".to_string());
    }
    if let Some(record_path) = &args.record {
        parts.push(format!("record: {}", record_path.display()));
    }
    if let Some(log_path) = &args.log {
        parts.push(format!("log: {}", log_path.display()));
    }
    parts.join(" | ")
}

fn read_session_header(path: &PathBuf) -> Result<Vec<Target>> {
    let file = File::open(path)
        .with_context(|| format!("failed to open replay file {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    let header: SessionHeader = serde_json::from_str(first_line.trim_end())?;
    Ok(header.targets)
}

fn open_session_event_reader(path: &PathBuf) -> Result<SessionEventReader> {
    let file = File::open(path)
        .with_context(|| format!("failed to open replay file {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    if first_line.trim().is_empty() {
        bail!("replay header is empty");
    }
    let _: SessionHeader = serde_json::from_str(first_line.trim_end())?;
    Ok(SessionEventReader { reader })
}

impl SessionEventReader {
    fn read_next_event(&mut self) -> Result<Option<ProbeEvent>> {
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line)?;
            if read == 0 {
                return Ok(None);
            }
            if line.trim().is_empty() {
                continue;
            }
            return Ok(Some(serde_json::from_str::<ProbeEvent>(line.trim_end())?));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_editor_command;

    #[test]
    fn parses_editor_command_with_args() {
        let (program, args) = parse_editor_command("code --wait").unwrap();
        assert_eq!(program, "code");
        assert_eq!(args, vec!["--wait"]);
    }

    #[test]
    fn parses_editor_command_with_quotes() {
        let (program, args) = parse_editor_command(
            "\"/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code\" --wait",
        )
        .unwrap();
        assert_eq!(
            program,
            "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"
        );
        assert_eq!(args, vec!["--wait"]);
    }

    #[test]
    fn rejects_unmatched_quote_in_editor_command() {
        let err = parse_editor_command("\"code --wait").unwrap_err();
        assert!(err.to_string().contains("unmatched quote"));
    }
}
