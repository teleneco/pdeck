use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use crossterm::event::{self, Event};
use tokio::sync::watch;

use crate::log::append_text_log_event;
use crate::model::{App, ProbeEvent};
use crate::record::read_session_events;
use crate::ui;

const REPLAY_SPEEDS: [u64; 4] = [1, 2, 5, 10];
const REPLAY_UI_TICK_MS: u64 = 33;

pub async fn run_replay(
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
        if event::poll(Duration::from_millis(1))?
            && let Event::Key(key) = event::read()?
            && handle_replay_key(app, key, &mut replay, replay_path, &pause_tx)?
        {
            return Ok(());
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
    replay_path: &Path,
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

fn update_replay_status(app: &mut App, replay: &ReplayState, replay_path: &Path) {
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
