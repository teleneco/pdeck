mod cli;
mod model;
mod probe;
mod ui;

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::{Local, TimeZone};
use clap::Parser;
use crossterm::event::{self, Event};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use crate::cli::Args;
use crate::model::{App, HostStats, ProbeEvent, Target};

const DEFAULT_INTERVAL_MS: u64 = 500;
const REPLAY_SPEEDS: [u64; 4] = [1, 2, 5, 10];
const REPLAY_UI_TICK_MS: u64 = 33;

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
    if args.stats.is_some() && args.replay.is_none() {
        bail!("--stats requires --replay <FILE>");
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

    let record_path = args
        .record
        .as_ref()
        .map(|record_arg| resolve_record_path(&args.file, record_arg.as_ref()));
    let status_line = build_status_line(&args, record_path.as_ref());
    let mut app = App::new(args.clone(), targets, status_line);
    if let Some(stats_arg) = &args.stats {
        let Some(replay_path) = &args.replay else {
            bail!("--stats requires --replay <FILE>");
        };
        let stats_path = resolve_stats_path(replay_path, stats_arg.as_ref());
        write_stats_from_record(replay_path, &stats_path, &mut app)?;
        return Ok(());
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
    let (pause_tx, _pause_rx) = watch::channel(false);
    let events = read_session_events(replay_path)?;
    if events.is_empty() {
        bail!("no replay events found in {}", replay_path.display());
    }

    let start_ts = events.first().map(|event| event.ts_ms).unwrap_or(0);
    let end_ts = events.last().map(|event| event.ts_ms).unwrap_or(start_ts);
    let mut replay = ReplayState::new(start_ts, end_ts);
    let mut next_event_index = 0;
    update_replay_status(app, &replay, replay_path);

    terminal.draw(|frame| ui::draw_ui(frame, app))?;

    loop {
        let tick_start = Instant::now();
        if event::poll(Duration::from_millis(1))? {
            if let Event::Key(key) = event::read()? {
                if handle_replay_key(app, key, &mut replay, replay_path, &pause_tx)? {
                    return Ok(());
                }
            }
        }

        let mut seek_target = replay.take_seek_target();
        if seek_target.is_none() && !app.paused && replay.current_ts < end_ts {
            let elapsed_ms = tick_start.elapsed().as_millis() as u64;
            let step_ms = elapsed_ms
                .max(REPLAY_UI_TICK_MS)
                .saturating_mul(replay.speed());
            seek_target = Some(replay.current_ts.saturating_add(step_ms).min(end_ts));
        }

        if let Some(target_ts) = seek_target {
            if target_ts < replay.current_ts {
                next_event_index = rebuild_replay_to(app, &events, target_ts);
            } else {
                while let Some(event) = events.get(next_event_index) {
                    if event.ts_ms > target_ts {
                        break;
                    }
                    app.apply_probe_event(event);
                    if let Some(file) = log_file.as_deref_mut() {
                        append_text_log_event(file, event)?;
                    }
                    next_event_index += 1;
                }
            }
            replay.current_ts = target_ts;
            update_replay_status(app, &replay, replay_path);
        }

        terminal.draw(|frame| ui::draw_ui(frame, app))?;
        tokio::time::sleep(Duration::from_millis(REPLAY_UI_TICK_MS)).await;
    }
}

struct ReplayState {
    current_ts: u64,
    start_ts: u64,
    end_ts: u64,
    speed_index: usize,
    seek_target: Option<u64>,
}

impl ReplayState {
    fn new(start_ts: u64, end_ts: u64) -> Self {
        Self {
            current_ts: start_ts,
            start_ts,
            end_ts,
            speed_index: 0,
            seek_target: None,
        }
    }

    fn speed(&self) -> u64 {
        REPLAY_SPEEDS[self.speed_index]
    }

    fn set_speed(&mut self, speed: u64) {
        if let Some(index) = REPLAY_SPEEDS.iter().position(|value| *value == speed) {
            self.speed_index = index;
        }
    }

    fn seek_relative(&mut self, seconds: i64) {
        let delta_ms = seconds.saturating_mul(1000);
        let target = if delta_ms.is_negative() {
            self.current_ts.saturating_sub(delta_ms.unsigned_abs())
        } else {
            self.current_ts.saturating_add(delta_ms as u64)
        };
        self.seek_target = Some(target.clamp(self.start_ts, self.end_ts));
    }

    fn take_seek_target(&mut self) -> Option<u64> {
        self.seek_target.take()
    }
}

fn handle_replay_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    replay: &mut ReplayState,
    replay_path: &PathBuf,
    pause_tx: &watch::Sender<bool>,
) -> Result<bool> {
    use crossterm::event::{KeyCode, KeyEventKind};

    if key.kind != KeyEventKind::Release {
        match key.code {
            KeyCode::Char('1') => replay.set_speed(1),
            KeyCode::Char('2') => replay.set_speed(2),
            KeyCode::Char('5') => replay.set_speed(5),
            KeyCode::Char('0') => replay.set_speed(10),
            KeyCode::Right
                if key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::SHIFT) =>
            {
                replay.seek_relative(60)
            }
            KeyCode::Left
                if key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::SHIFT) =>
            {
                replay.seek_relative(-60)
            }
            KeyCode::Right => replay.seek_relative(10),
            KeyCode::Left => replay.seek_relative(-10),
            _ => {
                let quit = ui::handle_key(app, key, pause_tx)?;
                update_replay_status(app, replay, replay_path);
                return Ok(quit);
            }
        }
        update_replay_status(app, replay, replay_path);
    }

    Ok(false)
}

fn rebuild_replay_to(app: &mut App, events: &[ProbeEvent], target_ts: u64) -> usize {
    app.reset_probe_state();
    let mut next_event_index = 0;
    for event in events {
        if event.ts_ms > target_ts {
            break;
        }
        app.apply_probe_event(event);
        next_event_index += 1;
    }
    next_event_index
}

fn update_replay_status(app: &mut App, replay: &ReplayState, replay_path: &PathBuf) {
    let state = if app.paused { "paused" } else { "replay" };
    app.status_line = format!(
        "{}: {} | speed: x{} | position: {}/{} | Left/Right +/-10s | Shift+Left/Right +/-60s | 1/2/5/0 speed",
        state,
        replay_path.display(),
        replay.speed(),
        format_duration_ms(replay.current_ts.saturating_sub(replay.start_ts)),
        format_duration_ms(replay.end_ts.saturating_sub(replay.start_ts)),
    );
}

fn format_duration_ms(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes:02}:{seconds:02}")
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

fn write_stats_csv(path: &PathBuf, app: &App) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("failed to open stats file {}", path.display()))?;

    writeln!(
        file,
        "host,ip,description,packets,responses,losses,loss_percent,started_at,ended_at,duration_ms,duration,downtime_count,downtime_ms,downtime,downtime_percent,downtime_periods,last_status,last_response"
    )?;
    for stat in &app.stats {
        write_stats_row(&mut file, stat)?;
    }
    file.flush()?;
    Ok(())
}

fn write_stats_from_record(
    replay_path: &PathBuf,
    stats_path: &PathBuf,
    app: &mut App,
) -> Result<()> {
    let mut reader = open_session_event_reader(replay_path)?;
    while let Some(event) = reader.read_next_event()? {
        app.apply_probe_event(&event);
    }
    write_stats_csv(stats_path, app)
}

fn resolve_stats_path(replay_path: &PathBuf, stats_path: Option<&PathBuf>) -> PathBuf {
    if let Some(path) = stats_path {
        return path.clone();
    }

    let mut path = replay_path.clone();
    let stem = replay_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("stats");
    path.set_file_name(format!("{stem}_stats.csv"));
    path
}

fn write_stats_row(file: &mut File, stat: &HostStats) -> Result<()> {
    let last_ts_ms = stat.last_ts_ms.unwrap_or(0);
    let open_down_ms = stat
        .down_since_ms
        .map(|down_since_ms| last_ts_ms.saturating_sub(down_since_ms))
        .unwrap_or(0);
    let downtime_ms = stat.total_down_ms.saturating_add(open_down_ms);
    let duration_ms = match (stat.first_ts_ms, stat.last_ts_ms) {
        (Some(start), Some(end)) => end.saturating_sub(start),
        _ => 0,
    };
    let downtime_percent = if duration_ms == 0 {
        0.0
    } else {
        ((downtime_ms as f64 / duration_ms as f64) * 10000.0).round() / 100.0
    };
    let mut periods = stat
        .downtime_periods
        .iter()
        .map(|(start, end)| format!("{}..{}", format_ts(*start), format_ts(*end)))
        .collect::<Vec<_>>();
    if let Some(down_since_ms) = stat.down_since_ms {
        periods.push(format!(
            "{}..{}",
            format_ts(down_since_ms),
            format_ts(last_ts_ms)
        ));
    }

    let fields = [
        stat.target.display.clone(),
        stat.last_resolved_ip.clone().unwrap_or_default(),
        stat.target.description.clone(),
        stat.sent_count.to_string(),
        stat.success_count.to_string(),
        stat.loss_count.to_string(),
        format!("{:.2}", stat.loss_percent),
        stat.first_ts_ms.map(format_ts).unwrap_or_default(),
        stat.last_ts_ms.map(format_ts).unwrap_or_default(),
        duration_ms.to_string(),
        format_duration_ms(duration_ms),
        stat.down_events.to_string(),
        downtime_ms.to_string(),
        format_duration_ms(downtime_ms),
        format!("{:.2}", downtime_percent),
        periods.join(";"),
        stat.last_status.clone(),
        stat.last_response.clone(),
    ];
    writeln!(
        file,
        "{}",
        fields
            .iter()
            .map(|field| csv_escape(field))
            .collect::<Vec<_>>()
            .join(",")
    )?;
    Ok(())
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn build_status_line(args: &Args, record_path: Option<&PathBuf>) -> String {
    let mut parts = Vec::new();
    if let Some(replay_path) = &args.replay {
        parts.push(format!("replay: {}", replay_path.display()));
    } else {
        parts.push("live".to_string());
    }
    if let Some(record_path) = record_path {
        parts.push(format!("record: {}", record_path.display()));
    }
    if let Some(log_path) = &args.log {
        parts.push(format!("log: {}", log_path.display()));
    }
    parts.join(" | ")
}

fn resolve_record_path(targets_path: &PathBuf, record_path: Option<&PathBuf>) -> PathBuf {
    if let Some(path) = record_path {
        return path.clone();
    }

    let stem = targets_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("pdeck");
    PathBuf::from(format!(
        "{}_{}.jsonl",
        stem,
        Local::now().format("%Y%m%d_%H%M%S")
    ))
}

fn format_ts(ts_ms: u64) -> String {
    match Local.timestamp_millis_opt(ts_ms as i64).single() {
        Some(dt) => dt.to_rfc3339(),
        None => String::new(),
    }
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

fn read_session_events(path: &PathBuf) -> Result<Vec<ProbeEvent>> {
    let mut reader = open_session_event_reader(path)?;
    let mut events = Vec::new();
    while let Some(event) = reader.read_next_event()? {
        events.push(event);
    }
    Ok(events)
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
    use std::path::{Path, PathBuf};

    use super::{
        parse_editor_command, read_session_events, read_session_header, resolve_record_path,
        resolve_stats_path, write_stats_from_record,
    };
    use crate::cli::{Args, DurationArg, IcmpBackendArg};
    use crate::model::App;

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

    #[test]
    fn derives_stats_path_from_replay_path() {
        assert_eq!(
            resolve_stats_path(&PathBuf::from("session.jsonl"), None),
            PathBuf::from("session_stats.csv")
        );
        assert_eq!(
            resolve_stats_path(&PathBuf::from("logs/session.json"), None),
            PathBuf::from("logs/session_stats.csv")
        );
        assert_eq!(
            resolve_stats_path(
                &PathBuf::from("session.jsonl"),
                Some(&PathBuf::from("custom.csv"))
            ),
            PathBuf::from("custom.csv")
        );
    }

    #[test]
    fn derives_record_path_from_targets_path() {
        let path = resolve_record_path(&PathBuf::from("office.txt"), None);
        let filename = path.file_name().and_then(|value| value.to_str()).unwrap();
        assert!(filename.starts_with("office_"));
        assert!(filename.ends_with(".jsonl"));

        assert_eq!(
            resolve_record_path(
                &PathBuf::from("office.txt"),
                Some(&PathBuf::from("custom.jsonl"))
            ),
            PathBuf::from("custom.jsonl")
        );
    }

    #[test]
    fn reads_cross_platform_replay_fixture() {
        let path = fixture_path("replay_cross_platform.jsonl");
        let targets = read_session_header(&path).unwrap();
        let events = read_session_events(&path).unwrap();

        assert_eq!(targets.len(), 2);
        assert_eq!(events.len(), 7);
        assert_eq!(events[0].response, "time=2.4 ms");
        assert_eq!(events[4].response, "Request timed out.");
        assert_eq!(events[6].response, "2.1ms");
        assert_eq!(events.iter().filter(|event| event.ok).count(), 4);
    }

    #[test]
    fn converts_cross_platform_replay_fixture_to_stats() {
        let replay_path = fixture_path("replay_cross_platform.jsonl");
        let stats_path = std::env::temp_dir().join(format!(
            "pdeck-cross-platform-stats-{}.csv",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&stats_path);

        let targets = read_session_header(&replay_path).unwrap();
        let mut app = App::new(test_args(replay_path.clone()), targets, "test".to_string());
        write_stats_from_record(&replay_path, &stats_path, &mut app).unwrap();
        let stats = std::fs::read_to_string(&stats_path).unwrap();
        let _ = std::fs::remove_file(&stats_path);

        assert!(stats.contains("router.local,192.168.1.1,gateway,4,2,2,50.00"));
        assert!(stats.contains("example.com:443,93.184.216.34,https tcp,3,2,1,33.33"));
        assert!(stats.contains(",7000,00:07,1,5000,00:05,71.43,"));
        assert!(stats.contains(",6000,00:06,1,3000,00:03,50.00,"));
    }

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn test_args(replay_path: PathBuf) -> Args {
        Args {
            interval: DurationArg(std::time::Duration::from_millis(500)),
            timeout: DurationArg(std::time::Duration::from_secs(3)),
            file: PathBuf::from("targets.txt"),
            arp_entries: false,
            vi_mode: false,
            concurrency: 16,
            icmp_backend: IcmpBackendArg::Auto,
            record: None,
            replay: Some(replay_path),
            log: None,
            stats: None,
        }
    }
}
