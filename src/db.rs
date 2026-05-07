use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use chrono::{Duration, TimeZone, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use crate::diff::diff_rows;
use crate::diff::{DiffResult, PreviousEntry};
use crate::parse::LeaderboardRow;

#[derive(Debug, Clone)]
pub struct LeaderboardViewRow {
    pub team_id: String,
    pub rank: i64,
    pub score: f64,
    pub version: String,
    pub fetched_at: String,
    pub rank_delta: Option<i64>,
    pub score_delta: Option<f64>,
    pub is_new: bool,
}

#[derive(Debug, Clone)]
pub struct EventViewRow {
    pub fetched_at: String,
    pub team_id: String,
    pub event_type: String,
    pub old_rank: Option<i64>,
    pub new_rank: Option<i64>,
    pub old_score: Option<f64>,
    pub new_score: Option<f64>,
    pub old_version: Option<String>,
    pub new_version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChartPoint {
    pub timestamp: i64,
    pub rank: i64,
    pub score: f64,
}

pub fn open_rw(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)
        .with_context(|| format!("failed to open sqlite database {}", path.display()))?;
    init_db(&conn)?;
    Ok(conn)
}

pub fn open_ro(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("failed to open sqlite database {}", path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL").ok();
    Ok(conn)
}

pub fn init_db(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")
        .context("failed to enable sqlite WAL mode")?;
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    fetched_at TEXT NOT NULL,
    source_updated_at TEXT,
    content_hash TEXT NOT NULL,
    row_count INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS teams (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    external_team_id TEXT NOT NULL UNIQUE,
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS snapshot_entries (
    snapshot_id INTEGER NOT NULL,
    team_id INTEGER NOT NULL,
    rank INTEGER,
    score REAL,
    version TEXT,
    present INTEGER NOT NULL,
    PRIMARY KEY (snapshot_id, team_id),
    FOREIGN KEY (snapshot_id) REFERENCES snapshots(id),
    FOREIGN KEY (team_id) REFERENCES teams(id)
);

CREATE TABLE IF NOT EXISTS team_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_id INTEGER NOT NULL,
    team_id INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    old_rank INTEGER,
    new_rank INTEGER,
    old_score REAL,
    new_score REAL,
    old_version TEXT,
    new_version TEXT,
    FOREIGN KEY (snapshot_id) REFERENCES snapshots(id),
    FOREIGN KEY (team_id) REFERENCES teams(id)
);

CREATE INDEX IF NOT EXISTS idx_snapshot_entries_snapshot_id ON snapshot_entries(snapshot_id);
CREATE INDEX IF NOT EXISTS idx_snapshot_entries_team_id ON snapshot_entries(team_id);
CREATE INDEX IF NOT EXISTS idx_team_events_snapshot_id ON team_events(snapshot_id);
CREATE INDEX IF NOT EXISTS idx_team_events_team_id ON team_events(team_id);
"#,
    )
    .context("failed to initialize sqlite schema")?;
    Ok(())
}

pub fn latest_snapshot_id(conn: &Connection) -> Result<Option<i64>> {
    conn.query_row(
        "SELECT id FROM snapshots ORDER BY id DESC LIMIT 1",
        [],
        |row| row.get(0),
    )
    .optional()
    .context("failed to query latest snapshot id")
}

pub fn previous_snapshot_rows(conn: &Connection) -> Result<HashMap<String, PreviousEntry>> {
    let Some(snapshot_id) = latest_snapshot_id(conn)? else {
        return Ok(HashMap::new());
    };

    let mut statement = conn.prepare(
        r#"
SELECT t.external_team_id, se.rank, se.score, se.version, se.present
FROM snapshot_entries se
JOIN teams t ON t.id = se.team_id
WHERE se.snapshot_id = ?
"#,
    )?;

    let rows = statement.query_map([snapshot_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            PreviousEntry {
                rank: row.get(1)?,
                score: row.get(2)?,
                version: row.get(3)?,
                present: row.get::<_, i64>(4)? == 1,
            },
        ))
    })?;

    let mut result = HashMap::new();
    for row in rows {
        let (team_id, entry) = row?;
        result.insert(team_id, entry);
    }
    Ok(result)
}

pub fn insert_snapshot(
    conn: &mut Connection,
    source_updated_at: Option<&str>,
    rows: &[LeaderboardRow],
    diff: &DiffResult,
) -> Result<String> {
    let now = Utc::now().to_rfc3339();
    insert_snapshot_at(conn, source_updated_at, rows, diff, &now)
}

fn insert_snapshot_at(
    conn: &mut Connection,
    source_updated_at: Option<&str>,
    rows: &[LeaderboardRow],
    diff: &DiffResult,
    fetched_at: &str,
) -> Result<String> {
    let transaction = conn.transaction().context("failed to start transaction")?;
    transaction.execute(
        r#"
INSERT INTO snapshots (fetched_at, source_updated_at, content_hash, row_count)
VALUES (?1, ?2, ?3, ?4)
"#,
        params![
            fetched_at,
            source_updated_at,
            diff.content_hash,
            rows.len() as i64
        ],
    )?;
    let snapshot_id = transaction.last_insert_rowid();

    let mut team_ids = HashMap::new();
    for row in rows {
        let team_id = upsert_team(&transaction, &row.team_id, fetched_at)?;
        team_ids.insert(row.team_id.clone(), team_id);
        transaction.execute(
            r#"
INSERT INTO snapshot_entries (snapshot_id, team_id, rank, score, version, present)
VALUES (?1, ?2, ?3, ?4, ?5, 1)
"#,
            params![snapshot_id, team_id, row.rank, row.score, row.version],
        )?;
    }

    for dropped in &diff.dropped_team_ids {
        let team_id = upsert_team(&transaction, dropped, fetched_at)?;
        transaction.execute(
            r#"
INSERT INTO snapshot_entries (snapshot_id, team_id, rank, score, version, present)
VALUES (?1, ?2, NULL, NULL, NULL, 0)
"#,
            params![snapshot_id, team_id],
        )?;
    }

    for event in &diff.events {
        let team_id = if let Some(team_id) = team_ids.get(&event.team_id) {
            *team_id
        } else {
            upsert_team(&transaction, &event.team_id, fetched_at)?
        };
        transaction.execute(
            r#"
INSERT INTO team_events (
    snapshot_id, team_id, event_type, old_rank, new_rank, old_score, new_score, old_version, new_version
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
"#,
            params![
                snapshot_id,
                team_id,
                event.event_type.as_str(),
                event.old_rank,
                event.new_rank,
                event.old_score,
                event.new_score,
                event.old_version,
                event.new_version
            ],
        )?;
    }

    transaction
        .commit()
        .context("failed to commit transaction")?;
    Ok(fetched_at.to_string())
}

fn upsert_team(conn: &Connection, team_id: &str, timestamp: &str) -> Result<i64> {
    conn.execute(
        r#"
INSERT INTO teams (external_team_id, first_seen_at, last_seen_at)
VALUES (?1, ?2, ?2)
ON CONFLICT(external_team_id) DO UPDATE SET last_seen_at = excluded.last_seen_at
"#,
        params![team_id, timestamp],
    )?;

    conn.query_row(
        "SELECT id FROM teams WHERE external_team_id = ?1",
        [team_id],
        |row| row.get(0),
    )
    .context("failed to fetch team id")
}

pub fn latest_leaderboard(conn: &Connection) -> Result<Vec<LeaderboardViewRow>> {
    let Some(snapshot_id) = latest_snapshot_id(conn)? else {
        return Ok(Vec::new());
    };
    let previous_snapshot_id = conn
        .query_row(
            "SELECT id FROM snapshots WHERE id < ?1 ORDER BY id DESC LIMIT 1",
            [snapshot_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let mut previous = HashMap::new();
    if let Some(previous_snapshot_id) = previous_snapshot_id {
        let mut statement = conn.prepare(
            r#"
SELECT t.external_team_id, se.rank, se.score
FROM snapshot_entries se
JOIN teams t ON t.id = se.team_id
WHERE se.snapshot_id = ?1 AND se.present = 1
"#,
        )?;
        let rows = statement.query_map([previous_snapshot_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, i64>(1)?, row.get::<_, f64>(2)?),
            ))
        })?;
        for row in rows {
            let (team_id, values) = row?;
            previous.insert(team_id, values);
        }
    }

    let mut statement = conn.prepare(
        r#"
SELECT t.external_team_id, se.rank, se.score, se.version, s.fetched_at
FROM snapshot_entries se
JOIN teams t ON t.id = se.team_id
JOIN snapshots s ON s.id = se.snapshot_id
WHERE se.snapshot_id = ?1 AND se.present = 1
ORDER BY se.rank ASC, t.external_team_id ASC
"#,
    )?;
    let rows = statement.query_map([snapshot_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, f64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;

    let mut result = Vec::new();
    for row in rows {
        let (team_id, rank, score, version, fetched_at) = row?;
        let previous_values = previous.get(&team_id).copied();
        result.push(LeaderboardViewRow {
            team_id,
            rank,
            score,
            version,
            fetched_at,
            rank_delta: previous_values.map(|(old_rank, _)| old_rank - rank),
            score_delta: previous_values.map(|(_, old_score)| score - old_score),
            is_new: previous_values.is_none(),
        });
    }
    Ok(result)
}

pub fn recent_events(
    conn: &Connection,
    team_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<EventViewRow>> {
    let query = if team_filter.is_some() {
        r#"
SELECT s.fetched_at, t.external_team_id, te.event_type, te.old_rank, te.new_rank, te.old_score, te.new_score, te.old_version, te.new_version
FROM team_events te
JOIN snapshots s ON s.id = te.snapshot_id
JOIN teams t ON t.id = te.team_id
WHERE t.external_team_id = ?1
ORDER BY te.id DESC
LIMIT ?2
"#
    } else {
        r#"
SELECT s.fetched_at, t.external_team_id, te.event_type, te.old_rank, te.new_rank, te.old_score, te.new_score, te.old_version, te.new_version
FROM team_events te
JOIN snapshots s ON s.id = te.snapshot_id
JOIN teams t ON t.id = te.team_id
ORDER BY te.id DESC
LIMIT ?1
"#
    };
    let mut statement = conn.prepare(query)?;
    let rows = if let Some(team_id) = team_filter {
        statement.query_map(params![team_id, limit as i64], map_event_row)?
    } else {
        statement.query_map(params![limit as i64], map_event_row)?
    };
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

fn map_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventViewRow> {
    Ok(EventViewRow {
        fetched_at: row.get(0)?,
        team_id: row.get(1)?,
        event_type: row.get(2)?,
        old_rank: row.get(3)?,
        new_rank: row.get(4)?,
        old_score: row.get(5)?,
        new_score: row.get(6)?,
        old_version: row.get(7)?,
        new_version: row.get(8)?,
    })
}

pub fn team_chart_series(
    conn: &Connection,
    team_ids: &[String],
) -> Result<HashMap<String, Vec<ChartPoint>>> {
    let mut result = HashMap::new();
    for team_id in team_ids {
        let mut statement = conn.prepare(
            r#"
SELECT s.fetched_at, se.score
     , se.rank
FROM snapshot_entries se
JOIN teams t ON t.id = se.team_id
JOIN snapshots s ON s.id = se.snapshot_id
WHERE t.external_team_id = ?1 AND se.present = 1
ORDER BY s.id ASC
"#,
        )?;
        let rows = statement.query_map([team_id], |row| {
            let fetched_at: String = row.get(0)?;
            let parsed = chrono::DateTime::parse_from_rfc3339(&fetched_at).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok(ChartPoint {
                timestamp: parsed.timestamp(),
                score: row.get(1)?,
                rank: row.get(2)?,
            })
        })?;
        let mut series = Vec::new();
        for row in rows {
            series.push(row?);
        }
        result.insert(team_id.clone(), series);
    }
    Ok(result)
}

pub fn assert_has_snapshots(conn: &Connection) -> Result<()> {
    if latest_snapshot_id(conn)?.is_some() {
        Ok(())
    } else {
        Err(anyhow!(
            "database has no snapshots yet, start `lb-monitor serve` first"
        ))
    }
}

pub fn replace_with_dummy_data(
    conn: &mut Connection,
    snapshots: usize,
    teams: usize,
) -> Result<()> {
    let snapshots = snapshots.max(1);
    let teams = teams.max(3);
    conn.execute_batch(
        r#"
DELETE FROM team_events;
DELETE FROM snapshot_entries;
DELETE FROM snapshots;
DELETE FROM teams;
"#,
    )
    .context("failed to clear existing sqlite data before dummy generation")?;

    let mut previous = HashMap::new();
    let start = Utc
        .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
        .single()
        .expect("valid dummy seed time");

    for snapshot_idx in 0..snapshots {
        let fetched_at = start + Duration::hours((snapshot_idx as i64) * 6);
        let source_updated_at = fetched_at.format("%Y-%m-%d").to_string();
        let rows = build_dummy_rows(snapshot_idx, teams);
        let diff = diff_rows(&previous, &rows, Some(&source_updated_at));
        insert_snapshot_at(
            conn,
            Some(&source_updated_at),
            &rows,
            &diff,
            &fetched_at.to_rfc3339(),
        )?;
        previous = rows
            .iter()
            .map(|row| {
                (
                    row.team_id.clone(),
                    PreviousEntry {
                        rank: Some(row.rank),
                        score: Some(row.score),
                        version: Some(row.version.clone()),
                        present: true,
                    },
                )
            })
            .collect();
    }

    Ok(())
}

fn build_dummy_rows(snapshot_idx: usize, teams: usize) -> Vec<LeaderboardRow> {
    let active_count = (8 + snapshot_idx * 2).min(teams);
    let mut scored = Vec::new();

    for team_idx in 0..active_count {
        if active_count > 10
            && snapshot_idx > 4
            && snapshot_idx.is_multiple_of(5)
            && team_idx + 1 == active_count
        {
            continue;
        }

        let team_number = team_idx + 1;
        let team_id = format!("dummy-{team_number:04}");
        let tier = (teams.saturating_sub(team_idx)) as f64 / teams as f64;
        let trend = match team_idx % 4 {
            0 => snapshot_idx as f64 * 0.0022,
            1 => -(snapshot_idx as f64) * 0.0014,
            2 => snapshot_idx as f64 * 0.0011,
            _ => -(snapshot_idx as f64) * 0.0004,
        };
        let wave = (((snapshot_idx * (team_idx + 3)) % 11) as f64 - 5.0) / 900.0;
        let rivalry = if team_idx % 6 == 0 {
            snapshot_idx as f64 * 0.0009
        } else {
            0.0
        };
        let score = (0.12 + tier * 0.68 + trend + wave + rivalry).max(0.0);
        let version = format!("v{}", 1 + ((snapshot_idx + team_idx) / 4));
        scored.push((team_id, score, version));
    }

    scored.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });

    scored
        .into_iter()
        .enumerate()
        .map(|(idx, (team_id, score, version))| LeaderboardRow {
            rank: (idx + 1) as i64,
            team_id,
            score: (score * 10_000.0).round() / 10_000.0,
            version,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::diff::EventType;
    use crate::diff::diff_rows;
    use crate::parse::LeaderboardRow;

    #[test]
    fn stores_snapshot_and_query_views() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("test.sqlite3");
        let mut conn = open_rw(&db_path).expect("open db");
        let rows = vec![LeaderboardRow {
            rank: 1,
            team_id: "alpha".to_string(),
            score: 99.0,
            version: "v1".to_string(),
        }];
        let diff = diff_rows(&HashMap::new(), &rows, Some("2026-05-07"));
        insert_snapshot(&mut conn, Some("2026-05-07"), &rows, &diff).expect("insert snapshot");

        let board = latest_leaderboard(&conn).expect("query board");
        assert_eq!(board.len(), 1);
        assert_eq!(board[0].team_id, "alpha");

        let events = recent_events(&conn, Some("alpha"), 10).expect("query events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::NewTeam.as_str());
    }

    #[test]
    fn generates_dummy_database() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("dummy.sqlite3");
        let mut conn = open_rw(&db_path).expect("open db");

        replace_with_dummy_data(&mut conn, 6, 12).expect("generate dummy data");

        let snapshot_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM snapshots", [], |row| row.get(0))
            .expect("count snapshots");
        let team_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM teams", [], |row| row.get(0))
            .expect("count teams");

        assert_eq!(snapshot_count, 6);
        assert!(team_count >= 8);
        assert!(!latest_leaderboard(&conn).expect("leaderboard").is_empty());
    }
}
