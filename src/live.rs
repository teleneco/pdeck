use std::fs::File;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event};
use tokio::sync::{mpsc, watch};

use crate::log::append_text_log_event;
use crate::model::App;
use crate::probe;
use crate::record::append_record_event;
use crate::ui;

pub async fn run_app(
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
