use std::net::IpAddr;

use anyhow::Result;
use chrono::{Local, TimeZone};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Block, Borders, Cell, Chart, Dataset, GraphType, Paragraph, Row, Table, Wrap,
};
use ratatui::{DefaultTerminal, Frame};
use tokio::sync::watch;

use crate::model::{App, HostStats, RepeatableAction, RttSample};

pub struct TerminalGuard {
    terminal: DefaultTerminal,
}

impl TerminalGuard {
    pub fn new() -> Result<Self> {
        Ok(Self {
            terminal: ratatui::init(),
        })
    }

    pub fn terminal(&mut self) -> &mut DefaultTerminal {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

pub fn draw_ui(frame: &mut Frame<'_>, app: &App) {
    let root = frame.area();
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(10),
            Constraint::Length(2),
        ])
        .split(root);

    let header = if app.args.replay.is_some() {
        Line::from(vec![
            Span::raw("Ctrl+S pause/resume  "),
            Span::raw("1/2/5/0 speed  "),
            Span::raw("Left/Right +/-10s  "),
            Span::raw("Shift+Left/Right +/-60s  "),
            Span::raw("q/Esc exit"),
        ])
    } else {
        Line::from(vec![
            Span::raw("Ctrl+S pause/resume  "),
            Span::raw("Up/Down scroll  "),
            Span::raw("d/D dead host  "),
            Span::raw("q/Esc exit"),
        ])
    };
    frame.render_widget(Paragraph::new(header), vertical[0]);

    let has_dead = app.stats.iter().any(|stat| stat.dead_now);
    if has_dead {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(32),
                Constraint::Percentage(26),
                Constraint::Percentage(22),
                Constraint::Percentage(20),
            ])
            .split(vertical[1]);

        draw_results(frame, body[0], app);
        draw_stats(frame, body[1], app);
        draw_dead_hosts(frame, body[2], app);
        draw_host_summary(frame, body[3], app);
    } else {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(42),
                Constraint::Percentage(30),
                Constraint::Percentage(28),
            ])
            .split(vertical[1]);

        draw_results(frame, body[0], app);
        draw_stats(frame, body[1], app);
        draw_host_summary(frame, body[2], app);
    }
    draw_detail(frame, vertical[2], app);

    let footer = Paragraph::new(app.status_line.as_str())
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::TOP));
    frame.render_widget(footer, vertical[3]);
}

pub fn handle_key(app: &mut App, key: KeyEvent, pause_tx: &watch::Sender<bool>) -> Result<bool> {
    if key.kind == KeyEventKind::Release {
        return Ok(false);
    }

    let is_repeat = key.kind == KeyEventKind::Repeat;

    let quit = match key.code {
        KeyCode::Esc | KeyCode::Char('q') => true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.paused = !app.paused;
            pause_tx.send(app.paused)?;
            app.status_line = if app.paused {
                "paused".to_string()
            } else {
                "running".to_string()
            };
            false
        }
        KeyCode::Up => {
            if app.should_accept_repeat(RepeatableAction::MoveUp, is_repeat) {
                app.selected_index = app.selected_index.saturating_sub(1);
            }
            false
        }
        KeyCode::Down => {
            if app.should_accept_repeat(RepeatableAction::MoveDown, is_repeat)
                && app.selected_index + 1 < app.stats.len()
            {
                app.selected_index += 1;
            }
            false
        }
        KeyCode::Char('d') => {
            if app.should_accept_repeat(RepeatableAction::NextDead, is_repeat) {
                app.select_next_dead();
            }
            false
        }
        KeyCode::Char('D') => {
            if app.should_accept_repeat(RepeatableAction::PreviousDead, is_repeat) {
                app.select_previous_dead();
            }
            false
        }
        _ => false,
    };
    Ok(quit)
}

fn draw_results(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let rows = app
        .results
        .iter()
        .rev()
        .take(area.height.saturating_sub(3) as usize)
        .map(|event| {
            let color = match event.status.as_str() {
                "o" | "200" => Color::Blue,
                _ => Color::Red,
            };
            Row::new(vec![
                Cell::from(event.status.clone()).style(Style::default().fg(color)),
                Cell::from(event.target.clone()).style(Style::default().fg(color)),
                Cell::from(event.response.clone()).style(Style::default().fg(color)),
            ])
        })
        .collect::<Vec<_>>();

    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Percentage(42),
            Constraint::Percentage(35),
        ],
    )
    .header(
        Row::new(vec!["St", "Host", "Response"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .title("Recent Results")
            .borders(Borders::ALL),
    );
    frame.render_widget(table, area);
}

fn draw_stats(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let visible_rows = area.height.saturating_sub(3) as usize;
    let scroll_offset = app
        .selected_index
        .saturating_sub(visible_rows.saturating_sub(1));
    let visible = app
        .stats
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_rows)
        .map(|(index, stat)| {
            let color = if stat.dead_now {
                Color::Red
            } else {
                Color::Green
            };
            let dead = if stat.dead_now { "Dead Now!" } else { "" };
            let base = if index == app.selected_index {
                Style::default().fg(Color::Black).bg(Color::Yellow)
            } else {
                Style::default().fg(color)
            };
            Row::new(vec![
                Cell::from(stat.target.display.clone()).style(base),
                Cell::from(format!("{:.2}", stat.loss_percent)).style(base),
                Cell::from(stat.loss_count.to_string()).style(base),
                Cell::from(dead).style(base),
            ])
        })
        .collect::<Vec<_>>();

    let title = if app.paused {
        "Per Host Loss [paused]"
    } else {
        "Per Host Loss"
    };
    let table = Table::new(
        visible,
        [
            Constraint::Percentage(42),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Percentage(30),
        ],
    )
    .header(
        Row::new(vec!["Hostname", "Loss%", "Loss", "State"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().title(title).borders(Borders::ALL));
    frame.render_widget(table, area);
}

fn draw_dead_hosts(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let visible_rows = area.height.saturating_sub(3) as usize;
    let dead_hosts = app
        .stats
        .iter()
        .enumerate()
        .filter(|(_, stat)| stat.dead_now)
        .take(visible_rows)
        .map(|(index, stat)| {
            let base = if index == app.selected_index {
                Style::default().fg(Color::Black).bg(Color::Red)
            } else {
                Style::default().fg(Color::Red)
            };
            let last = stat.last_error.as_deref().unwrap_or(&stat.last_response);
            Row::new(vec![
                Cell::from(stat.target.display.clone()).style(base),
                Cell::from(stat.target.description.clone()).style(base),
                Cell::from(stat.consecutive_failures.to_string()).style(base),
                Cell::from(last.to_string()).style(base),
            ])
        })
        .collect::<Vec<_>>();

    let rows = if dead_hosts.is_empty() {
        vec![Row::new(vec![
            Cell::from("-").style(Style::default().fg(Color::Green)),
            Cell::from("No dead hosts").style(Style::default().fg(Color::Green)),
            Cell::from("-").style(Style::default().fg(Color::Green)),
            Cell::from("-").style(Style::default().fg(Color::Green)),
        ])]
    } else {
        dead_hosts
    };

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(32),
            Constraint::Percentage(30),
            Constraint::Length(5),
            Constraint::Percentage(38),
        ],
    )
    .header(
        Row::new(vec!["Host", "Description", "Fail", "Last"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().title("Dead Hosts").borders(Borders::ALL));
    frame.render_widget(table, area);
}

fn draw_detail(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(stat) = app.selected_stat() else {
        return;
    };

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(32), Constraint::Percentage(68)])
        .split(area);

    draw_selected_results(frame, columns[0], app, stat);
    draw_rtt_graph(frame, columns[1], stat);
}

fn draw_rtt_graph(frame: &mut Frame<'_>, area: Rect, stat: &HostStats) {
    let samples = stat.recent_rtts.iter().cloned().collect::<Vec<_>>();
    let max_rtt = samples
        .iter()
        .filter_map(|value| value.rtt_ms)
        .fold(1.0_f64, f64::max)
        .ceil()
        .max(1.0);

    let success_segments = build_rtt_segments(&samples, false);
    let failure_segments = build_rtt_segments(&samples, true);
    let mut datasets = Vec::new();
    for segment in &success_segments {
        datasets.push(
            Dataset::default()
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::Cyan))
                .data(segment),
        );
    }
    for segment in &failure_segments {
        datasets.push(
            Dataset::default()
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::Red))
                .data(segment),
        );
    }

    let min_ts = samples.first().map(|sample| sample.ts_ms).unwrap_or(0) as f64;
    let max_ts = samples.last().map(|sample| sample.ts_ms).unwrap_or(1) as f64;
    let x_labels = vec![
        Span::raw(format_timestamp(min_ts as u64)),
        Span::raw(format_timestamp(max_ts as u64)),
    ];
    let y_labels = vec![
        Span::raw("0"),
        Span::raw(format!("{:.0}", max_rtt / 2.0)),
        Span::raw(format!("{:.0} ms", max_rtt)),
    ];

    let chart = Chart::new(datasets)
        .block(
            Block::default()
                .title(format!("RTT History: {}", stat.target.display))
                .borders(Borders::ALL),
        )
        .x_axis(
            Axis::default()
                .bounds([min_ts, max_ts.max(min_ts + 1.0)])
                .labels(x_labels),
        )
        .y_axis(Axis::default().bounds([0.0, max_rtt]).labels(y_labels));
    frame.render_widget(chart, area);
}

fn build_rtt_segments(samples: &[RttSample], failures: bool) -> Vec<Vec<(f64, f64)>> {
    let mut segments = Vec::new();
    let mut last_y = 0.0;

    for idx in 0..samples.len() {
        let current_is_failure = samples[idx].rtt_ms.is_none();
        let current_y = match samples[idx].rtt_ms {
            Some(rtt) => {
                last_y = rtt;
                rtt
            }
            None => last_y,
        };

        if current_is_failure != failures {
            continue;
        }

        let prev_x = samples[idx.saturating_sub(1)].ts_ms as f64;
        let prev_y = if idx == 0 {
            current_y
        } else {
            samples[idx - 1].rtt_ms.unwrap_or(current_y)
        };
        let current_x = samples[idx].ts_ms as f64;

        segments.push(vec![(prev_x, prev_y), (current_x, current_y)]);
    }

    segments
}

fn format_timestamp(ts_ms: u64) -> String {
    match Local.timestamp_millis_opt(ts_ms as i64).single() {
        Some(dt) => dt.format("%H:%M:%S").to_string(),
        None => "--:--:--".to_string(),
    }
}

fn draw_host_summary(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(stat) = app.selected_stat() else {
        return;
    };

    let last_error = stat.last_error.as_deref().unwrap_or("-");
    let resolved_ip = format_resolved_ip(stat.last_resolved_ip.as_deref());
    let text = vec![
        Line::from(format!("Host: {}", stat.target.display)),
        Line::from(format!("Resolved IP: {}", resolved_ip)),
        Line::from(format!("Description: {}", stat.target.description)),
        Line::from(format!("Last status: {}", stat.last_status)),
        Line::from(format!("Last response: {}", stat.last_response)),
        Line::from(format!(
            "Loss: {:.2}% ({})",
            stat.loss_percent, stat.loss_count
        )),
        Line::from(format!(
            "Consecutive failures: {}",
            stat.consecutive_failures
        )),
        Line::from(format!("Last error: {}", last_error)),
    ];
    let para = Paragraph::new(text).wrap(Wrap { trim: true }).block(
        Block::default()
            .title("Selected Host")
            .borders(Borders::ALL),
    );
    frame.render_widget(para, area);
}

fn format_resolved_ip(resolved_ip: Option<&str>) -> String {
    match resolved_ip.and_then(|ip| ip.parse::<IpAddr>().ok()) {
        Some(IpAddr::V4(ip)) => format!("{ip} (IPv4)"),
        Some(IpAddr::V6(ip)) => format!("{ip} (IPv6)"),
        None => "-".to_string(),
    }
}

fn draw_selected_results(frame: &mut Frame<'_>, area: Rect, app: &App, stat: &HostStats) {
    let rows = app
        .results
        .iter()
        .rev()
        .filter(|event| event.target == stat.target.display)
        .take(area.height.saturating_sub(3) as usize)
        .map(|event| {
            let color = match event.status.as_str() {
                "o" | "200" => Color::Blue,
                _ => Color::Red,
            };
            Row::new(vec![
                Cell::from(event.status.clone()).style(Style::default().fg(color)),
                Cell::from(event.response.clone()).style(Style::default().fg(color)),
            ])
        })
        .collect::<Vec<_>>();

    let table = Table::new(rows, [Constraint::Length(5), Constraint::Percentage(100)])
        .header(
            Row::new(vec!["St", "Response"]).style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(
            Block::default()
                .title(format!("Recent: {}", stat.target.display))
                .borders(Borders::ALL),
        );
    frame.render_widget(table, area);
}
