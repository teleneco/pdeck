use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use chrono::Local;
use tempfile::NamedTempFile;

use crate::cli::Args;

pub struct TempFileGuard {
    file: NamedTempFile,
}

impl TempFileGuard {
    pub fn path(&self) -> &Path {
        self.file.path()
    }
}

pub fn open_editor_with_tempfile() -> Result<TempFileGuard> {
    let file = tempfile::Builder::new()
        .prefix("pdeck-")
        .suffix(".txt")
        .tempfile()?;
    let path = file.path().to_path_buf();
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let (program, args) = parse_editor_command(&editor)?;
    let status = std::process::Command::new(program)
        .args(args)
        .arg(&path)
        .status()?;
    if !status.success() {
        bail!("editor exited with status {status}");
    }
    Ok(TempFileGuard { file })
}

pub fn parse_editor_command(input: &str) -> Result<(String, Vec<String>)> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' => {
                escaped = true;
            }
            '\'' | '"' => {
                if let Some(active) = quote {
                    if active == ch {
                        quote = None;
                    } else {
                        current.push(ch);
                    }
                } else {
                    quote = Some(ch);
                }
            }
            c if c.is_whitespace() && quote.is_none() => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped {
        bail!("EDITOR ends with an escape character");
    }
    if quote.is_some() {
        bail!("EDITOR contains an unmatched quote");
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() {
        bail!("EDITOR must not be empty");
    }

    let program = parts.remove(0);
    Ok((program, parts))
}

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

    use super::{parse_editor_command, resolve_record_path};

    #[test]
    fn parses_editor_command_with_args() {
        let (program, args) = parse_editor_command("code --wait").unwrap();
        assert_eq!(program, "code");
        assert_eq!(args, vec!["--wait"]);
    }

    #[test]
    fn parses_editor_command_with_quotes() {
        let (program, args) = parse_editor_command(
            "\"/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code\" --wait",
        )
        .unwrap();
        assert_eq!(
            program,
            "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"
        );
        assert_eq!(args, vec!["--wait"]);
    }

    #[test]
    fn rejects_unmatched_quote_in_editor_command() {
        let err = parse_editor_command("\"code --wait").unwrap_err();
        assert!(err.to_string().contains("unmatched quote"));
    }

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
