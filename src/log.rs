use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::model::ProbeEvent;
use crate::record::open_session_event_reader;

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

pub fn write_log_from_record(replay_path: &PathBuf, log_path: &PathBuf) -> Result<()> {
    let mut reader = open_session_event_reader(replay_path)?;
    let mut file = init_text_log_file(log_path)?;
    while let Some(event) = reader.read_next_event()? {
        append_text_log_event(&mut file, &event)?;
    }
    Ok(())
}

pub fn resolve_log_path(replay_path: &Path, log_path: Option<&PathBuf>) -> PathBuf {
    if let Some(path) = log_path {
        return path.clone();
    }

    let mut path = replay_path.to_path_buf();
    let stem = replay_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("log");
    path.set_file_name(format!("{stem}.log"));
    path
}
