mod api;
mod cli;
mod config;
mod db;
mod diff;
mod fetch;
mod notify;
mod parse;
mod query;
mod source;
mod tui;

use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::cli::{Cli, Command, DummyArgs};
use crate::config::LoadedConfig;
use crate::db::{insert_snapshot, open_rw, previous_snapshot_rows, replace_with_dummy_data};
use crate::diff::diff_rows;
use crate::fetch::fetch_leaderboard;
use crate::notify::{MailNotifier, NoopNotifier, Notifier};

fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let resolved_command = cli
        .command
        .clone()
        .unwrap_or(Command::Tui(Default::default()));
    let loaded = LoadedConfig::load(&cli)?;
    let command_summary = loaded.config.redacted_command_summary(&resolved_command);

    info!(command = %command_summary, "starting lb-monitor");
    match resolved_command {
        Command::Tui(_) => tui::run(&loaded.config),
        Command::Serve(args) => serve(&loaded.config, args.once),
        Command::Dummy(args) => dummy(&loaded.config, &args),
    }
}

fn serve(config: &config::Config, once: bool) -> Result<()> {
    info!(once, "starting serve loop");
    let notifier: Box<dyn Notifier> = if config.serve.mail.enabled {
        Box::new(MailNotifier::new(&config.serve.mail)?)
    } else {
        Box::new(NoopNotifier)
    };

    let mut conn = open_rw(&config.database.path)?;
    if once {
        run_fetch_cycle(&mut conn, config, notifier.as_ref())?;
        info!("completed single fetch cycle");
        return Ok(());
    }

    let _api_thread =
        api::spawn_http_server(config.database.path.clone(), &config.serve.http.listen)?;
    loop {
        if let Err(error) = run_fetch_cycle(&mut conn, config, notifier.as_ref()) {
            error!(%error, "fetch cycle failed");
        }
        std::thread::sleep(Duration::from_secs(
            config.serve.fetch.interval_seconds.max(1),
        ));
    }
}

fn run_fetch_cycle(
    conn: &mut rusqlite::Connection,
    config: &config::Config,
    notifier: &dyn Notifier,
) -> Result<bool> {
    let page = fetch_leaderboard(&config.serve.fetch.url)?;
    let previous = previous_snapshot_rows(conn)?;
    let diff = diff_rows(&previous, &page.rows, page.source_updated_at.as_deref());

    if !diff.changed {
        return Ok(false);
    }

    let is_initial_snapshot = previous.is_empty();
    let fetched_at = insert_snapshot(conn, page.source_updated_at.as_deref(), &page.rows, &diff)
        .context("failed to persist leaderboard snapshot")?;
    let (subject, body) = build_notification_message(
        is_initial_snapshot,
        &fetched_at,
        page.rows.len(),
        &diff.events,
    );
    notifier.notify_update(&subject, &body)?;
    if is_initial_snapshot {
        info!(
            teams = page.rows.len(),
            "initial leaderboard snapshot created"
        );
    } else {
        info!(changes = diff.events.len(), "leaderboard updated");
    }

    Ok(true)
}

fn build_notification_message(
    is_initial_snapshot: bool,
    fetched_at: &str,
    team_count: usize,
    events: &[crate::diff::TeamEvent],
) -> (String, String) {
    if is_initial_snapshot {
        return (
            format!(
                "Initial leaderboard snapshot created ({} teams)",
                team_count
            ),
            format_initial_mail_body(fetched_at, team_count, events),
        );
    }

    (
        format!("Leaderboard updated ({} changes)", events.len()),
        format_update_mail_body(fetched_at, events),
    )
}

fn format_initial_mail_body(
    fetched_at: &str,
    team_count: usize,
    events: &[crate::diff::TeamEvent],
) -> String {
    let mut lines = vec![
        format!("Initial leaderboard snapshot created at {fetched_at}"),
        format!("Tracked teams: {team_count}"),
        String::new(),
    ];
    lines.extend(format_event_lines(events));
    lines.join("\n")
}

fn format_update_mail_body(fetched_at: &str, events: &[crate::diff::TeamEvent]) -> String {
    let mut lines = vec![
        format!("Leaderboard updated at {fetched_at}"),
        String::new(),
    ];
    lines.extend(format_event_lines(events));
    lines.join("\n")
}

fn format_event_lines(events: &[crate::diff::TeamEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event.event_type {
            crate::diff::EventType::NewTeam => format!(
                "+ {} rank={} score={:.4} version={}",
                event.team_id,
                event.new_rank.unwrap_or_default(),
                event.new_score.unwrap_or_default(),
                event.new_version.as_deref().unwrap_or("-")
            ),
            crate::diff::EventType::DroppedTeam => format!(
                "- {} rank={} score={:.4} version={}",
                event.team_id,
                event.old_rank.unwrap_or_default(),
                event.old_score.unwrap_or_default(),
                event.old_version.as_deref().unwrap_or("-")
            ),
            crate::diff::EventType::RankChanged => format!(
                "~ {} rank {} -> {}",
                event.team_id,
                event.old_rank.unwrap_or_default(),
                event.new_rank.unwrap_or_default()
            ),
            crate::diff::EventType::ScoreChanged => format!(
                "~ {} score {:.4} -> {:.4}",
                event.team_id,
                event.old_score.unwrap_or_default(),
                event.new_score.unwrap_or_default()
            ),
            crate::diff::EventType::VersionChanged => format!(
                "~ {} version {} -> {}",
                event.team_id,
                event.old_version.as_deref().unwrap_or("-"),
                event.new_version.as_deref().unwrap_or("-")
            ),
            crate::diff::EventType::MultiChanged => format!(
                "~ {} rank {:?} score {:?} version {:?} -> {:?} {:?} {:?}",
                event.team_id,
                event.old_rank,
                event.old_score,
                event.old_version,
                event.new_rank,
                event.new_score,
                event.new_version
            ),
        })
        .collect()
}

fn dummy(config: &config::Config, args: &DummyArgs) -> Result<()> {
    let mut conn = open_rw(&config.database.path)?;
    replace_with_dummy_data(&mut conn, args.snapshots, args.teams)?;
    info!(
        snapshots = args.snapshots,
        teams = args.teams,
        "generated dummy database"
    );
    println!(
        "generated dummy database at {} with {} snapshots and {} teams",
        config.database.path.display(),
        args.snapshots.max(1),
        args.teams.max(3)
    );
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("lb_monitor=info,axum=info,tower_http=info,reqwest=warn")
    });
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false).compact());
    let _ = tracing::subscriber::set_global_default(subscriber);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{EventType, TeamEvent};

    #[test]
    fn formats_mail_summary() {
        let events = vec![TeamEvent {
            team_id: "alpha".to_string(),
            event_type: EventType::NewTeam,
            old_rank: None,
            new_rank: Some(1),
            old_score: None,
            new_score: Some(100.0),
            old_version: None,
            new_version: Some("v1".to_string()),
        }];

        let body = format_update_mail_body("2026-05-07T00:00:00Z", &events);
        assert!(body.contains("Leaderboard updated at"));
        assert!(body.contains("+ alpha"));
    }

    #[test]
    fn formats_initial_snapshot_mail_summary() {
        let events = vec![TeamEvent {
            team_id: "alpha".to_string(),
            event_type: EventType::NewTeam,
            old_rank: None,
            new_rank: Some(1),
            old_score: None,
            new_score: Some(100.0),
            old_version: None,
            new_version: Some("v1".to_string()),
        }];

        let (subject, body) = build_notification_message(true, "2026-05-07T00:00:00Z", 1, &events);
        assert!(subject.contains("Initial leaderboard snapshot created"));
        assert!(body.contains("Initial leaderboard snapshot created at"));
        assert!(body.contains("Tracked teams: 1"));
        assert!(body.contains("+ alpha"));
    }
}
