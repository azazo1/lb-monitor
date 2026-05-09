use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use tokio::task;

use crate::db::{
    ChartPoint, EventViewRow, LeaderboardState, SnapshotMeta, assert_has_snapshots,
    chart_series_to_snapshot, events_for_snapshot, leaderboard_state, open_ro, snapshot_meta,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotPolicy {
    AllowEmpty,
    RequireExisting,
}

pub async fn load_leaderboard_state(
    db_path: &Path,
    policy: SnapshotPolicy,
    snapshot_id: Option<i64>,
) -> Result<LeaderboardState> {
    let db_path = db_path.to_path_buf();
    let span = tracing::info_span!("load_leaderboard_state", db_path = %db_path.display());
    task::spawn_blocking(move || {
        let _entered = span.enter();
        let conn = open_query_connection(&db_path, policy)?;
        leaderboard_state(&conn, snapshot_id)
    })
    .await
    .context("load_leaderboard_state task join failed")?
}

pub async fn load_recent_events(
    db_path: &Path,
    snapshot_id: Option<i64>,
    team_filter: Option<&str>,
    limit: usize,
    policy: SnapshotPolicy,
) -> Result<Vec<EventViewRow>> {
    let db_path = db_path.to_path_buf();
    let team_filter = team_filter.map(ToOwned::to_owned);
    let span = tracing::info_span!("load_recent_events", db_path = %db_path.display(), limit);
    task::spawn_blocking(move || {
        let _entered = span.enter();
        let conn = open_query_connection(&db_path, policy)?;
        events_for_snapshot(&conn, snapshot_id, team_filter.as_deref(), limit)
    })
    .await
    .context("load_recent_events task join failed")?
}

pub async fn load_chart_series(
    db_path: &Path,
    team_ids: &[String],
    snapshot_id: Option<i64>,
    policy: SnapshotPolicy,
) -> Result<HashMap<String, Vec<ChartPoint>>> {
    let db_path = db_path.to_path_buf();
    let team_ids = team_ids.to_vec();
    let span = tracing::info_span!(
        "load_chart_series",
        db_path = %db_path.display(),
        team_count = team_ids.len()
    );
    task::spawn_blocking(move || {
        let _entered = span.enter();
        let conn = open_query_connection(&db_path, policy)?;
        chart_series_to_snapshot(&conn, &team_ids, snapshot_id)
    })
    .await
    .context("load_chart_series task join failed")?
}

pub async fn load_snapshot_meta(db_path: &Path, snapshot_id: Option<i64>) -> Result<SnapshotMeta> {
    let db_path = db_path.to_path_buf();
    let span = tracing::info_span!("load_snapshot_meta", db_path = %db_path.display());
    task::spawn_blocking(move || {
        let _entered = span.enter();
        let conn = open_ro(&db_path)?;
        snapshot_meta(&conn, snapshot_id)
    })
    .await
    .context("load_snapshot_meta task join failed")?
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

    #[tokio::test]
    async fn loads_state_from_sqlite() {
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

        let state = load_leaderboard_state(&db_path, SnapshotPolicy::RequireExisting, None)
            .await
            .expect("load state");
        assert_eq!(state.snapshot.current_snapshot_id, Some(1));
        assert_eq!(state.leaderboard.len(), 1);

        let snapshot = load_snapshot_meta(&db_path, None).await.expect("snapshot");
        assert_eq!(snapshot.latest_snapshot_id, Some(1));
    }
}
