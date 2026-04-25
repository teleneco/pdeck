use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

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

#[derive(Debug)]
pub struct RecordWriter {
    file: File,
    path: PathBuf,
    written_bytes: u64,
    size_limit_bytes: u64,
    limit_reached: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RecordWriteStatus {
    Written,
    LimitReached,
    AlreadyStopped,
}

pub fn init_record_file(
    path: &Path,
    targets: &[Target],
    overwrite: bool,
    avoid_collisions: bool,
    size_limit_bytes: u64,
) -> Result<RecordWriter> {
    if overwrite {
        return create_record_file(path, targets, true, size_limit_bytes);
    }

    let mut candidate = path.to_path_buf();
    for index in 1.. {
        match create_record_file(&candidate, targets, false, size_limit_bytes) {
            Ok(writer) => return Ok(writer),
            Err(err)
                if avoid_collisions
                    && err
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|io_err| io_err.kind() == ErrorKind::AlreadyExists) =>
            {
                candidate = suffixed_path(path, index + 1);
            }
            Err(err) => return Err(err),
        }
    }

    unreachable!("unbounded record file creation loop should always return");
}

impl RecordWriter {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn create_record_file(
    path: &Path,
    targets: &[Target],
    overwrite: bool,
    size_limit_bytes: u64,
) -> Result<RecordWriter> {
    let mut options = OpenOptions::new();
    options.write(true);
    harden_record_open_options(&mut options);
    if overwrite {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }

    let mut file = options.open(path)?;
    let header = SessionHeader {
        version: 1,
        targets: targets.to_vec(),
    };
    let header = format!("{}\n", serde_json::to_string(&header)?);
    file.write_all(header.as_bytes())?;
    file.flush()?;
    Ok(RecordWriter {
        file,
        path: path.to_path_buf(),
        written_bytes: header.len() as u64,
        size_limit_bytes,
        limit_reached: false,
    })
}

fn suffixed_path(path: &Path, index: usize) -> PathBuf {
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("pdeck");
    let extension = path.extension().and_then(|value| value.to_str());
    let filename = match extension {
        Some(extension) => format!("{stem}_{index}.{extension}"),
        None => format!("{stem}_{index}"),
    };
    parent.join(filename)
}

fn harden_record_open_options(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
}

pub fn append_record_event(
    writer: &mut RecordWriter,
    event: &ProbeEvent,
) -> Result<RecordWriteStatus> {
    if writer.limit_reached {
        return Ok(RecordWriteStatus::AlreadyStopped);
    }

    let line = format!("{}\n", serde_json::to_string(event)?);
    let line_len = line.len() as u64;
    if writer.size_limit_bytes > 0
        && writer.written_bytes.saturating_add(line_len) > writer.size_limit_bytes
    {
        writer.limit_reached = true;
        writer.file.flush()?;
        return Ok(RecordWriteStatus::LimitReached);
    }

    writer.file.write_all(line.as_bytes())?;
    writer.file.flush()?;
    writer.written_bytes = writer.written_bytes.saturating_add(line_len);
    Ok(RecordWriteStatus::Written)
}

pub fn read_session_header(path: &Path) -> Result<Vec<Target>> {
    let file = File::open(path)
        .with_context(|| format!("failed to open replay file {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    if first_line.trim().is_empty() {
        bail!("replay header is empty");
    }
    let header: SessionHeader = serde_json::from_str(first_line.trim_end())?;
    Ok(header.targets)
}

pub fn open_session_event_reader(path: &Path) -> Result<SessionEventReader> {
    let file = File::open(path)
        .with_context(|| format!("failed to open replay file {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    if first_line.trim().is_empty() {
        bail!("replay header is empty");
    }
    let _: SessionHeader = serde_json::from_str(first_line.trim_end())
        .with_context(|| format!("failed to parse replay header in {}", path.display()))?;
    Ok(SessionEventReader { reader })
}

pub fn read_session_events(path: &Path) -> Result<Vec<ProbeEvent>> {
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
            match serde_json::from_str::<ProbeEvent>(line.trim_end()) {
                Ok(event) => return Ok(Some(event)),
                Err(_) => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::model::{ProbeEvent, Target, TargetKind};

    use super::{
        RecordWriteStatus, append_record_event, init_record_file, read_session_events,
        read_session_header,
    };

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
    fn skips_blank_and_malformed_event_lines() {
        let path =
            std::env::temp_dir().join(format!("pdeck-damaged-jsonl-{}.jsonl", std::process::id()));
        std::fs::write(
            &path,
            concat!(
                "{\"version\":1,\"targets\":[{\"display\":\"example.com\",\"host\":\"example.com\",\"kind\":\"Icmp\",\"description\":\"web\"}]}\n",
                "\n",
                "not-json\n",
                "{\"index\":0,\"status\":\"o\",\"target\":\"example.com\",\"resolved_ip\":null,\"response\":\"ok\",\"log_line\":\"ok\\n\",\"ok\":true,\"rtt_ms\":1.0,\"ts_ms\":1}\n",
                "{\"index\":\"bad\"}\n",
                "{\"index\":0,\"status\":\"x\",\"target\":\"example.com\",\"resolved_ip\":null,\"response\":\"timeout\",\"log_line\":\"timeout\\n\",\"ok\":false,\"rtt_ms\":null,\"ts_ms\":2}\n",
            ),
        )
        .unwrap();

        let events = read_session_events(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(events.len(), 2);
        assert!(events[0].ok);
        assert!(!events[1].ok);
    }

    #[test]
    fn refuses_to_replace_existing_record_without_overwrite() {
        let path = std::env::temp_dir().join(format!(
            "pdeck-existing-record-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(&path, "existing").unwrap();

        let err = init_record_file(&path, &sample_targets(), false, false, 0).unwrap_err();
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            err.downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::AlreadyExists)
        );
    }

    #[test]
    fn auto_record_creation_uses_atomic_suffix_on_collision() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("office.jsonl");
        let second = dir.path().join("office_2.jsonl");
        std::fs::write(&first, "existing").unwrap();

        let writer = init_record_file(&first, &sample_targets(), false, true, 0).unwrap();

        assert_eq!(writer.path(), second);
    }

    #[test]
    fn stops_recording_events_when_size_limit_is_reached() {
        let path =
            std::env::temp_dir().join(format!("pdeck-limited-record-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut writer = init_record_file(&path, &sample_targets(), false, false, 130).unwrap();

        let status = append_record_event(&mut writer, &sample_event()).unwrap();
        let stopped = append_record_event(&mut writer, &sample_event()).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(status, RecordWriteStatus::LimitReached);
        assert_eq!(stopped, RecordWriteStatus::AlreadyStopped);
    }

    #[test]
    fn rejects_malformed_session_header() {
        let path =
            std::env::temp_dir().join(format!("pdeck-bad-header-{}.jsonl", std::process::id()));
        std::fs::write(
            &path,
            concat!(
                "not-json\n",
                "{\"index\":0,\"status\":\"o\",\"target\":\"example.com\",\"resolved_ip\":null,\"response\":\"ok\",\"log_line\":\"ok\\n\",\"ok\":true,\"rtt_ms\":1.0,\"ts_ms\":1}\n",
            ),
        )
        .unwrap();

        let err = read_session_events(&path).unwrap_err();
        let _ = std::fs::remove_file(&path);

        assert!(err.to_string().contains("failed to parse replay header"));
    }

    #[cfg(unix)]
    #[test]
    fn record_overwrite_refuses_symlink_targets() {
        let dir = tempfile::tempdir().unwrap();
        let real_path = dir.path().join("real.jsonl");
        let link_path = dir.path().join("link.jsonl");
        std::fs::write(&real_path, "existing").unwrap();
        std::os::unix::fs::symlink(&real_path, &link_path).unwrap();

        let err = init_record_file(&link_path, &sample_targets(), true, false, 0).unwrap_err();

        assert_eq!(
            err.downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::raw_os_error),
            Some(libc::ELOOP)
        );
    }

    fn sample_targets() -> Vec<Target> {
        vec![Target {
            display: "example.com".to_string(),
            host: "example.com".to_string(),
            kind: TargetKind::Icmp,
            description: "web".to_string(),
        }]
    }

    fn sample_event() -> ProbeEvent {
        ProbeEvent {
            index: 0,
            status: "o".to_string(),
            target: "example.com".to_string(),
            resolved_ip: None,
            response: "ok".to_string(),
            log_line: "ok\n".to_string(),
            ok: true,
            rtt_ms: Some(1.0),
            ts_ms: 1,
        }
    }

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }
}
