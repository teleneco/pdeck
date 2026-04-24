use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::cli::Args;

const MAX_RESULTS: usize = 512;
const MAX_RTT_HISTORY: usize = 60;
const KEY_REPEAT_INTERVAL: Duration = Duration::from_millis(140);

#[derive(Clone, Debug)]
pub struct RttSample {
    pub ts_ms: u64,
    pub rtt_ms: Option<f64>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TargetKind {
    Icmp,
    Tcp { port: u16 },
    Http { use_tls: bool },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Target {
    pub display: String,
    pub host: String,
    pub kind: TargetKind,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct HostStats {
    pub target: Target,
    pub last_resolved_ip: Option<String>,
    pub loss_count: u64,
    pub sent_count: u64,
    pub success_count: u64,
    pub loss_percent: f64,
    pub rtt_count: u64,
    pub rtt_min_ms: Option<f64>,
    pub rtt_max_ms: Option<f64>,
    pub rtt_sum_ms: f64,
    pub rtt_sum_squares_ms: f64,
    pub dead_now: bool,
    pub last_status: String,
    pub last_response: String,
    pub last_error: Option<String>,
    pub consecutive_failures: u64,
    pub recent_rtts: VecDeque<RttSample>,
    pub first_ts_ms: Option<u64>,
    pub last_ts_ms: Option<u64>,
    pub total_down_ms: u64,
    pub down_events: u64,
    pub down_since_ms: Option<u64>,
    pub downtime_periods: Vec<(u64, u64)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProbeEvent {
    pub index: usize,
    pub status: String,
    pub target: String,
    #[serde(default)]
    pub resolved_ip: Option<String>,
    pub response: String,
    pub log_line: String,
    pub ok: bool,
    pub rtt_ms: Option<f64>,
    pub ts_ms: u64,
}

#[derive(Debug)]
pub struct App {
    pub args: Args,
    pub targets: Vec<Target>,
    pub stats: Vec<HostStats>,
    pub results: VecDeque<ProbeEvent>,
    pub paused: bool,
    pub selected_index: usize,
    pub status_line: String,
    key_repeat_state: Option<KeyRepeatState>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RepeatableAction {
    MoveUp,
    MoveDown,
    NextDead,
    PreviousDead,
}

#[derive(Debug)]
struct KeyRepeatState {
    action: RepeatableAction,
    accepted_at: Instant,
}

impl App {
    pub fn new(args: Args, targets: Vec<Target>, status_line: String) -> Self {
        let stats = build_host_stats(&targets);

        Self {
            args: args.clone(),
            targets,
            stats,
            results: VecDeque::with_capacity(MAX_RESULTS),
            paused: false,
            selected_index: 0,
            status_line,
            key_repeat_state: None,
        }
    }

    pub fn reset_probe_state(&mut self) {
        self.stats = build_host_stats(&self.targets);
        self.results.clear();
        self.selected_index = self.selected_index.min(self.stats.len().saturating_sub(1));
        self.key_repeat_state = None;
    }

    pub fn apply_probe_event(&mut self, event: &ProbeEvent) {
        if let Some(stat) = self.stats.get_mut(event.index) {
            stat.first_ts_ms.get_or_insert(event.ts_ms);
            stat.last_ts_ms = Some(event.ts_ms);
            stat.sent_count += 1;
            if event.ok {
                stat.success_count += 1;
            } else {
                stat.loss_count += 1;
            }
            if let Some(rtt_ms) = event.rtt_ms {
                stat.rtt_count += 1;
                stat.rtt_min_ms = Some(stat.rtt_min_ms.map_or(rtt_ms, |value| value.min(rtt_ms)));
                stat.rtt_max_ms = Some(stat.rtt_max_ms.map_or(rtt_ms, |value| value.max(rtt_ms)));
                stat.rtt_sum_ms += rtt_ms;
                stat.rtt_sum_squares_ms += rtt_ms * rtt_ms;
            }
            stat.loss_percent = if stat.sent_count == 0 {
                0.0
            } else {
                ((stat.loss_count as f64 / stat.sent_count as f64) * 10000.0).round() / 100.0
            };
            if event.ok {
                if let Some(down_since_ms) = stat.down_since_ms.take() {
                    stat.total_down_ms = stat
                        .total_down_ms
                        .saturating_add(event.ts_ms.saturating_sub(down_since_ms));
                    stat.downtime_periods.push((down_since_ms, event.ts_ms));
                }
            } else if stat.down_since_ms.is_none() {
                stat.down_since_ms = Some(event.ts_ms);
                stat.down_events += 1;
            }
            stat.dead_now = !event.ok;
            stat.last_status = event.status.clone();
            stat.last_response = event.response.clone();
            if let Some(resolved_ip) = &event.resolved_ip {
                stat.last_resolved_ip = Some(resolved_ip.clone());
            }
            stat.last_error = if event.ok {
                None
            } else {
                Some(event.response.clone())
            };
            stat.consecutive_failures = if event.ok {
                0
            } else {
                stat.consecutive_failures + 1
            };
            if stat.recent_rtts.len() >= MAX_RTT_HISTORY {
                stat.recent_rtts.pop_front();
            }
            stat.recent_rtts.push_back(RttSample {
                ts_ms: event.ts_ms,
                rtt_ms: event.rtt_ms,
            });
        }

        if self.results.len() >= MAX_RESULTS {
            self.results.pop_front();
        }
        self.results.push_back(event.clone());
    }

    pub fn selected_stat(&self) -> Option<&HostStats> {
        self.stats.get(self.selected_index)
    }

    pub fn select_next_dead(&mut self) {
        if self.stats.is_empty() {
            return;
        }

        let len = self.stats.len();
        for offset in 1..=len {
            let index = (self.selected_index + offset) % len;
            if self.stats[index].dead_now {
                self.selected_index = index;
                return;
            }
        }
    }

    pub fn select_previous_dead(&mut self) {
        if self.stats.is_empty() {
            return;
        }

        let len = self.stats.len();
        for offset in 1..=len {
            let index = (self.selected_index + len - offset) % len;
            if self.stats[index].dead_now {
                self.selected_index = index;
                return;
            }
        }
    }

    pub fn should_accept_repeat(&mut self, action: RepeatableAction, is_repeat: bool) -> bool {
        let now = Instant::now();
        if !is_repeat {
            self.key_repeat_state = Some(KeyRepeatState {
                action,
                accepted_at: now,
            });
            return true;
        }

        if let Some(state) = &mut self.key_repeat_state {
            if state.action == action && now.duration_since(state.accepted_at) < KEY_REPEAT_INTERVAL
            {
                return false;
            }

            state.action = action;
            state.accepted_at = now;
            return true;
        }

        self.key_repeat_state = Some(KeyRepeatState {
            action,
            accepted_at: now,
        });
        true
    }
}

fn build_host_stats(targets: &[Target]) -> Vec<HostStats> {
    targets
        .iter()
        .cloned()
        .map(|target| HostStats {
            target,
            last_resolved_ip: None,
            loss_count: 0,
            sent_count: 0,
            success_count: 0,
            loss_percent: 0.0,
            rtt_count: 0,
            rtt_min_ms: None,
            rtt_max_ms: None,
            rtt_sum_ms: 0.0,
            rtt_sum_squares_ms: 0.0,
            dead_now: false,
            last_status: "-".to_string(),
            last_response: "-".to_string(),
            last_error: None,
            consecutive_failures: 0,
            recent_rtts: VecDeque::with_capacity(MAX_RTT_HISTORY),
            first_ts_ms: None,
            last_ts_ms: None,
            total_down_ms: 0,
            down_events: 0,
            down_since_ms: None,
            downtime_periods: Vec::new(),
        })
        .collect::<Vec<_>>()
}
