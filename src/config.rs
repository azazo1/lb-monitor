use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::warn;

use crate::cli::{Cli, Command, ServeArgs, TuiArgs};

const DEFAULT_CONFIG_DIR: &str = ".config/lbm";
const DEFAULT_CONFIG_FILE_NAME: &str = "config.toml";
const DEFAULT_DB_PATH: &str = "lb-monitor.sqlite3";
const DEFAULT_DUMMY_DB_PATH: &str = "lb-monitor-dummy.sqlite3";
const DEFAULT_FETCH_URL: &str = "https://dataagent.top/leaderboard";
const DEFAULT_TUI_API_BASE_URL: &str = "http://127.0.0.1:8080";
const DEFAULT_SERVE_LISTEN_ADDR: &str = "127.0.0.1:8080";
const DEFAULT_FETCH_INTERVAL_SECONDS: u64 = 300;
const DEFAULT_TUI_REFRESH_SECONDS: u64 = 5;
const DEFAULT_SMTP_PORT: u16 = 465;

#[derive(Debug, Clone)]
pub struct Config {
    pub database: DatabaseConfig,
    pub serve: ServeConfig,
    pub tui: TuiConfig,
}

#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ServeConfig {
    pub fetch: FetchConfig,
    pub http: ServeHttpConfig,
    pub mail: MailConfig,
}

#[derive(Debug, Clone)]
pub struct FetchConfig {
    pub url: String,
    pub interval_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct ServeHttpConfig {
    pub listen: String,
}

#[derive(Debug, Clone)]
pub struct MailConfig {
    pub enabled: bool,
    pub smtp: SmtpConfig,
}

#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: Option<String>,
    pub to: Vec<String>,
    pub security: SmtpSecurity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpSecurity {
    Plain,
    StartTls,
    Tls,
}

#[derive(Debug, Clone)]
pub struct TuiConfig {
    pub refresh_seconds: u64,
    pub api_base_url: String,
    pub source: TuiSource,
    pub database_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiSource {
    LocalSqlite,
    RemoteApi,
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Config,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    database: Option<FileDatabaseConfig>,
    serve: Option<FileServeConfig>,
    tui: Option<FileTuiConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct FileDatabaseConfig {
    path: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct FileServeConfig {
    fetch: Option<FileFetchConfig>,
    http: Option<FileServeHttpConfig>,
    mail: Option<FileMailConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct FileFetchConfig {
    url: Option<String>,
    interval_seconds: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct FileServeHttpConfig {
    listen: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FileMailConfig {
    enabled: Option<bool>,
    smtp: Option<FileSmtpConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct FileSmtpConfig {
    host: Option<String>,
    port: Option<u16>,
    username: Option<String>,
    password: Option<String>,
    from: Option<String>,
    to: Option<Vec<String>>,
    security: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FileTuiConfig {
    refresh_seconds: Option<u64>,
    api_base_url: Option<String>,
    source: Option<String>,
    database_path: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            database: DatabaseConfig {
                path: PathBuf::from(DEFAULT_DB_PATH),
            },
            serve: ServeConfig {
                fetch: FetchConfig {
                    url: DEFAULT_FETCH_URL.to_string(),
                    interval_seconds: DEFAULT_FETCH_INTERVAL_SECONDS,
                },
                http: ServeHttpConfig {
                    listen: DEFAULT_SERVE_LISTEN_ADDR.to_string(),
                },
                mail: MailConfig {
                    enabled: false,
                    smtp: SmtpConfig {
                        host: String::new(),
                        port: DEFAULT_SMTP_PORT,
                        username: None,
                        password: None,
                        from: None,
                        to: Vec::new(),
                        security: SmtpSecurity::Tls,
                    },
                },
            },
            tui: TuiConfig {
                refresh_seconds: DEFAULT_TUI_REFRESH_SECONDS,
                api_base_url: DEFAULT_TUI_API_BASE_URL.to_string(),
                source: TuiSource::LocalSqlite,
                database_path: PathBuf::from(DEFAULT_DB_PATH),
            },
        }
    }
}

impl LoadedConfig {
    pub fn load(cli: &Cli) -> Result<Self> {
        let config_path = cli.config.clone().unwrap_or_else(default_config_path);
        let mut file_config = load_file_config(&config_path)?;
        expand_file_config_paths(&mut file_config)?;
        let mut config = Config::default();
        let mut tui_source_explicit = false;
        let mut tui_database_path_explicit = false;
        let mut tui_api_base_url_set = false;

        if let Some(database) = file_config.database
            && let Some(path) = database.path
        {
            config.database.path = path;
        }

        if let Some(serve) = file_config.serve {
            if let Some(fetch) = serve.fetch {
                apply_fetch_overrides(&mut config.serve.fetch, fetch);
            }
            if let Some(http) = serve.http
                && let Some(listen) = http.listen
            {
                config.serve.http.listen = listen;
            }
            if let Some(mail) = serve.mail {
                apply_mail_overrides(&mut config.serve.mail, mail);
            }
        }

        if let Some(tui) = file_config.tui {
            if let Some(refresh_seconds) = tui.refresh_seconds {
                config.tui.refresh_seconds = refresh_seconds;
            }
            if let Some(api_base_url) = tui.api_base_url {
                config.tui.api_base_url = api_base_url;
                tui_api_base_url_set = true;
            }
            if let Some(source) = tui.source {
                config.tui.source = parse_tui_source(&source).unwrap_or(TuiSource::LocalSqlite);
                tui_source_explicit = true;
            }
            if let Some(database_path) = tui.database_path {
                config.tui.database_path = database_path;
                tui_database_path_explicit = true;
            }
        }

        if tui_api_base_url_set && !tui_source_explicit {
            config.tui.source = TuiSource::RemoteApi;
        }

        if let Some(db_path) = &cli.db {
            config.database.path = db_path.clone();
            config.tui.database_path = db_path.clone();
        } else if matches!(cli.command, Some(Command::Dummy(_))) {
            config.database.path = PathBuf::from(DEFAULT_DUMMY_DB_PATH);
            config.tui.database_path = config.database.path.clone();
        } else if !tui_database_path_explicit {
            config.tui.database_path = config.database.path.clone();
        }

        match &cli.command {
            Some(Command::Tui(args)) => apply_tui_overrides(&mut config, args),
            Some(Command::Serve(args)) => apply_serve_overrides(&mut config, args),
            Some(Command::Dummy(_)) => {}
            None => {}
        }

        if config.tui.api_base_url.is_empty() {
            config.tui.api_base_url = DEFAULT_TUI_API_BASE_URL.to_string();
        }

        Ok(Self { config })
    }
}

fn default_config_path() -> PathBuf {
    shellexpand::tilde(
        &Path::new("~")
            .join(DEFAULT_CONFIG_DIR)
            .join(DEFAULT_CONFIG_FILE_NAME)
            .to_string_lossy(),
    )
    .to_string()
    .into()
}

fn expand_file_config_paths(file_config: &mut FileConfig) -> Result<()> {
    expand_file_config_paths_with(file_config, &|path| {
        Ok(shellexpand::path::full(path).map(|value| value.into_owned())?)
    })
}

fn expand_file_config_paths_with<F>(file_config: &mut FileConfig, expand_path: &F) -> Result<()>
where
    F: Fn(&Path) -> Result<PathBuf>,
{
    if let Some(database) = file_config.database.as_mut()
        && let Some(path) = database.path.as_mut()
    {
        *path = expand_path(path).map_err(|error| {
            anyhow::anyhow!(
                "failed to expand [database].path `{}`: {error}",
                path.display()
            )
        })?;
    }

    if let Some(tui) = file_config.tui.as_mut()
        && let Some(path) = tui.database_path.as_mut()
    {
        *path = expand_path(path).map_err(|error| {
            anyhow::anyhow!(
                "failed to expand [tui].database_path `{}`: {error}",
                path.display()
            )
        })?;
    }

    Ok(())
}

impl Config {
    pub fn redacted_command_summary(&self, command: &Command) -> String {
        match command {
            Command::Tui(_) => format!(
                "Tui(Config {{ database_path: {:?}, refresh_seconds: {}, source: {:?}, api_base_url: {:?} }})",
                self.tui.database_path,
                self.tui.refresh_seconds,
                self.tui.source,
                self.tui.api_base_url,
            ),
            Command::Serve(args) => format!(
                "Serve(Config {{ database_path: {:?}, fetch_url: {:?}, interval_seconds: {}, listen: {:?}, once: {}, mail_enabled: {}, smtp_host: {:?}, smtp_port: {}, smtp_username: {}, smtp_password: {}, smtp_from: {}, smtp_to: {}, smtp_security: {:?} }})",
                self.database.path,
                self.serve.fetch.url,
                self.serve.fetch.interval_seconds,
                self.serve.http.listen,
                args.once,
                self.serve.mail.enabled,
                self.serve.mail.smtp.host,
                self.serve.mail.smtp.port,
                redact_option(&self.serve.mail.smtp.username),
                redact_option(&self.serve.mail.smtp.password),
                redact_option(&self.serve.mail.smtp.from),
                redact_vec(&self.serve.mail.smtp.to),
                self.serve.mail.smtp.security,
            ),
            Command::Dummy(args) => format!(
                "Dummy(Config {{ database_path: {:?}, snapshots: {}, teams: {} }})",
                self.database.path, args.snapshots, args.teams,
            ),
        }
    }
}

fn apply_fetch_overrides(target: &mut FetchConfig, source: FileFetchConfig) {
    if let Some(url) = source.url {
        target.url = url;
    }
    if let Some(interval_seconds) = source.interval_seconds {
        target.interval_seconds = interval_seconds;
    }
}

fn apply_mail_overrides(target: &mut MailConfig, source: FileMailConfig) {
    if let Some(enabled) = source.enabled {
        target.enabled = enabled;
    }
    if let Some(smtp) = source.smtp {
        apply_smtp_overrides(&mut target.smtp, smtp);
    }
}

fn apply_tui_overrides(config: &mut Config, args: &TuiArgs) {
    if let Some(refresh_seconds) = args.refresh_seconds {
        config.tui.refresh_seconds = refresh_seconds;
    }
    if let Some(source) = &args.source {
        config.tui.source = parse_tui_source(source).unwrap_or(TuiSource::LocalSqlite);
    }
    if let Some(api_base_url) = &args.api_base_url {
        config.tui.api_base_url = api_base_url.clone();
        if args.source.is_none() {
            config.tui.source = TuiSource::RemoteApi;
        }
    }
}

fn apply_serve_overrides(config: &mut Config, args: &ServeArgs) {
    if let Some(interval) = args.interval {
        config.serve.fetch.interval_seconds = interval;
    }
    if let Some(listen) = &args.listen {
        config.serve.http.listen = listen.clone();
    }
    if args.notify {
        config.serve.mail.enabled = true;
    }
    if args.no_notify {
        config.serve.mail.enabled = false;
    }
    if let Some(host) = &args.smtp_host {
        config.serve.mail.smtp.host = host.clone();
    }
    if let Some(port) = args.smtp_port {
        config.serve.mail.smtp.port = port;
    }
    if let Some(username) = &args.smtp_username {
        config.serve.mail.smtp.username = Some(username.clone());
    }
    if let Some(password) = &args.smtp_password {
        config.serve.mail.smtp.password = Some(password.clone());
    }
    if let Some(from) = &args.smtp_from {
        config.serve.mail.smtp.from = Some(from.clone());
    }
    if !args.smtp_to.is_empty() {
        config.serve.mail.smtp.to = args.smtp_to.clone();
    }
    if let Some(security) = &args.smtp_security {
        config.serve.mail.smtp.security =
            parse_smtp_security(security).unwrap_or(SmtpSecurity::StartTls);
    }
}

fn apply_smtp_overrides(target: &mut SmtpConfig, source: FileSmtpConfig) {
    if let Some(host) = source.host {
        target.host = host;
    }
    if let Some(port) = source.port {
        target.port = port;
    }
    if let Some(username) = source.username {
        target.username = Some(username);
    }
    if let Some(password) = source.password {
        target.password = Some(password);
    }
    if let Some(from) = source.from {
        target.from = Some(from);
    }
    if let Some(to) = source.to {
        target.to = to;
    }
    if let Some(security) = source.security {
        target.security = parse_smtp_security(&security).unwrap_or(SmtpSecurity::StartTls);
    }
}

fn parse_smtp_security(value: &str) -> Option<SmtpSecurity> {
    match value.to_ascii_lowercase().as_str() {
        "plain" => Some(SmtpSecurity::Plain),
        "starttls" => Some(SmtpSecurity::StartTls),
        "tls" => Some(SmtpSecurity::Tls),
        _ => None,
    }
}

fn parse_tui_source(value: &str) -> Option<TuiSource> {
    match value.to_ascii_lowercase().as_str() {
        "sqlite" | "local" | "local-sqlite" => Some(TuiSource::LocalSqlite),
        "http" | "https" | "remote" | "remote-api" => Some(TuiSource::RemoteApi),
        _ => None,
    }
}

fn redact_option<T>(value: &Option<T>) -> String {
    if value.is_some() {
        "Some(<redacted>)".to_string()
    } else {
        "None".to_string()
    }
}

fn redact_vec(values: &[String]) -> String {
    if values.is_empty() {
        "[]".to_string()
    } else {
        format!("[<redacted>; {}]", values.len())
    }
}

fn load_file_config(path: &Path) -> Result<FileConfig> {
    if !path.exists() {
        warn!(path = %path.display(), "config does not exist, use default config instead");
        return Ok(FileConfig::default());
    }

    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    toml::from_str(&contents)
        .with_context(|| format!("failed to parse config file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command, ServeArgs, TuiArgs};
    use tempfile::tempdir;

    fn missing_config_path() -> PathBuf {
        tempdir()
            .expect("tempdir")
            .keep()
            .join("missing-config.toml")
    }

    #[test]
    fn tui_defaults_to_local_sqlite() {
        let cli = Cli {
            config: Some(missing_config_path()),
            db: None,
            command: Some(Command::Tui(TuiArgs::default())),
        };
        let loaded = LoadedConfig::load(&cli).expect("load config");
        assert!(matches!(loaded.config.tui.source, TuiSource::LocalSqlite));
    }

    #[test]
    fn tui_switches_to_remote_when_api_base_url_is_set() {
        let cli = Cli {
            config: Some(missing_config_path()),
            db: None,
            command: Some(Command::Tui(TuiArgs {
                refresh_seconds: None,
                source: None,
                api_base_url: Some("https://example.com".to_string()),
            })),
        };
        let loaded = LoadedConfig::load(&cli).expect("load config");
        assert!(matches!(loaded.config.tui.source, TuiSource::RemoteApi));
        assert_eq!(loaded.config.tui.api_base_url, "https://example.com");
    }

    #[test]
    fn explicit_sqlite_source_keeps_local_reads() {
        let cli = Cli {
            config: Some(missing_config_path()),
            db: None,
            command: Some(Command::Tui(TuiArgs {
                refresh_seconds: None,
                source: Some("sqlite".to_string()),
                api_base_url: Some("https://example.com".to_string()),
            })),
        };
        let loaded = LoadedConfig::load(&cli).expect("load config");
        assert!(matches!(loaded.config.tui.source, TuiSource::LocalSqlite));
        assert_eq!(loaded.config.tui.api_base_url, "https://example.com");
    }

    #[test]
    fn loads_grouped_serve_and_tui_sections() {
        let dir = tempdir().expect("tempdir");
        let config_path = dir.path().join("lb-monitor.toml");
        fs::write(
            &config_path,
            r#"
[database]
path = "shared.sqlite3"

[serve.fetch]
url = "https://example.com/leaderboard"
interval_seconds = 120

[serve.http]
listen = "0.0.0.0:9000"

[serve.mail]
enabled = true

[serve.mail.smtp]
host = "smtp.example.com"
port = 2525
from = "sender@example.com"
to = ["alpha@example.com", "beta@example.com"]
security = "plain"

[tui]
refresh_seconds = 9
source = "remote-api"
api_base_url = "https://api.example.com"
database_path = "tui.sqlite3"
"#,
        )
        .expect("write config");

        let cli = Cli {
            config: Some(config_path),
            db: None,
            command: Some(Command::Tui(TuiArgs::default())),
        };
        let loaded = LoadedConfig::load(&cli).expect("load config");

        assert_eq!(loaded.config.database.path, PathBuf::from("shared.sqlite3"));
        assert_eq!(
            loaded.config.serve.fetch.url,
            "https://example.com/leaderboard"
        );
        assert_eq!(loaded.config.serve.fetch.interval_seconds, 120);
        assert_eq!(loaded.config.serve.http.listen, "0.0.0.0:9000");
        assert!(loaded.config.serve.mail.enabled);
        assert_eq!(loaded.config.serve.mail.smtp.host, "smtp.example.com");
        assert_eq!(loaded.config.serve.mail.smtp.port, 2525);
        assert_eq!(
            loaded.config.serve.mail.smtp.to,
            vec![
                "alpha@example.com".to_string(),
                "beta@example.com".to_string()
            ]
        );
        assert!(matches!(
            loaded.config.serve.mail.smtp.security,
            SmtpSecurity::Plain
        ));
        assert_eq!(loaded.config.tui.refresh_seconds, 9);
        assert!(matches!(loaded.config.tui.source, TuiSource::RemoteApi));
        assert_eq!(loaded.config.tui.api_base_url, "https://api.example.com");
        assert_eq!(
            loaded.config.tui.database_path,
            PathBuf::from("tui.sqlite3")
        );
    }

    #[test]
    fn redacted_summary_uses_effective_serve_config() {
        let cli = Cli {
            config: Some(missing_config_path()),
            db: None,
            command: Some(Command::Serve(ServeArgs {
                interval: None,
                listen: None,
                once: false,
                notify: false,
                no_notify: false,
                smtp_host: None,
                smtp_port: None,
                smtp_username: None,
                smtp_password: None,
                smtp_from: None,
                smtp_to: Vec::new(),
                smtp_security: None,
            })),
        };
        let loaded = LoadedConfig::load(&cli).expect("load config");
        let summary = loaded
            .config
            .redacted_command_summary(cli.command.as_ref().expect("command"));

        assert!(summary.contains("interval_seconds: 300"));
        assert!(summary.contains("listen:"));
        assert!(summary.contains(&loaded.config.serve.http.listen));
        assert!(summary.contains("smtp_port: 465"));
        assert!(summary.contains("smtp_security: Tls"));
        assert!(!summary.contains("interval: None"));
        assert!(!summary.contains("listen: None"));
    }
}
