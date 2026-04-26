use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{ProbeEvent, Target};

#[derive(Serialize, Deserialize)]
struct SessionHeader {
    version: u8,
    targets: Vec<Target>,
}

#[derive(Clone, Debug)]
pub struct SessionData {
    pub targets: Vec<Target>,
    pub events: Vec<RecordedEvent>,
}

#[derive(Clone, Debug)]
pub struct RecordedEvent {
    pub event: ProbeEvent,
    pub source_path: PathBuf,
    pub part: Option<u32>,
    pub part_count: Option<usize>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SessionReadMode {
    Auto,
    Only,
}

#[derive(Debug)]
pub struct RecordWriter {
    file: File,
    path: PathBuf,
    base_path: PathBuf,
    session_id: String,
    targets: Vec<Target>,
    part: u32,
    written_bytes: u64,
    size_limit_bytes: u64,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RecordWriteStatus {
    Written,
    Rotated,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct V2MetaRecord {
    format_version: u8,
    record_type: String,
    session_id: String,
    part: u32,
    file_started_at: String,
    targets: Vec<Target>,
}

#[derive(Debug, Deserialize, Serialize)]
struct V2ProbeRecord {
    format_version: u8,
    record_type: String,
    session_id: String,
    event: ProbeEvent,
}

pub fn init_record_file(
    path: &Path,
    targets: &[Target],
    overwrite: bool,
    avoid_collisions: bool,
    size_limit_bytes: u64,
) -> Result<RecordWriter> {
    if overwrite {
        bail!("--record-overwrite is not supported for rotated v2 records");
    }

    let mut candidate = path.to_path_buf();
    for index in 1.. {
        match create_record_file(&candidate, targets, size_limit_bytes) {
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
    size_limit_bytes: u64,
) -> Result<RecordWriter> {
    ensure_parent_exists(path)?;
    ensure_record_path_available(path)?;
    let session_id = new_session_id();
    create_record_part(path, targets, &session_id, 1, size_limit_bytes)
}

fn create_record_part(
    path: &Path,
    targets: &[Target],
    session_id: &str,
    part: u32,
    size_limit_bytes: u64,
) -> Result<RecordWriter> {
    let mut options = OpenOptions::new();
    options.write(true);
    harden_record_open_options(&mut options);
    options.create_new(true);

    let mut file = options.open(path)?;
    let header = V2MetaRecord {
        format_version: 2,
        record_type: "meta".to_string(),
        session_id: session_id.to_string(),
        part,
        file_started_at: Utc::now().to_rfc3339(),
        targets: targets.to_vec(),
    };
    let header = format!("{}\n", serde_json::to_string(&header)?);
    if size_limit_bytes > 0 && header.len() as u64 >= size_limit_bytes {
        bail!("record size limit is too small for the v2 metadata header");
    }
    file.write_all(header.as_bytes())?;
    file.flush()?;
    Ok(RecordWriter {
        file,
        path: path.to_path_buf(),
        base_path: if part == 1 {
            path.to_path_buf()
        } else {
            base_path_from_part(path, part).unwrap_or_else(|| path.to_path_buf())
        },
        session_id: session_id.to_string(),
        targets: targets.to_vec(),
        part,
        written_bytes: header.len() as u64,
        size_limit_bytes,
    })
}

fn ensure_parent_exists(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        bail!(
            "record output directory does not exist: {}",
            parent.display()
        );
    }
    Ok(())
}

fn ensure_record_path_available(path: &Path) -> Result<()> {
    if path.exists() {
        return Err(std::io::Error::new(
            ErrorKind::AlreadyExists,
            format!("record output already exists: {}", path.display()),
        )
        .into());
    }
    let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) else {
        return ensure_no_rotated_conflicts(Path::new("."), path);
    };
    ensure_no_rotated_conflicts(parent, path)
}

fn ensure_no_rotated_conflicts(parent: &Path, path: &Path) -> Result<()> {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("pdeck");
    let extension = path.extension().and_then(|value| value.to_str());
    let prefix = format!("{stem}_part");
    for entry in fs::read_dir(parent)
        .with_context(|| format!("failed to scan record directory {}", parent.display()))?
    {
        let entry = entry?;
        let entry_path = entry.path();
        if entry_path == path {
            return Err(std::io::Error::new(
                ErrorKind::AlreadyExists,
                format!("record output already exists: {}", path.display()),
            )
            .into());
        }
        let Some(name) = entry_path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if !name.starts_with(&prefix) {
            continue;
        }
        if entry_path.extension().and_then(|value| value.to_str()) == extension {
            return Err(std::io::Error::new(
                ErrorKind::AlreadyExists,
                format!(
                    "rotated record output already exists: {}",
                    entry_path.display()
                ),
            )
            .into());
        }
    }
    Ok(())
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
    let record = V2ProbeRecord {
        format_version: 2,
        record_type: "probe".to_string(),
        session_id: writer.session_id.clone(),
        event: event.clone(),
    };
    let line = format!("{}\n", serde_json::to_string(&record)?);
    let line_len = line.len() as u64;
    if writer.size_limit_bytes > 0
        && writer.written_bytes.saturating_add(line_len) > writer.size_limit_bytes
    {
        writer.file.flush()?;
        rotate_record_file(writer)?;
        if writer.size_limit_bytes > 0
            && writer.written_bytes.saturating_add(line_len) > writer.size_limit_bytes
        {
            bail!("record event is too large for the configured record size limit");
        }
        writer.file.write_all(line.as_bytes())?;
        writer.file.flush()?;
        writer.written_bytes = writer.written_bytes.saturating_add(line_len);
        return Ok(RecordWriteStatus::Rotated);
    }

    writer.file.write_all(line.as_bytes())?;
    writer.file.flush()?;
    writer.written_bytes = writer.written_bytes.saturating_add(line_len);
    Ok(RecordWriteStatus::Written)
}

fn rotate_record_file(writer: &mut RecordWriter) -> Result<()> {
    let next_part = writer.part.saturating_add(1);
    let next_path = part_path(&writer.base_path, next_part);
    ensure_parent_exists(&next_path)?;
    let next = create_record_part(
        &next_path,
        &writer.targets,
        &writer.session_id,
        next_part,
        writer.size_limit_bytes,
    )?;
    *writer = next;
    Ok(())
}

pub fn read_session_events_with_mode(path: &Path, mode: SessionReadMode) -> Result<SessionData> {
    read_session(path, mode)
}

fn read_session(path: &Path, mode: SessionReadMode) -> Result<SessionData> {
    match read_first_record(path)? {
        FirstRecord::V1(header) => read_v1_session(path, header),
        FirstRecord::V2(meta) => read_v2_session(path, meta, mode),
    }
}

enum FirstRecord {
    V1(SessionHeader),
    V2(V2MetaRecord),
}

fn read_first_record(path: &Path) -> Result<FirstRecord> {
    let first_line = read_first_line(path)?
        .with_context(|| format!("replay header is empty in {}", path.display()))?;
    parse_first_record(&first_line)
        .with_context(|| format!("failed to parse replay header in {}", path.display()))
}

fn read_first_line(path: &Path) -> Result<Option<String>> {
    let file = File::open(path)
        .with_context(|| format!("failed to open replay file {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    let read = reader.read_line(&mut first_line)?;
    if read == 0 || first_line.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(first_line))
}

fn parse_first_record(line: &str) -> Result<FirstRecord> {
    let value: Value = serde_json::from_str(line.trim_end())?;
    if value.get("format_version").and_then(Value::as_u64) == Some(2)
        && value.get("record_type").and_then(Value::as_str) == Some("meta")
    {
        return Ok(FirstRecord::V2(serde_json::from_value(value)?));
    }
    Ok(FirstRecord::V1(serde_json::from_value(value)?))
}

fn read_v1_session(path: &Path, header: SessionHeader) -> Result<SessionData> {
    let mut reader = open_v1_event_reader(path)?;
    let mut events = Vec::new();
    while let Some(event) = read_next_v1_event(&mut reader)? {
        events.push(RecordedEvent {
            event,
            source_path: path.to_path_buf(),
            part: None,
            part_count: Some(1),
        });
    }
    Ok(SessionData {
        targets: header.targets,
        events,
    })
}

fn open_v1_event_reader(path: &Path) -> Result<BufReader<File>> {
    let file = File::open(path)
        .with_context(|| format!("failed to open replay file {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    Ok(reader)
}

fn read_next_v1_event(reader: &mut BufReader<File>) -> Result<Option<ProbeEvent>> {
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
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

fn read_v2_session(path: &Path, meta: V2MetaRecord, mode: SessionReadMode) -> Result<SessionData> {
    let mut files = match mode {
        SessionReadMode::Auto => discover_v2_session_files(path, &meta)?,
        SessionReadMode::Only => vec![(path.to_path_buf(), meta)],
    };
    files.sort_by_key(|(_, meta)| meta.part);
    if mode == SessionReadMode::Auto {
        validate_v2_parts(&files)?;
    }
    let part_count = match mode {
        SessionReadMode::Auto => Some(files.len()),
        SessionReadMode::Only => None,
    };
    let targets = files
        .first()
        .map(|(_, meta)| meta.targets.clone())
        .unwrap_or_default();
    let session_id = files
        .first()
        .map(|(_, meta)| meta.session_id.clone())
        .unwrap_or_default();
    let mut events = Vec::new();
    for (source_path, file_meta) in files {
        if file_meta.session_id != session_id {
            bail!("record session_id mismatch in {}", source_path.display());
        }
        if file_meta.targets != targets {
            bail!("record targets mismatch in {}", source_path.display());
        }
        read_v2_events_from_file(&source_path, &file_meta, part_count, &mut events)?;
    }
    Ok(SessionData { targets, events })
}

fn discover_v2_session_files(
    path: &Path,
    origin: &V2MetaRecord,
) -> Result<Vec<(PathBuf, V2MetaRecord)>> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let origin_extension = path.extension().and_then(|value| value.to_str());
    let mut files = Vec::new();
    for entry in fs::read_dir(parent)
        .with_context(|| format!("failed to scan replay directory {}", parent.display()))?
    {
        let entry = entry?;
        let entry_path = entry.path();
        let entry_extension = entry_path.extension().and_then(|value| value.to_str());
        if entry_extension != Some("jsonl") && entry_extension != origin_extension {
            continue;
        }
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(first_line) = read_first_line(&entry_path)? else {
            continue;
        };
        let Ok(FirstRecord::V2(meta)) = parse_first_record(&first_line) else {
            continue;
        };
        if meta.session_id == origin.session_id {
            files.push((entry_path, meta));
        }
    }
    if files.is_empty() {
        files.push((path.to_path_buf(), origin.clone()));
    }
    Ok(files)
}

fn validate_v2_parts(files: &[(PathBuf, V2MetaRecord)]) -> Result<()> {
    for (index, (path, meta)) in files.iter().enumerate() {
        let expected = index as u32 + 1;
        if meta.part != expected {
            bail!(
                "record session parts must be contiguous from 1; expected part {expected}, found part {} in {}",
                meta.part,
                path.display()
            );
        }
    }
    Ok(())
}

fn read_v2_events_from_file(
    path: &Path,
    meta: &V2MetaRecord,
    part_count: Option<usize>,
    events: &mut Vec<RecordedEvent>,
) -> Result<()> {
    let file = File::open(path)
        .with_context(|| format!("failed to open replay file {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<V2ProbeRecord>(line.trim_end()) else {
            continue;
        };
        if record.format_version != 2 || record.record_type != "probe" {
            continue;
        }
        if record.session_id != meta.session_id {
            bail!("record event session_id mismatch in {}", path.display());
        }
        events.push(RecordedEvent {
            event: record.event,
            source_path: path.to_path_buf(),
            part: Some(meta.part),
            part_count,
        });
    }
}

fn part_path(base_path: &Path, part: u32) -> PathBuf {
    if part == 1 {
        return base_path.to_path_buf();
    }
    let parent = base_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let stem = base_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("pdeck");
    let extension = base_path.extension().and_then(|value| value.to_str());
    let filename = match extension {
        Some(extension) => format!("{stem}_part{part:04}.{extension}"),
        None => format!("{stem}_part{part:04}"),
    };
    parent.join(filename)
}

fn base_path_from_part(path: &Path, part: u32) -> Option<PathBuf> {
    if part == 1 {
        return Some(path.to_path_buf());
    }
    let suffix = format!("_part{part:04}");
    let stem = path.file_stem()?.to_str()?;
    let base_stem = stem.strip_suffix(&suffix)?;
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let extension = path.extension().and_then(|value| value.to_str());
    let filename = match extension {
        Some(extension) => format!("{base_stem}.{extension}"),
        None => base_stem.to_string(),
    };
    Some(parent.join(filename))
}

fn new_session_id() -> String {
    let now = Utc::now();
    format!("{}-{}", now.timestamp_micros(), std::process::id())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::model::{ProbeEvent, Target, TargetKind};

    use super::{
        RecordWriteStatus, SessionReadMode, V2MetaRecord, V2ProbeRecord, append_record_event,
        init_record_file, read_session_events_with_mode,
    };

    #[test]
    fn reads_cross_platform_replay_fixture() {
        let path = fixture_path("replay_cross_platform.jsonl");
        let session = read_session_events_with_mode(&path, SessionReadMode::Auto).unwrap();

        assert_eq!(session.targets.len(), 2);
        assert_eq!(session.events.len(), 7);
        assert_eq!(session.events[0].event.response, "time=2.4 ms");
        assert_eq!(session.events[4].event.response, "Request timed out.");
        assert_eq!(session.events[6].event.response, "2.1ms");
        assert_eq!(
            session.events.iter().filter(|event| event.event.ok).count(),
            4
        );
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

        let events = read_session_events_with_mode(&path, SessionReadMode::Auto).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(events.events.len(), 2);
        assert!(events.events[0].event.ok);
        assert!(!events.events[1].event.ok);
    }

    #[test]
    fn auto_discovers_v2_parts_from_origin_file() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("session.jsonl");
        let second = dir.path().join("session_part0002.jsonl");
        let ignored = dir.path().join("other.jsonl");
        write_v2_part(&second, "session-a", 2, &[sample_event_with_ts(2)]);
        write_v2_part(&first, "session-a", 1, &[sample_event_with_ts(1)]);
        write_v2_part(&ignored, "session-b", 1, &[sample_event_with_ts(99)]);

        let session = read_session_events_with_mode(&second, SessionReadMode::Auto).unwrap();

        assert_eq!(session.targets.len(), 1);
        assert_eq!(session.events.len(), 2);
        assert_eq!(session.events[0].event.ts_ms, 1);
        assert_eq!(session.events[0].part, Some(1));
        assert_eq!(session.events[0].part_count, Some(2));
        assert_eq!(session.events[1].event.ts_ms, 2);
        assert_eq!(session.events[1].source_path, second);
    }

    #[test]
    fn only_reads_single_v2_part() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("session.jsonl");
        let second = dir.path().join("session_part0002.jsonl");
        write_v2_part(&first, "session-a", 1, &[sample_event_with_ts(1)]);
        write_v2_part(&second, "session-a", 2, &[sample_event_with_ts(2)]);

        let session = read_session_events_with_mode(&second, SessionReadMode::Only).unwrap();

        assert_eq!(session.events.len(), 1);
        assert_eq!(session.events[0].event.ts_ms, 2);
        assert_eq!(session.events[0].part, Some(2));
        assert_eq!(session.events[0].part_count, None);
    }

    #[test]
    fn rejects_missing_v2_part_in_auto_mode() {
        let dir = tempfile::tempdir().unwrap();
        let second = dir.path().join("session_part0002.jsonl");
        write_v2_part(&second, "session-a", 2, &[sample_event_with_ts(2)]);

        let err = read_session_events_with_mode(&second, SessionReadMode::Auto).unwrap_err();

        assert!(err.to_string().contains("contiguous from 1"));
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
    fn explicit_record_creation_rejects_existing_rotated_part() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("office.jsonl");
        let second = dir.path().join("office_part0002.jsonl");
        std::fs::write(&second, "existing").unwrap();

        let err = init_record_file(&first, &sample_targets(), false, false, 0).unwrap_err();

        assert_eq!(
            err.downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::AlreadyExists)
        );
    }

    #[test]
    fn rotates_recording_events_when_size_limit_is_reached() {
        let path =
            std::env::temp_dir().join(format!("pdeck-limited-record-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let part2 = path.with_file_name(format!(
            "{}_part0002.jsonl",
            path.file_stem().and_then(|value| value.to_str()).unwrap()
        ));
        let _ = std::fs::remove_file(&part2);
        let mut writer = init_record_file(&path, &sample_targets(), false, false, 520).unwrap();

        let status = append_record_event(&mut writer, &sample_event()).unwrap();
        let stopped = append_record_event(&mut writer, &sample_event()).unwrap();
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&part2);

        assert_eq!(status, RecordWriteStatus::Written);
        assert_eq!(stopped, RecordWriteStatus::Rotated);
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

        let err = read_session_events_with_mode(&path, SessionReadMode::Auto).unwrap_err();
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

        assert!(
            err.to_string()
                .contains("--record-overwrite is not supported")
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

    fn sample_event_with_ts(ts_ms: u64) -> ProbeEvent {
        ProbeEvent {
            ts_ms,
            ..sample_event()
        }
    }

    fn write_v2_part(path: &Path, session_id: &str, part: u32, events: &[ProbeEvent]) {
        let meta = V2MetaRecord {
            format_version: 2,
            record_type: "meta".to_string(),
            session_id: session_id.to_string(),
            part,
            file_started_at: format!("2026-04-26T12:00:0{part}Z"),
            targets: sample_targets(),
        };
        let mut lines = vec![serde_json::to_string(&meta).unwrap()];
        for event in events {
            lines.push(
                serde_json::to_string(&V2ProbeRecord {
                    format_version: 2,
                    record_type: "probe".to_string(),
                    session_id: session_id.to_string(),
                    event: event.clone(),
                })
                .unwrap(),
            );
        }
        std::fs::write(path, format!("{}\n", lines.join("\n"))).unwrap();
    }

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }
}
