use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::cli::{Cli, Command, ServeArgs};

const DEFAULT_CONFIG_PATH: &str = "lb-monitor.toml";
const DEFAULT_DB_PATH: &str = "lb-monitor.sqlite3";
const DEFAULT_DUMMY_DB_PATH: &str = "lb-monitor-dummy.sqlite3";
const DEFAULT_URL: &str = "https://dataagent.top/leaderboard";
const DEFAULT_INTERVAL_SECONDS: u64 = 300;
const DEFAULT_TUI_REFRESH_SECONDS: u64 = 5;

#[derive(Debug, Clone)]
pub struct Config {
    pub database: DatabaseConfig,
    pub fetch: FetchConfig,
    pub notify: NotifyConfig,
    pub tui: TuiConfig,
}

#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct FetchConfig {
    pub url: String,
    pub interval_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct NotifyConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct TuiConfig {
    pub refresh_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Config,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    database: Option<FileDatabaseConfig>,
    fetch: Option<FileFetchConfig>,
    notify: Option<FileNotifyConfig>,
    tui: Option<FileTuiConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct FileDatabaseConfig {
    path: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct FileFetchConfig {
    url: Option<String>,
    interval_seconds: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct FileNotifyConfig {
    enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct FileTuiConfig {
    refresh_seconds: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            database: DatabaseConfig {
                path: PathBuf::from(DEFAULT_DB_PATH),
            },
            fetch: FetchConfig {
                url: DEFAULT_URL.to_string(),
                interval_seconds: DEFAULT_INTERVAL_SECONDS,
            },
            notify: NotifyConfig { enabled: true },
            tui: TuiConfig {
                refresh_seconds: DEFAULT_TUI_REFRESH_SECONDS,
            },
        }
    }
}

impl LoadedConfig {
    pub fn load(cli: &Cli) -> Result<Self> {
        let config_path = cli
            .config
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH));
        let file_config = load_file_config(&config_path)?;
        let mut config = Config::default();

        if let Some(database) = file_config.database
            && let Some(path) = database.path
        {
            config.database.path = path;
        }
        if let Some(fetch) = file_config.fetch {
            if let Some(url) = fetch.url {
                config.fetch.url = url;
            }
            if let Some(interval_seconds) = fetch.interval_seconds {
                config.fetch.interval_seconds = interval_seconds;
            }
        }
        if let Some(notify) = file_config.notify
            && let Some(enabled) = notify.enabled
        {
            config.notify.enabled = enabled;
        }
        if let Some(tui) = file_config.tui
            && let Some(refresh_seconds) = tui.refresh_seconds
        {
            config.tui.refresh_seconds = refresh_seconds;
        }

        if let Some(db_path) = &cli.db {
            config.database.path = db_path.clone();
        } else if matches!(cli.command, Some(Command::Dummy(_))) {
            config.database.path = PathBuf::from(DEFAULT_DUMMY_DB_PATH);
        }

        match &cli.command {
            Some(Command::Tui(args)) => {
                if let Some(refresh_seconds) = args.refresh_seconds {
                    config.tui.refresh_seconds = refresh_seconds;
                }
            }
            Some(Command::Serve(args)) => apply_serve_overrides(&mut config, args),
            Some(Command::Dummy(_)) => {}
            None => {}
        }

        Ok(Self { config })
    }
}

fn apply_serve_overrides(config: &mut Config, args: &ServeArgs) {
    if let Some(interval) = args.interval {
        config.fetch.interval_seconds = interval;
    }
    if args.notify {
        config.notify.enabled = true;
    }
    if args.no_notify {
        config.notify.enabled = false;
    }
}

fn load_file_config(path: &Path) -> Result<FileConfig> {
    if !path.exists() {
        return Ok(FileConfig::default());
    }

    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    toml::from_str(&contents)
        .with_context(|| format!("failed to parse config file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use clap::Parser;
    use tempfile::tempdir;

    use super::*;
    use crate::cli::Cli;

    #[test]
    fn uses_defaults_when_config_missing() {
        let dir = tempdir().expect("tempdir");
        let missing_config = dir.path().join("missing.toml");
        let cli = Cli::parse_from([
            "lb-monitor",
            "--config",
            missing_config.to_str().expect("utf8"),
        ]);
        let loaded = LoadedConfig::load(&cli).expect("load config");
        assert_eq!(loaded.config.fetch.interval_seconds, DEFAULT_INTERVAL_SECONDS);
        assert_eq!(loaded.config.database.path, PathBuf::from(DEFAULT_DB_PATH));
    }

    #[test]
    fn merges_toml_and_cli_overrides() {
        let dir = tempdir().expect("tempdir");
        let config_path = dir.path().join("lb-monitor.toml");
        fs::write(
            &config_path,
            r#"
[database]
path = "sample.sqlite3"

[fetch]
interval_seconds = 123

[notify]
enabled = false

[tui]
refresh_seconds = 9
"#,
        )
        .expect("write config");
        let cli = Cli::parse_from([
            "lb-monitor",
            "--config",
            config_path.to_str().expect("utf8"),
            "--db",
            "override.sqlite3",
            "serve",
            "--interval",
            "60",
            "--notify",
        ]);

        let loaded = LoadedConfig::load(&cli).expect("load config");
        assert_eq!(loaded.config.database.path, PathBuf::from("override.sqlite3"));
        assert_eq!(loaded.config.fetch.interval_seconds, 60);
        assert!(loaded.config.notify.enabled);
        assert_eq!(loaded.config.tui.refresh_seconds, 9);
    }

    #[test]
    fn dummy_uses_separate_default_db_when_no_config() {
        let cli = Cli::parse_from(["lb-monitor", "dummy"]);
        let loaded = LoadedConfig::load(&cli).expect("load config");
        assert_eq!(
            loaded.config.database.path,
            PathBuf::from(DEFAULT_DUMMY_DB_PATH)
        );
    }
}
