use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};

use crate::parse::LeaderboardRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    NewTeam,
    DroppedTeam,
    RankChanged,
    ScoreChanged,
    VersionChanged,
    MultiChanged,
}

#[derive(Debug, Clone)]
pub struct PreviousEntry {
    pub rank: Option<i64>,
    pub score: Option<f64>,
    pub version: Option<String>,
    pub present: bool,
}

#[derive(Debug, Clone)]
pub struct TeamEvent {
    pub team_id: String,
    pub event_type: EventType,
    pub old_rank: Option<i64>,
    pub new_rank: Option<i64>,
    pub old_score: Option<f64>,
    pub new_score: Option<f64>,
    pub old_version: Option<String>,
    pub new_version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DiffResult {
    pub changed: bool,
    pub content_hash: String,
    pub events: Vec<TeamEvent>,
    pub dropped_team_ids: Vec<String>,
}

pub fn diff_rows(
    previous: &HashMap<String, PreviousEntry>,
    current_rows: &[LeaderboardRow],
    source_updated_at: Option<&str>,
) -> DiffResult {
    let span = tracing::info_span!("diff_rows", current_count = current_rows.len());
    let _entered = span.enter();
    let content_hash = build_content_hash(current_rows, source_updated_at);
    let current_map: HashMap<&str, &LeaderboardRow> = current_rows
        .iter()
        .map(|row| (row.team_id.as_str(), row))
        .collect();
    let mut changed = previous.is_empty();
    let mut events = Vec::new();
    let mut dropped_team_ids = Vec::new();

    for row in current_rows {
        match previous.get(&row.team_id) {
            None => {
                changed = true;
                events.push(TeamEvent {
                    team_id: row.team_id.clone(),
                    event_type: EventType::NewTeam,
                    old_rank: None,
                    new_rank: Some(row.rank),
                    old_score: None,
                    new_score: Some(row.score),
                    old_version: None,
                    new_version: Some(row.version.clone()),
                });
            }
            Some(previous_entry) => {
                let mut field_changes = 0;
                if previous_entry.rank != Some(row.rank) {
                    field_changes += 1;
                }
                if previous_entry.score != Some(row.score) {
                    field_changes += 1;
                }
                if previous_entry.version.as_deref() != Some(row.version.as_str()) {
                    field_changes += 1;
                }
                if !previous_entry.present {
                    field_changes += 1;
                }
                if field_changes == 0 {
                    continue;
                }

                changed = true;
                let event_type = match field_changes {
                    1 if previous_entry.rank != Some(row.rank) => EventType::RankChanged,
                    1 if previous_entry.score != Some(row.score) => EventType::ScoreChanged,
                    1 if previous_entry.version.as_deref() != Some(row.version.as_str()) => {
                        EventType::VersionChanged
                    }
                    _ => EventType::MultiChanged,
                };
                events.push(TeamEvent {
                    team_id: row.team_id.clone(),
                    event_type,
                    old_rank: previous_entry.rank,
                    new_rank: Some(row.rank),
                    old_score: previous_entry.score,
                    new_score: Some(row.score),
                    old_version: previous_entry.version.clone(),
                    new_version: Some(row.version.clone()),
                });
            }
        }
    }

    let current_ids: HashSet<&str> = current_map.keys().copied().collect();
    for (team_id, entry) in previous {
        if entry.present && !current_ids.contains(team_id.as_str()) {
            changed = true;
            dropped_team_ids.push(team_id.clone());
            events.push(TeamEvent {
                team_id: team_id.clone(),
                event_type: EventType::DroppedTeam,
                old_rank: entry.rank,
                new_rank: None,
                old_score: entry.score,
                new_score: None,
                old_version: entry.version.clone(),
                new_version: None,
            });
        }
    }

    DiffResult {
        changed,
        content_hash,
        events,
        dropped_team_ids,
    }
}

fn build_content_hash(rows: &[LeaderboardRow], source_updated_at: Option<&str>) -> String {
    let span = tracing::info_span!("build_content_hash", row_count = rows.len());
    let _entered = span.enter();
    let mut hasher = Sha256::new();
    if let Some(source_updated_at) = source_updated_at {
        hasher.update(source_updated_at.as_bytes());
    }
    for row in rows {
        hasher.update(row.rank.to_le_bytes());
        hasher.update(row.team_id.as_bytes());
        hasher.update(row.score.to_le_bytes());
        hasher.update(row.version.as_bytes());
    }
    hex::encode(hasher.finalize())
}

impl EventType {
    pub fn as_str(self) -> &'static str {
        match self {
            EventType::NewTeam => "new_team",
            EventType::DroppedTeam => "dropped_team",
            EventType::RankChanged => "rank_changed",
            EventType::ScoreChanged => "score_changed",
            EventType::VersionChanged => "version_changed",
            EventType::MultiChanged => "multi_changed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(rank: i64, team_id: &str, score: f64, version: &str) -> LeaderboardRow {
        LeaderboardRow {
            rank,
            team_id: team_id.to_string(),
            score,
            version: version.to_string(),
        }
    }

    #[test]
    fn detects_new_and_changed_teams() {
        let previous = HashMap::from([
            (
                "alpha".to_string(),
                PreviousEntry {
                    rank: Some(2),
                    score: Some(95.0),
                    version: Some("v1".to_string()),
                    present: true,
                },
            ),
            (
                "beta".to_string(),
                PreviousEntry {
                    rank: Some(1),
                    score: Some(96.0),
                    version: Some("v1".to_string()),
                    present: true,
                },
            ),
        ]);
        let current = vec![row(1, "alpha", 97.0, "v1"), row(2, "gamma", 93.0, "v2")];

        let diff = diff_rows(&previous, &current, Some("2026-05-07"));
        assert!(diff.changed);
        assert_eq!(diff.events.len(), 3);
        assert!(diff.dropped_team_ids.contains(&"beta".to_string()));
    }
}
