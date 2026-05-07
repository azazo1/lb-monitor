mod cli;
mod config;
mod db;
mod diff;
mod fetch;
mod notify;
mod parse;
mod tui;

use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::config::LoadedConfig;
use crate::db::{insert_snapshot, open_rw, previous_snapshot_rows};
use crate::diff::diff_rows;
use crate::fetch::fetch_leaderboard;
use crate::notify::{NoopNotifier, Notifier, SystemNotifier};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let loaded = LoadedConfig::load(&cli)?;

    match cli.command.unwrap_or(Command::Tui(Default::default())) {
        Command::Tui(_) => tui::run(&loaded.config),
        Command::Serve(args) => serve(&loaded.config, args.once),
    }
}

fn serve(config: &config::Config, once: bool) -> Result<()> {
    let notifier: Box<dyn Notifier> = if config.notify.enabled {
        Box::new(SystemNotifier)
    } else {
        Box::new(NoopNotifier)
    };

    let mut conn = open_rw(&config.database.path)?;
    loop {
        run_fetch_cycle(&mut conn, config, notifier.as_ref())?;
        if once {
            break;
        }
        thread::sleep(Duration::from_secs(config.fetch.interval_seconds.max(1)));
    }
    Ok(())
}

fn run_fetch_cycle(
    conn: &mut rusqlite::Connection,
    config: &config::Config,
    notifier: &dyn Notifier,
) -> Result<bool> {
    let page = fetch_leaderboard(&config.fetch.url)?;
    let previous = previous_snapshot_rows(conn)?;
    let diff = diff_rows(&previous, &page.rows, page.source_updated_at.as_deref());

    if !diff.changed {
        return Ok(false);
    }

    let is_initial_snapshot = previous.is_empty();
    let fetched_at = insert_snapshot(conn, page.source_updated_at.as_deref(), &page.rows, &diff)
        .context("failed to persist leaderboard snapshot")?;

    if !is_initial_snapshot {
        let body = format!(
            "Detected {} team changes at {}",
            diff.events.len(),
            fetched_at
        );
        notifier.notify_update(&body)?;
    }

    Ok(true)
}
