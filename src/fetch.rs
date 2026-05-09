use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::Client;

use crate::parse::{
    LeaderboardPage, extract_bundle_path, parse_leaderboard, parse_leaderboard_bundle,
};

#[tracing::instrument(skip_all, fields(url = %url))]
pub async fn fetch_leaderboard(url: &str) -> Result<LeaderboardPage> {
    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("lb-monitor/0.1.0")
        .build()
        .context("failed to build HTTP client")?;
    let response = client
        .get(url)
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .with_context(|| format!("failed to fetch leaderboard from {url}"))?;
    let html = response
        .text()
        .await
        .context("failed to read leaderboard body")?;
    if let Ok(parsed) = parse_leaderboard(&html) {
        return Ok(parsed);
    }

    let bundle_path = extract_bundle_path(&html).context("failed to locate leaderboard bundle")?;
    let bundle_url = if bundle_path.starts_with("http://") || bundle_path.starts_with("https://") {
        bundle_path
    } else {
        let base = reqwest::Url::parse(url).context("invalid leaderboard url")?;
        base.join(&bundle_path)
            .context("failed to resolve leaderboard bundle url")?
            .to_string()
    };

    let bundle = client
        .get(&bundle_url)
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .with_context(|| format!("failed to fetch leaderboard bundle from {bundle_url}"))?
        .text()
        .await
        .context("failed to read leaderboard bundle body")?;

    parse_leaderboard_bundle(&bundle)
}
