use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "lb-monitor",
    version,
    about = "Leaderboard monitor with SQLite-backed TUI"
)]
pub struct Cli {
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[arg(long, global = true)]
    pub db: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    Tui(TuiArgs),
    Serve(ServeArgs),
    Dummy(DummyArgs),
}

#[derive(Debug, Clone, Args, Default)]
pub struct TuiArgs {
    #[arg(long)]
    pub refresh_seconds: Option<u64>,
}

#[derive(Debug, Clone, Args)]
pub struct ServeArgs {
    #[arg(long)]
    pub interval: Option<u64>,
    #[arg(long)]
    pub once: bool,
    #[arg(long, default_value_t = false, conflicts_with = "no_notify")]
    pub notify: bool,
    #[arg(long, default_value_t = false, conflicts_with = "notify")]
    pub no_notify: bool,
}

#[derive(Debug, Clone, Args)]
pub struct DummyArgs {
    #[arg(long, default_value_t = 24)]
    pub snapshots: usize,
    #[arg(long, default_value_t = 18)]
    pub teams: usize,
}
