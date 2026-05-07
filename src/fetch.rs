use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::blocking::Client;

use crate::parse::{LeaderboardPage, parse_leaderboard};

pub fn fetch_leaderboard(url: &str) -> Result<LeaderboardPage> {
    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("lb-monitor/0.1.0")
        .build()
        .context("failed to build HTTP client")?;
    let response = client
        .get(url)
        .send()
        .and_then(|response| response.error_for_status())
        .with_context(|| format!("failed to fetch leaderboard from {url}"))?;
    let html = response.text().context("failed to read leaderboard body")?;
    parse_leaderboard(&html)
}
