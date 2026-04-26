use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::model::ProbeEvent;
use crate::record::{SessionReadMode, read_session_events_with_mode};

pub fn init_text_log_file(path: &PathBuf) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("failed to open log file {}", path.display()))
}

pub fn append_text_log_event(file: &mut File, event: &ProbeEvent) -> Result<()> {
    file.write_all(event.log_line.as_bytes())?;
    file.flush()?;
    Ok(())
}

pub fn write_log_from_record(
    replay_path: &Path,
    log_path: &PathBuf,
    mode: SessionReadMode,
) -> Result<()> {
    let session = read_session_events_with_mode(replay_path, mode)?;
    let mut file = init_text_log_file(log_path)?;
    for event in session.events {
        append_text_log_event(&mut file, &event.event)?;
    }
    Ok(())
}

pub fn resolve_log_path(
    replay_path: &Path,
    log_path: Option<&PathBuf>,
    mode: SessionReadMode,
) -> PathBuf {
    if let Some(path) = log_path {
        return path.clone();
    }

    let mut path = replay_path.to_path_buf();
    let stem = replay_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("log");
    let stem = match mode {
        SessionReadMode::Auto => strip_part_suffix(stem),
        SessionReadMode::Only => stem,
    };
    path.set_file_name(format!("{stem}.log"));
    path
}

fn strip_part_suffix(stem: &str) -> &str {
    let Some((base, part)) = stem.rsplit_once("_part") else {
        return stem;
    };
    if part.len() == 4 && part.chars().all(|value| value.is_ascii_digit()) {
        base
    } else {
        stem
    }
}
