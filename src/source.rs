use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::api::ApiClient;
use crate::config::{Config, TuiSource};
use crate::db::{ChartPoint, EventViewRow, LeaderboardState};
use crate::query::{SnapshotPolicy, load_chart_series, load_leaderboard_state, load_recent_events};

#[async_trait]
pub trait TuiDataSource: Send + Sync {
    async fn load_state(&self, snapshot_id: Option<i64>) -> Result<LeaderboardState>;
    async fn load_events(
        &self,
        snapshot_id: Option<i64>,
        team_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EventViewRow>>;
    async fn load_chart(
        &self,
        snapshot_id: Option<i64>,
        team_ids: &[String],
    ) -> Result<HashMap<String, Vec<ChartPoint>>>;
}

pub fn build_tui_data_source(config: &Config) -> Result<Arc<dyn TuiDataSource>> {
    match config.tui.source {
        TuiSource::LocalSqlite => Ok(Arc::new(SqliteTuiDataSource {
            db_path: config.tui.database_path.clone(),
        })),
        TuiSource::RemoteApi => Ok(Arc::new(HttpTuiDataSource {
            api: ApiClient::new(&config.tui.api_base_url)?,
        })),
    }
}

struct SqliteTuiDataSource {
    db_path: PathBuf,
}

#[async_trait]
impl TuiDataSource for SqliteTuiDataSource {
    async fn load_state(&self, snapshot_id: Option<i64>) -> Result<LeaderboardState> {
        load_leaderboard_state(&self.db_path, SnapshotPolicy::RequireExisting, snapshot_id).await
    }

    async fn load_events(
        &self,
        snapshot_id: Option<i64>,
        team_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EventViewRow>> {
        load_recent_events(
            &self.db_path,
            snapshot_id,
            team_filter,
            limit,
            SnapshotPolicy::RequireExisting,
        )
        .await
    }

    async fn load_chart(
        &self,
        snapshot_id: Option<i64>,
        team_ids: &[String],
    ) -> Result<HashMap<String, Vec<ChartPoint>>> {
        load_chart_series(
            &self.db_path,
            team_ids,
            snapshot_id,
            SnapshotPolicy::RequireExisting,
        )
        .await
    }
}

struct HttpTuiDataSource {
    api: ApiClient,
}

#[async_trait]
impl TuiDataSource for HttpTuiDataSource {
    async fn load_state(&self, snapshot_id: Option<i64>) -> Result<LeaderboardState> {
        self.api.state(snapshot_id).await
    }

    async fn load_events(
        &self,
        snapshot_id: Option<i64>,
        team_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EventViewRow>> {
        self.api.events(snapshot_id, team_filter, limit).await
    }

    async fn load_chart(
        &self,
        snapshot_id: Option<i64>,
        team_ids: &[String],
    ) -> Result<HashMap<String, Vec<ChartPoint>>> {
        self.api.chart(snapshot_id, team_ids).await
    }
}
