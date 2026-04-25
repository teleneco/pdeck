use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{Local, TimeZone};

use crate::cli::Args;
use crate::model::{App, HostStats};
use crate::record::{open_session_event_reader, read_session_header};

pub fn write_stats_from_record(replay_path: &Path, stats_path: &PathBuf) -> Result<()> {
    let targets = read_session_header(replay_path)?;
    let mut app = App::new(
        stats_args(replay_path.to_path_buf()),
        targets,
        "stats conversion".to_string(),
    );
    let mut reader = open_session_event_reader(replay_path)?;
    while let Some(event) = reader.read_next_event()? {
        app.apply_probe_event(&event);
    }
    write_stats_csv(stats_path, &app)
}

pub fn resolve_stats_path(replay_path: &Path, stats_path: Option<&PathBuf>) -> PathBuf {
    if let Some(path) = stats_path {
        return path.clone();
    }

    let mut path = replay_path.to_path_buf();
    let stem = replay_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("stats");
    path.set_file_name(format!("{stem}_stats.csv"));
    path
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
        "host,ip,description,packets,responses,losses,loss_percent,rtt_min_ms,rtt_avg_ms,rtt_max_ms,rtt_stddev_ms,started_at,ended_at,duration_ms,duration,downtime_count,downtime_ms,downtime,downtime_percent,downtime_periods,last_status,last_response"
    )?;
    for stat in &app.stats {
        write_stats_row(&mut file, stat)?;
    }
    file.flush()?;
    Ok(())
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
    let rtt_avg_ms = if stat.rtt_count == 0 {
        None
    } else {
        Some(stat.rtt_sum_ms / stat.rtt_count as f64)
    };
    let rtt_stddev_ms = rtt_avg_ms.map(|avg| {
        let variance = (stat.rtt_sum_squares_ms / stat.rtt_count as f64) - (avg * avg);
        variance.max(0.0).sqrt()
    });
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
        format_optional_ms(stat.rtt_min_ms),
        format_optional_ms(rtt_avg_ms),
        format_optional_ms(stat.rtt_max_ms),
        format_optional_ms(rtt_stddev_ms),
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

fn format_duration_ms(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes:02}:{seconds:02}")
}

fn format_optional_ms(value: Option<f64>) -> String {
    value.map(|value| format!("{value:.3}")).unwrap_or_default()
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn format_ts(ts_ms: u64) -> String {
    match Local.timestamp_millis_opt(ts_ms as i64).single() {
        Some(dt) => dt.to_rfc3339(),
        None => String::new(),
    }
}

fn stats_args(replay_path: PathBuf) -> Args {
    Args {
        command: None,
        interval: crate::cli::DurationArg(std::time::Duration::from_millis(500)),
        timeout: crate::cli::DurationArg(std::time::Duration::from_secs(3)),
        file: PathBuf::from("targets.txt"),
        arp_entries: false,
        concurrency: 16,
        icmp_backend: crate::cli::IcmpBackendArg::Auto,
        record: None,
        record_overwrite: false,
        record_size_limit: 0,
        no_tui: false,
        replay: Some(replay_path),
        log: None,
        stats: None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{resolve_stats_path, write_stats_from_record};

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
    fn converts_cross_platform_replay_fixture_to_stats() {
        let replay_path = fixture_path("replay_cross_platform.jsonl");
        let stats_path = std::env::temp_dir().join(format!(
            "pdeck-cross-platform-stats-{}.csv",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&stats_path);

        write_stats_from_record(&replay_path, &stats_path).unwrap();
        let stats = std::fs::read_to_string(&stats_path).unwrap();
        let _ = std::fs::remove_file(&stats_path);

        assert!(stats.contains("router.local,192.168.1.1,gateway,4,2,2,50.00"));
        assert!(stats.contains("example.com:443,93.184.216.34,https tcp,3,2,1,33.33"));
        assert!(stats.contains(",2.100,2.250,2.400,0.150,"));
        assert!(stats.contains(",18.000,19.500,21.000,1.500,"));
        assert!(stats.contains(",7000,00:07,1,5000,00:05,71.43,"));
        assert!(stats.contains(",6000,00:06,1,3000,00:03,50.00,"));
    }

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }
}
