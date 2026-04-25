use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::model::{ProbeEvent, Target};

#[derive(Serialize, Deserialize)]
struct SessionHeader {
    version: u8,
    targets: Vec<Target>,
}

pub struct SessionEventReader {
    reader: BufReader<File>,
}

pub fn init_record_file(path: &PathBuf, targets: &[Target]) -> Result<File> {
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

pub fn append_record_event(file: &mut File, event: &ProbeEvent) -> Result<()> {
    writeln!(file, "{}", serde_json::to_string(event)?)?;
    file.flush()?;
    Ok(())
}

pub fn read_session_header(path: &PathBuf) -> Result<Vec<Target>> {
    let file = File::open(path)
        .with_context(|| format!("failed to open replay file {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    let header: SessionHeader = serde_json::from_str(first_line.trim_end())?;
    Ok(header.targets)
}

pub fn open_session_event_reader(path: &PathBuf) -> Result<SessionEventReader> {
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

pub fn read_session_events(path: &PathBuf) -> Result<Vec<ProbeEvent>> {
    let mut reader = open_session_event_reader(path)?;
    let mut events = Vec::new();
    while let Some(event) = reader.read_next_event()? {
        events.push(event);
    }
    Ok(events)
}

impl SessionEventReader {
    pub fn read_next_event(&mut self) -> Result<Option<ProbeEvent>> {
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

    use super::{read_session_events, read_session_header};

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

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }
}
