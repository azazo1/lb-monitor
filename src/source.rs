use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use crate::api::ApiClient;
use crate::config::{Config, TuiSource};
use crate::db::{ChartPoint, EventViewRow};
use crate::query::{
    LeaderboardState, SnapshotPolicy, load_chart_series, load_leaderboard_state, load_recent_events,
};

pub trait TuiDataSource: Send + Sync {
    fn load_state(&self) -> Result<LeaderboardState>;
    fn load_events(&self, team_filter: Option<&str>, limit: usize) -> Result<Vec<EventViewRow>>;
    fn load_chart(&self, team_ids: &[String]) -> Result<HashMap<String, Vec<ChartPoint>>>;
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

impl TuiDataSource for SqliteTuiDataSource {
    fn load_state(&self) -> Result<LeaderboardState> {
        load_leaderboard_state(&self.db_path, SnapshotPolicy::RequireExisting)
    }

    fn load_events(&self, team_filter: Option<&str>, limit: usize) -> Result<Vec<EventViewRow>> {
        load_recent_events(
            &self.db_path,
            team_filter,
            limit,
            SnapshotPolicy::RequireExisting,
        )
    }

    fn load_chart(&self, team_ids: &[String]) -> Result<HashMap<String, Vec<ChartPoint>>> {
        load_chart_series(&self.db_path, team_ids, SnapshotPolicy::RequireExisting)
    }
}

struct HttpTuiDataSource {
    api: ApiClient,
}

impl TuiDataSource for HttpTuiDataSource {
    fn load_state(&self) -> Result<LeaderboardState> {
        let state = self.api.state()?;
        Ok(LeaderboardState {
            latest_snapshot_id: state.snapshot.latest_snapshot_id,
            leaderboard: state.leaderboard,
        })
    }

    fn load_events(&self, team_filter: Option<&str>, limit: usize) -> Result<Vec<EventViewRow>> {
        self.api.events(team_filter, limit)
    }

    fn load_chart(&self, team_ids: &[String]) -> Result<HashMap<String, Vec<ChartPoint>>> {
        self.api.chart(team_ids)
    }
}
