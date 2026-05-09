use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use reqwest::Client;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tower_http::trace::TraceLayer;
use tracing::{error, info};

use crate::db::{ChartPoint, EventViewRow, LeaderboardState, LeaderboardViewRow, SnapshotMeta};
use crate::query::{
    SnapshotPolicy, load_chart_series, load_leaderboard_state, load_recent_events,
    load_snapshot_meta,
};

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

    pub async fn state(&self, snapshot_id: Option<i64>) -> Result<LeaderboardState> {
        let mut path = String::from("/api/v1/state");
        if let Some(snapshot_id) = snapshot_id {
            path.push_str(&format!("?snapshot_id={snapshot_id}"));
        }
        self.get_json(&path).await
    }

    pub async fn events(
        &self,
        snapshot_id: Option<i64>,
        team: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EventViewRow>> {
        let mut path = format!("/api/v1/events?limit={limit}");
        if let Some(snapshot_id) = snapshot_id {
            path.push_str(&format!("&snapshot_id={snapshot_id}"));
        }
        if let Some(team) = team {
            path.push_str("&team=");
            path.push_str(team);
        }
        self.get_json(&path).await
    }

    pub async fn chart(
        &self,
        snapshot_id: Option<i64>,
        team_ids: &[String],
    ) -> Result<HashMap<String, Vec<ChartPoint>>> {
        let mut path = String::from("/api/v1/chart");
        let mut has_query = false;
        if let Some(snapshot_id) = snapshot_id {
            path.push_str(&format!("?snapshot_id={snapshot_id}"));
            has_query = true;
        }
        if !team_ids.is_empty() {
            path.push_str(if has_query {
                "&team_ids="
            } else {
                "?team_ids="
            });
            path.push_str(&team_ids.join(","));
        }
        self.get_json(&path).await
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let span = tracing::info_span!("api_client_request", url = %url);
        let _entered = span.enter();
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("failed to fetch {url}"))?;
        let response = response
            .error_for_status()
            .with_context(|| format!("failed to fetch {url}"))?;
        response
            .json::<T>()
            .await
            .with_context(|| format!("failed to decode json from {url}"))
    }
}

#[derive(Clone)]
struct ApiAppState {
    db_path: PathBuf,
}

pub struct ApiServerHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<()>>,
}

impl ApiServerHandle {
    pub fn request_shutdown(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
    }

    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    pub async fn join(self) -> Result<()> {
        self.task.await.context("api server task join failed")?
    }
}

pub async fn spawn_http_server(db_path: PathBuf, listen: &str) -> Result<ApiServerHandle> {
    let state = ApiAppState { db_path };
    let app = Router::new()
        .route("/api/v1/state", get(get_state))
        .route("/api/v1/snapshot", get(get_snapshot))
        .route("/api/v1/leaderboard", get(get_leaderboard))
        .route("/api/v1/events", get(get_events))
        .route("/api/v1/chart", get(get_chart))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("failed to bind api listener at {listen}"))?;
    info!(listen = %listen, "api server listening");
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
            .context("api server stopped")
    });
    Ok(ApiServerHandle {
        shutdown_tx: Some(shutdown_tx),
        task,
    })
}

#[tracing::instrument(skip(state))]
async fn get_state(
    State(state): State<ApiAppState>,
    Query(query): Query<SnapshotQuery>,
) -> Result<Json<LeaderboardState>, ApiError> {
    let db_path = state.db_path.clone();
    Ok(Json(
        load_leaderboard_state(&db_path, SnapshotPolicy::AllowEmpty, query.snapshot_id).await?,
    ))
}

#[tracing::instrument(skip(state))]
async fn get_snapshot(
    State(state): State<ApiAppState>,
    Query(query): Query<SnapshotQuery>,
) -> Result<Json<SnapshotMeta>, ApiError> {
    let db_path = state.db_path.clone();
    Ok(Json(load_snapshot_meta(&db_path, query.snapshot_id).await?))
}

#[tracing::instrument(skip(state))]
async fn get_leaderboard(
    State(state): State<ApiAppState>,
    Query(query): Query<SnapshotQuery>,
) -> Result<Json<Vec<LeaderboardViewRow>>, ApiError> {
    let db_path = state.db_path.clone();
    let state =
        load_leaderboard_state(&db_path, SnapshotPolicy::AllowEmpty, query.snapshot_id).await?;
    Ok(Json(state.leaderboard))
}

#[tracing::instrument(skip(state))]
async fn get_events(
    State(state): State<ApiAppState>,
    Query(query): Query<EventQuery>,
) -> Result<Json<Vec<EventViewRow>>, ApiError> {
    let limit = query.limit.unwrap_or(100);
    let db_path = state.db_path.clone();
    Ok(Json(
        load_recent_events(
            &db_path,
            query.snapshot_id,
            query.team.as_deref(),
            limit,
            SnapshotPolicy::AllowEmpty,
        )
        .await?,
    ))
}

#[tracing::instrument(skip(state))]
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
    let db_path = state.db_path.clone();
    Ok(Json(
        load_chart_series(
            &db_path,
            &team_ids,
            query.snapshot_id,
            SnapshotPolicy::AllowEmpty,
        )
        .await?,
    ))
}

#[derive(Debug, Default, Deserialize)]
struct SnapshotQuery {
    snapshot_id: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
struct EventQuery {
    snapshot_id: Option<i64>,
    team: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
struct ChartQuery {
    snapshot_id: Option<i64>,
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
        error!(error = %self.0, "api request failed");
        let body = serde_json::json!({ "error": self.0.to_string() });
        (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
    }
}

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
