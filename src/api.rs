use std::collections::HashMap;
use std::path::PathBuf;
use std::thread;
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::runtime::Builder;

use crate::db::{ChartPoint, EventViewRow, LeaderboardViewRow};
use crate::query::{
    SnapshotMeta, SnapshotPolicy, load_chart_series, load_leaderboard_state, load_recent_events,
    load_snapshot_meta,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiState {
    pub snapshot: SnapshotMeta,
    pub leaderboard: Vec<LeaderboardViewRow>,
}

#[derive(Debug, Clone)]
pub struct ApiClient {
    base_url: String,
    client: Client,
}

impl ApiClient {
    pub fn new(base_url: &str) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .user_agent("lb-monitor/0.1.0")
            .build()
            .context("failed to build api client")?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
        })
    }

    pub fn state(&self) -> Result<ApiState> {
        self.get_json("/api/v1/state")
    }

    pub fn events(&self, team: Option<&str>, limit: usize) -> Result<Vec<EventViewRow>> {
        let mut path = format!("/api/v1/events?limit={limit}");
        if let Some(team) = team {
            path.push_str("&team=");
            path.push_str(team);
        }
        self.get_json(&path)
    }

    pub fn chart(&self, team_ids: &[String]) -> Result<HashMap<String, Vec<ChartPoint>>> {
        let mut path = String::from("/api/v1/chart");
        if !team_ids.is_empty() {
            path.push_str("?team_ids=");
            path.push_str(&team_ids.join(","));
        }
        self.get_json(&path)
    }

    fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .client
            .get(&url)
            .send()
            .and_then(|response| response.error_for_status())
            .with_context(|| format!("failed to fetch {url}"))?;
        response
            .json::<T>()
            .with_context(|| format!("failed to decode json from {url}"))
    }
}

#[derive(Clone)]
struct ApiAppState {
    db_path: PathBuf,
}

pub fn spawn_http_server(db_path: PathBuf, listen: &str) -> Result<JoinHandle<()>> {
    let listen = listen.to_string();
    Ok(thread::spawn(move || {
        let runtime = match Builder::new_multi_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("failed to start api runtime: {error}");
                return;
            }
        };
        runtime.block_on(async move {
            if let Err(error) = serve_http(db_path, &listen).await {
                eprintln!("api server stopped: {error}");
            }
        });
    }))
}

pub async fn serve_http(db_path: PathBuf, listen: &str) -> Result<()> {
    let state = ApiAppState { db_path };
    let app = Router::new()
        .route("/api/v1/state", get(get_state))
        .route("/api/v1/snapshot", get(get_snapshot))
        .route("/api/v1/leaderboard", get(get_leaderboard))
        .route("/api/v1/events", get(get_events))
        .route("/api/v1/chart", get(get_chart))
        .with_state(state);

    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("failed to bind api listener at {listen}"))?;
    axum::serve(listener, app)
        .await
        .context("api server stopped")
}

async fn get_state(State(state): State<ApiAppState>) -> Result<Json<ApiState>, ApiError> {
    let snapshot = load_snapshot_meta(&state.db_path)?;
    let leaderboard =
        load_leaderboard_state(&state.db_path, SnapshotPolicy::AllowEmpty)?.leaderboard;
    Ok(Json(ApiState {
        snapshot,
        leaderboard,
    }))
}

async fn get_snapshot(State(state): State<ApiAppState>) -> Result<Json<SnapshotMeta>, ApiError> {
    Ok(Json(load_snapshot_meta(&state.db_path)?))
}

async fn get_leaderboard(
    State(state): State<ApiAppState>,
) -> Result<Json<Vec<LeaderboardViewRow>>, ApiError> {
    Ok(Json(
        load_leaderboard_state(&state.db_path, SnapshotPolicy::AllowEmpty)?.leaderboard,
    ))
}

async fn get_events(
    State(state): State<ApiAppState>,
    Query(query): Query<EventQuery>,
) -> Result<Json<Vec<EventViewRow>>, ApiError> {
    let limit = query.limit.unwrap_or(100);
    Ok(Json(load_recent_events(
        &state.db_path,
        query.team.as_deref(),
        limit,
        SnapshotPolicy::AllowEmpty,
    )?))
}

async fn get_chart(
    State(state): State<ApiAppState>,
    Query(query): Query<ChartQuery>,
) -> Result<Json<HashMap<String, Vec<ChartPoint>>>, ApiError> {
    let team_ids = query
        .team_ids
        .map(|value| {
            value
                .split(',')
                .filter(|item| !item.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(Json(load_chart_series(
        &state.db_path,
        &team_ids,
        SnapshotPolicy::AllowEmpty,
    )?))
}

#[derive(Debug, Default, Deserialize)]
struct EventQuery {
    team: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
struct ChartQuery {
    team_ids: Option<String>,
}

#[derive(Debug)]
struct ApiError(anyhow::Error);

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(error: E) -> Self {
        Self(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = serde_json::json!({ "error": self.0.to_string() });
        (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
    }
}
use axum::response::IntoResponse;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{insert_snapshot, open_rw};
    use crate::diff::diff_rows;
    use crate::parse::LeaderboardRow;
    use tempfile::tempdir;
    use tower::ServiceExt;

    #[tokio::test]
    async fn serves_state_over_router() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("api.sqlite3");
        let mut conn = open_rw(&db_path).expect("open db");
        let rows = vec![LeaderboardRow {
            rank: 1,
            team_id: "alpha".to_string(),
            score: 1.0,
            version: "v1".to_string(),
        }];
        let diff = diff_rows(&HashMap::new(), &rows, Some("2026-05-07"));
        insert_snapshot(&mut conn, Some("2026-05-07"), &rows, &diff).expect("insert snapshot");

        let app = Router::new()
            .route("/api/v1/state", get(get_state))
            .with_state(ApiAppState { db_path });

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/v1/state")
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }
}
