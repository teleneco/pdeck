use std::path::{Path, PathBuf};

use chrono::Local;

use crate::cli::Args;

pub fn build_status_line(args: &Args, record_path: Option<&PathBuf>) -> String {
    let mut parts = Vec::new();
    if let Some(replay_path) = &args.replay {
        parts.push(format!("replay: {}", replay_path.display()));
    } else {
        parts.push("live".to_string());
    }
    if let Some(record_path) = record_path {
        parts.push(format!("record: {}", record_path.display()));
    }
    if args.record_size_limit.0 > 0 {
        parts.push(format!(
            "record-rotation: {} bytes",
            args.record_size_limit.0
        ));
    }
    if let Some(log_path) = &args.log {
        parts.push(format!("log: {}", log_path.display()));
    }
    parts.join(" | ")
}

pub fn resolve_record_path(targets_path: &Path, record_path: Option<&PathBuf>) -> PathBuf {
    if let Some(path) = record_path {
        return path.clone();
    }

    let stem = targets_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("pdeck");
    PathBuf::from(format!(
        "{}_{}.jsonl",
        stem,
        Local::now().format("%Y%m%d_%H%M%S")
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::resolve_record_path;

    #[test]
    fn derives_record_path_from_targets_path() {
        let path = resolve_record_path(&PathBuf::from("office.txt"), None);
        let filename = path.file_name().and_then(|value| value.to_str()).unwrap();
        assert!(filename.starts_with("office_"));
        assert!(filename.ends_with(".jsonl"));

        assert_eq!(
            resolve_record_path(
                &PathBuf::from("office.txt"),
                Some(&PathBuf::from("custom.jsonl"))
            ),
            PathBuf::from("custom.jsonl")
        );
    }
}
