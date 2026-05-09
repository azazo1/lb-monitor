use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::db::{
    ChartPoint, EventViewRow, LeaderboardViewRow, assert_has_snapshots, latest_leaderboard,
    latest_snapshot_id, open_ro, recent_events, team_chart_series,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotPolicy {
    AllowEmpty,
    RequireExisting,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotMeta {
    pub latest_snapshot_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LeaderboardState {
    pub latest_snapshot_id: Option<i64>,
    pub leaderboard: Vec<LeaderboardViewRow>,
}

pub fn load_leaderboard_state(db_path: &Path, policy: SnapshotPolicy) -> Result<LeaderboardState> {
    let span = tracing::info_span!("load_leaderboard_state", db_path = %db_path.display());
    let _entered = span.enter();
    let conn = open_query_connection(db_path, policy)?;
    Ok(LeaderboardState {
        latest_snapshot_id: latest_snapshot_id(&conn)?,
        leaderboard: latest_leaderboard(&conn)?,
    })
}

pub fn load_recent_events(
    db_path: &Path,
    team_filter: Option<&str>,
    limit: usize,
    policy: SnapshotPolicy,
) -> Result<Vec<EventViewRow>> {
    let span = tracing::info_span!("load_recent_events", db_path = %db_path.display(), limit);
    let _entered = span.enter();
    let conn = open_query_connection(db_path, policy)?;
    recent_events(&conn, team_filter, limit)
}

pub fn load_chart_series(
    db_path: &Path,
    team_ids: &[String],
    policy: SnapshotPolicy,
) -> Result<HashMap<String, Vec<ChartPoint>>> {
    let span = tracing::info_span!("load_chart_series", db_path = %db_path.display(), team_count = team_ids.len());
    let _entered = span.enter();
    let conn = open_query_connection(db_path, policy)?;
    team_chart_series(&conn, team_ids)
}

pub fn load_snapshot_meta(db_path: &Path) -> Result<SnapshotMeta> {
    let span = tracing::info_span!("load_snapshot_meta", db_path = %db_path.display());
    let _entered = span.enter();
    let conn = open_ro(db_path)?;
    Ok(SnapshotMeta {
        latest_snapshot_id: latest_snapshot_id(&conn)?,
    })
}

fn open_query_connection(db_path: &Path, policy: SnapshotPolicy) -> Result<rusqlite::Connection> {
    let span = tracing::info_span!(
        "open_query_connection",
        db_path = %db_path.display(),
        allow_empty = matches!(policy, SnapshotPolicy::AllowEmpty)
    );
    let _entered = span.enter();
    let conn = open_ro(db_path)?;
    if matches!(policy, SnapshotPolicy::RequireExisting) {
        assert_has_snapshots(&conn)?;
    }
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{insert_snapshot, open_rw};
    use crate::diff::diff_rows;
    use crate::parse::LeaderboardRow;
    use tempfile::tempdir;

    #[test]
    fn loads_state_from_sqlite() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("query.sqlite3");
        let mut conn = open_rw(&db_path).expect("open db");
        let rows = vec![LeaderboardRow {
            rank: 1,
            team_id: "alpha".to_string(),
            score: 1.0,
            version: "v1".to_string(),
        }];
        let diff = diff_rows(&HashMap::new(), &rows, Some("2026-05-07"));
        insert_snapshot(&mut conn, Some("2026-05-07"), &rows, &diff).expect("insert snapshot");

        let state =
            load_leaderboard_state(&db_path, SnapshotPolicy::RequireExisting).expect("load state");
        assert_eq!(state.latest_snapshot_id, Some(1));
        assert_eq!(state.leaderboard.len(), 1);

        let snapshot = load_snapshot_meta(&db_path).expect("snapshot");
        assert_eq!(snapshot.latest_snapshot_id, Some(1));
    }
}
