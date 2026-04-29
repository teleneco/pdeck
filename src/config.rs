use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, anyhow};
use chrono::Local;
use clap::ArgMatches;
use clap::parser::ValueSource;
use serde::{Deserialize, Serialize};

use crate::cli::{Args, Command, SizeArg};

#[derive(Clone, Debug, Default)]
pub struct ArgSources {
    pub record: Option<ValueSource>,
    pub record_size_limit: Option<ValueSource>,
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeConfig {
    pub record_dir: Option<PathBuf>,
    pub always_use_record_dir: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ConfigFile {
    record: bool,
    record_dir: PathBuf,
    record_size_limit: String,
    always_use_record_dir: bool,
}

#[derive(Debug, Deserialize)]
struct RawConfigFile {
    record: Option<bool>,
    record_dir: Option<PathBuf>,
    record_size_limit: Option<String>,
    always_use_record_dir: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct ConfigUpdate {
    pub record: Option<bool>,
    pub record_dir: Option<PathBuf>,
    pub record_size_limit: Option<String>,
    pub always_use_record_dir: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct ConfigCommandResult {
    pub config_path: PathBuf,
    pub record_dir: PathBuf,
    pub status: ConfigSetStatus,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ConfigSetStatus {
    Initialized,
    Updated,
    Unchanged,
}

impl ArgSources {
    pub fn from_matches(matches: &ArgMatches) -> Self {
        Self {
            record: matches.value_source("record"),
            record_size_limit: matches.value_source("record_size_limit"),
        }
    }
}

pub fn load_and_apply_config(args: &mut Args, sources: &ArgSources) -> Result<RuntimeConfig> {
    let Some(path) = config_path() else {
        return Ok(RuntimeConfig::default());
    };
    if !path.exists() {
        return Ok(RuntimeConfig::default());
    }

    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    let config = parse_config(&contents, &path)?;
    apply_config(args, sources, config)
        .with_context(|| format!("failed to apply config file {}", path.display()))
}

pub fn set_config(update: ConfigUpdate) -> Result<ConfigCommandResult> {
    let config_path =
        config_path().ok_or_else(|| anyhow!("failed to resolve pdeck config path"))?;
    let config_dir = config_path
        .parent()
        .ok_or_else(|| anyhow!("failed to resolve pdeck config directory"))?;
    fs::create_dir_all(config_dir)
        .with_context(|| format!("failed to create config directory {}", config_dir.display()))?;

    let existed = config_path.exists();
    let mut config = if existed {
        let config = read_config_file(&config_path)?;
        if update.is_empty() {
            let record_dir = expand_home(config.record_dir.clone())?;
            return Ok(ConfigCommandResult {
                config_path,
                record_dir,
                status: ConfigSetStatus::Unchanged,
            });
        }
        config
    } else {
        default_config()
    };
    if let Some(record) = update.record {
        config.record = record;
    }
    if let Some(record_dir) = update.record_dir {
        config.record_dir = record_dir;
    }
    if let Some(record_size_limit) = update.record_size_limit {
        config.record_size_limit = record_size_limit;
    }
    if let Some(always_use_record_dir) = update.always_use_record_dir {
        config.always_use_record_dir = always_use_record_dir;
    }
    validate_config(&config)?;

    let rendered = render_config(&config)?;
    fs::write(&config_path, rendered)
        .with_context(|| format!("failed to write config file {}", config_path.display()))?;
    let record_dir = expand_home(config.record_dir.clone())?;
    fs::create_dir_all(&record_dir)
        .with_context(|| format!("failed to create record directory {}", record_dir.display()))?;

    Ok(ConfigCommandResult {
        config_path,
        record_dir,
        status: if existed {
            ConfigSetStatus::Updated
        } else {
            ConfigSetStatus::Initialized
        },
    })
}

pub fn show_config() -> Result<(PathBuf, String)> {
    let config_path =
        config_path().ok_or_else(|| anyhow!("failed to resolve pdeck config path"))?;
    let config = read_config_file(&config_path)?;
    validate_config(&config)?;
    Ok((config_path, render_config(&config)?))
}

pub fn verify_config() -> Result<ConfigCommandResult> {
    let config_path =
        config_path().ok_or_else(|| anyhow!("failed to resolve pdeck config path"))?;
    let config = read_config_file(&config_path)?;
    validate_config(&config)?;
    let record_dir = expand_home(config.record_dir.clone())?;
    Ok(ConfigCommandResult {
        config_path,
        record_dir,
        status: ConfigSetStatus::Unchanged,
    })
}

impl ConfigUpdate {
    pub fn is_empty(&self) -> bool {
        self.record.is_none()
            && self.record_dir.is_none()
            && self.record_size_limit.is_none()
            && self.always_use_record_dir.is_none()
    }
}

fn apply_config(
    args: &mut Args,
    sources: &ArgSources,
    config: ConfigFile,
) -> Result<RuntimeConfig> {
    let use_config_record = source_allows_config(sources.record);
    validate_config(&config)?;
    let use_record_dir_for_config_record = use_config_record && !args.no_record && config.record;
    let record_dir = Some(expand_home(config.record_dir)?).or_else(default_record_dir);

    if use_record_dir_for_config_record {
        args.record = Some(None);
    }
    if args.record.is_some() && source_allows_config(sources.record_size_limit) {
        args.record_size_limit = SizeArg::from_str(&config.record_size_limit)?;
    }
    Ok(RuntimeConfig {
        record_dir,
        always_use_record_dir: use_record_dir_for_config_record || config.always_use_record_dir,
    })
}

fn read_config_file(path: &Path) -> Result<ConfigFile> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    parse_config(&contents, path)
}

fn parse_config(contents: &str, path: &Path) -> Result<ConfigFile> {
    let raw: RawConfigFile = toml::from_str(contents)
        .with_context(|| format!("failed to parse config file {}", path.display()))?;
    let mut config = default_config();
    if let Some(record) = raw.record {
        config.record = record;
    }
    if let Some(record_dir) = raw.record_dir {
        config.record_dir = record_dir;
    }
    if let Some(record_size_limit) = raw.record_size_limit {
        config.record_size_limit = record_size_limit;
    }
    if let Some(always_use_record_dir) = raw.always_use_record_dir {
        config.always_use_record_dir = always_use_record_dir;
    }
    Ok(config)
}

fn validate_config(config: &ConfigFile) -> Result<()> {
    let _ = SizeArg::from_str(&config.record_size_limit)?;
    if config.record_dir.as_os_str().is_empty() {
        return Err(anyhow!("record_dir cannot be empty"));
    }
    Ok(())
}

fn default_config() -> ConfigFile {
    ConfigFile {
        record: false,
        record_dir: PathBuf::from("~/.config/pdeck/records"),
        record_size_limit: "0".to_string(),
        always_use_record_dir: true,
    }
}

fn render_config(config: &ConfigFile) -> Result<String> {
    let mut rendered = toml::to_string_pretty(config).context("failed to render config")?;
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    Ok(rendered)
}

fn source_allows_config(source: Option<ValueSource>) -> bool {
    matches!(source, None | Some(ValueSource::DefaultValue))
}

pub fn config_path() -> Option<PathBuf> {
    config_home().map(|dir| dir.join("config.toml"))
}

fn default_record_dir() -> Option<PathBuf> {
    config_home().map(|dir| dir.join("records"))
}

fn config_home() -> Option<PathBuf> {
    dirs::home_dir().map(|dir| dir.join(".config").join("pdeck"))
}

fn expand_home(path: PathBuf) -> Result<PathBuf> {
    let Some(path_text) = path.to_str() else {
        return Ok(path);
    };
    if path_text == "~" {
        return dirs::home_dir().ok_or_else(|| anyhow!("failed to resolve home directory"));
    }
    if let Some(rest) = path_text.strip_prefix("~/") {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("failed to resolve home directory"))?;
        return Ok(home.join(rest));
    }
    Ok(path)
}

pub fn build_status_line(args: &Args, record_path: Option<&PathBuf>) -> String {
    let mut parts = Vec::new();
    if let Some(Command::Replay { file, only }) = &args.command {
        if let Some(replay_path) = file.as_ref().or(only.as_ref()) {
            parts.push(format!("replay: {}", replay_path.display()));
        } else {
            parts.push("replay".to_string());
        }
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
    parts.join(" | ")
}

pub fn resolve_record_path(
    targets_path: &Path,
    record_path: Option<&PathBuf>,
    generated_record_dir: Option<&PathBuf>,
) -> PathBuf {
    if let Some(path) = record_path {
        return path.clone();
    }

    let stem = targets_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("pdeck");
    let filename = format!("{}_{}.jsonl", stem, Local::now().format("%Y%m%d_%H%M%S"));
    generated_record_dir
        .map(|dir| dir.join(&filename))
        .unwrap_or_else(|| PathBuf::from(filename))
}

pub fn ensure_record_dir(path: Option<&PathBuf>) -> Result<()> {
    if let Some(path) = path {
        fs::create_dir_all(path)
            .with_context(|| format!("failed to create record directory {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::parser::ValueSource;

    use super::{ArgSources, ConfigFile, apply_config, resolve_record_path};

    #[test]
    fn derives_record_path_from_targets_path() {
        let path = resolve_record_path(&PathBuf::from("office.txt"), None, None);
        let filename = path.file_name().and_then(|value| value.to_str()).unwrap();
        assert!(filename.starts_with("office_"));
        assert!(filename.ends_with(".jsonl"));

        assert_eq!(
            resolve_record_path(
                &PathBuf::from("office.txt"),
                Some(&PathBuf::from("custom.jsonl")),
                Some(&PathBuf::from("records"))
            ),
            PathBuf::from("custom.jsonl")
        );
    }

    #[test]
    fn derives_record_path_inside_configured_record_dir() {
        let path = resolve_record_path(
            &PathBuf::from("office.txt"),
            None,
            Some(&PathBuf::from("records")),
        );

        assert_eq!(path.parent(), Some(PathBuf::from("records").as_path()));
        assert!(
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("office_")
        );
    }

    #[test]
    fn config_record_defaults_to_record_dir() {
        let mut args = crate::cli::Args {
            command: None,
            interval: crate::cli::DurationArg(std::time::Duration::from_secs(1)),
            timeout: crate::cli::DurationArg(std::time::Duration::from_secs(3)),
            file: PathBuf::from("targets.txt"),
            arp_entries: false,
            concurrency: 16,
            icmp_backend: crate::cli::IcmpBackendArg::Auto,
            record: None,
            no_record: false,
            record_overwrite: false,
            record_size_limit: crate::cli::SizeArg(0),
            no_tui: false,
        };
        let runtime = apply_config(
            &mut args,
            &ArgSources::default(),
            ConfigFile {
                record: true,
                record_dir: PathBuf::from("~/records"),
                record_size_limit: "100mb".to_string(),
                always_use_record_dir: true,
            },
        )
        .unwrap();

        assert!(matches!(args.record, Some(None)));
        assert_eq!(args.record_size_limit.0, 100_000_000);
        assert!(runtime.record_dir.is_some());
        assert!(runtime.always_use_record_dir);
    }

    #[test]
    fn cli_record_uses_record_dir_by_default() {
        let mut args = crate::cli::Args {
            command: None,
            interval: crate::cli::DurationArg(std::time::Duration::from_secs(1)),
            timeout: crate::cli::DurationArg(std::time::Duration::from_secs(3)),
            file: PathBuf::from("targets.txt"),
            arp_entries: false,
            concurrency: 16,
            icmp_backend: crate::cli::IcmpBackendArg::Auto,
            record: Some(None),
            no_record: false,
            record_overwrite: false,
            record_size_limit: crate::cli::SizeArg(0),
            no_tui: false,
        };
        let sources = ArgSources {
            record: Some(ValueSource::CommandLine),
            ..ArgSources::default()
        };
        let runtime = apply_config(
            &mut args,
            &sources,
            ConfigFile {
                record: false,
                record_dir: PathBuf::from("records"),
                record_size_limit: "0".to_string(),
                always_use_record_dir: true,
            },
        )
        .unwrap();

        assert!(matches!(args.record, Some(None)));
        assert_eq!(runtime.record_dir, Some(PathBuf::from("records")));
        assert!(runtime.always_use_record_dir);
    }

    #[test]
    fn config_can_keep_cli_pathless_record_in_cwd() {
        let mut args = crate::cli::Args {
            command: None,
            interval: crate::cli::DurationArg(std::time::Duration::from_secs(1)),
            timeout: crate::cli::DurationArg(std::time::Duration::from_secs(3)),
            file: PathBuf::from("targets.txt"),
            arp_entries: false,
            concurrency: 16,
            icmp_backend: crate::cli::IcmpBackendArg::Auto,
            record: Some(None),
            no_record: false,
            record_overwrite: false,
            record_size_limit: crate::cli::SizeArg(0),
            no_tui: false,
        };
        let sources = ArgSources {
            record: Some(ValueSource::CommandLine),
            ..ArgSources::default()
        };
        let runtime = apply_config(
            &mut args,
            &sources,
            ConfigFile {
                record: false,
                record_dir: PathBuf::from("records"),
                record_size_limit: "0".to_string(),
                always_use_record_dir: false,
            },
        )
        .unwrap();

        assert!(matches!(args.record, Some(None)));
        assert_eq!(runtime.record_dir, Some(PathBuf::from("records")));
        assert!(!runtime.always_use_record_dir);
    }

    #[test]
    fn no_record_disables_config_record() {
        let mut args = crate::cli::Args {
            command: None,
            interval: crate::cli::DurationArg(std::time::Duration::from_secs(1)),
            timeout: crate::cli::DurationArg(std::time::Duration::from_secs(3)),
            file: PathBuf::from("targets.txt"),
            arp_entries: false,
            concurrency: 16,
            icmp_backend: crate::cli::IcmpBackendArg::Auto,
            record: None,
            no_record: true,
            record_overwrite: false,
            record_size_limit: crate::cli::SizeArg(0),
            no_tui: false,
        };
        let runtime = apply_config(
            &mut args,
            &ArgSources::default(),
            ConfigFile {
                record: true,
                record_dir: PathBuf::from("records"),
                record_size_limit: "100mb".to_string(),
                always_use_record_dir: true,
            },
        )
        .unwrap();

        assert!(args.record.is_none());
        assert_eq!(args.record_size_limit.0, 0);
        assert_eq!(runtime.record_dir, Some(PathBuf::from("records")));
        assert!(runtime.always_use_record_dir);
    }
}
