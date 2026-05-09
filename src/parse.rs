use anyhow::{Result, anyhow};
use chrono::NaiveDate;
use regex::Regex;
use scraper::{Html, Selector};

#[derive(Debug, Clone, PartialEq)]
pub struct LeaderboardPage {
    pub source_updated_at: Option<String>,
    pub rows: Vec<LeaderboardRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LeaderboardRow {
    pub rank: i64,
    pub team_id: String,
    pub score: f64,
    pub version: String,
}

pub fn parse_leaderboard(html: &str) -> Result<LeaderboardPage> {
    let span = tracing::info_span!("parse_leaderboard");
    let _entered = span.enter();
    let document = Html::parse_document(html);
    let source_updated_at = extract_source_updated_at(&document);
    let structured_rows = parse_structured_rows(&document);
    let rows = if structured_rows.is_empty() {
        parse_text_rows(&document)
    } else {
        structured_rows
    };

    if rows.is_empty() {
        return Err(anyhow!("no leaderboard rows found in page"));
    }

    Ok(LeaderboardPage {
        source_updated_at,
        rows,
    })
}

pub fn parse_leaderboard_bundle(bundle: &str) -> Result<LeaderboardPage> {
    let span = tracing::info_span!("parse_leaderboard_bundle");
    let _entered = span.enter();
    let source_updated_at = extract_source_updated_at_from_text(bundle);
    let rows = parse_bundle_rows(bundle);

    if rows.is_empty() {
        return Err(anyhow!("no leaderboard rows found in bundle"));
    }

    Ok(LeaderboardPage {
        source_updated_at,
        rows,
    })
}

pub fn extract_bundle_path(html: &str) -> Option<String> {
    let span = tracing::info_span!("extract_bundle_path");
    let _entered = span.enter();
    let document = Html::parse_document(html);
    let selector = Selector::parse(r#"script[src]"#).ok()?;

    document
        .select(&selector)
        .filter_map(|node| node.value().attr("src"))
        .find(|src| src.contains("/assets/index-") && src.ends_with(".js"))
        .map(ToOwned::to_owned)
}

fn extract_source_updated_at(document: &Html) -> Option<String> {
    let text = collect_text(document);
    extract_source_updated_at_from_text(&text)
}

fn extract_source_updated_at_from_text(text: &str) -> Option<String> {
    let date_regex =
        Regex::new(r"Latest(?:\s+Update\s+Date| update:)\s*([A-Za-z]+ \d{1,2}, \d{4})").ok()?;
    let captures = date_regex.captures(text)?;
    normalize_date(captures.get(1)?.as_str())
}

fn normalize_date(input: &str) -> Option<String> {
    NaiveDate::parse_from_str(input.trim(), "%B %d, %Y")
        .or_else(|_| NaiveDate::parse_from_str(input.trim(), "%B %-d, %Y"))
        .ok()
        .map(|date| date.format("%Y-%m-%d").to_string())
}

fn parse_structured_rows(document: &Html) -> Vec<LeaderboardRow> {
    let row_selector = Selector::parse("table tr").expect("selector");
    let cell_selector = Selector::parse("td").expect("selector");
    let mut rows = Vec::new();

    for row in document.select(&row_selector) {
        let cells: Vec<String> = row
            .select(&cell_selector)
            .map(|cell| cell.text().collect::<Vec<_>>().join(" ").trim().to_string())
            .filter(|value| !value.is_empty())
            .collect();
        if cells.len() < 4 {
            continue;
        }
        if let Some(parsed) = parse_cells(&cells) {
            rows.push(parsed);
        }
    }

    rows
}

fn parse_cells(cells: &[String]) -> Option<LeaderboardRow> {
    let rank = cells.first()?.parse().ok()?;
    let team_id = cells.get(1)?.to_string();
    let score = cells.get(2)?.replace(',', "").parse().ok()?;
    let version = cells.get(3)?.to_string();
    Some(LeaderboardRow {
        rank,
        team_id,
        score,
        version,
    })
}

fn parse_text_rows(document: &Html) -> Vec<LeaderboardRow> {
    let body_text = collect_text(document);
    let pattern = Regex::new(
        r"(?m)^\s*#?(\d+)\s+([A-Za-z0-9._-]+)\s+(-?\d+(?:\.\d+)?)\s+([A-Za-z0-9._-]+)\s*$",
    )
    .expect("regex");

    pattern
        .captures_iter(&body_text)
        .filter_map(|captures| {
            let rank = captures.get(1)?.as_str().parse().ok()?;
            let team_id = captures.get(2)?.as_str().to_string();
            let score = captures.get(3)?.as_str().parse().ok()?;
            let version = captures.get(4)?.as_str().to_string();
            Some(LeaderboardRow {
                rank,
                team_id,
                score,
                version,
            })
        })
        .collect()
}

fn parse_bundle_rows(bundle: &str) -> Vec<LeaderboardRow> {
    let Some(array_match) = Regex::new(r#"const OT=\[(?s)(.*?)\];function"#)
        .expect("regex")
        .captures(bundle)
        .and_then(|captures| captures.get(1).map(|m| m.as_str().to_string()))
    else {
        return Vec::new();
    };

    let entry_regex =
        Regex::new(r#"\{rank:(\d+),team:"([^"]+)",score:([0-9.]+),version:"([^"]+)"\}"#)
            .expect("regex");

    entry_regex
        .captures_iter(&array_match)
        .filter_map(|captures| {
            let rank = captures.get(1)?.as_str().parse().ok()?;
            let team_id = captures.get(2)?.as_str().to_string();
            let score = captures.get(3)?.as_str().parse().ok()?;
            let version = captures.get(4)?.as_str().to_string();
            Some(LeaderboardRow {
                rank,
                team_id,
                score,
                version,
            })
        })
        .collect()
}

fn collect_text(document: &Html) -> String {
    document
        .root_element()
        .text()
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_fixture() {
        let html = r#"
<html>
  <body>
    <main>
      <h1>Leaderboard</h1>
      <p>Latest Update Date May 7, 2026</p>
      <pre>
Rank Team Score Version
1 team_1 95.5 v4
2 team_2 91.0 v3
      </pre>
    </main>
  </body>
</html>
"#;

        let parsed = parse_leaderboard(html).expect("parse leaderboard");
        assert_eq!(parsed.source_updated_at.as_deref(), Some("2026-05-07"));
        assert_eq!(parsed.rows.len(), 2);
        assert_eq!(parsed.rows[0].team_id, "team_1");
        assert_eq!(parsed.rows[1].version, "v3");
    }

    #[test]
    fn parses_current_leaderboard_text_format() {
        let html = r#"
<html>
  <body>
    <main>
      <h1>Leaderboard</h1>
      <p>Latest update: May 7, 2026</p>
      <div>
Rank Team Score Version
#1 1384 0.6311 v2
#2 1227 0.5906 v3
      </div>
    </main>
  </body>
</html>
"#;

        let parsed = parse_leaderboard(html).expect("parse leaderboard");
        assert_eq!(parsed.source_updated_at.as_deref(), Some("2026-05-07"));
        assert_eq!(parsed.rows.len(), 2);
        assert_eq!(parsed.rows[0].rank, 1);
        assert_eq!(parsed.rows[0].team_id, "1384");
        assert_eq!(parsed.rows[0].score, 0.6311);
        assert_eq!(parsed.rows[1].version, "v3");
    }

    #[test]
    fn extracts_bundle_path_and_rows() {
        let html = r#"
<html>
  <head>
    <script type="module" crossorigin src="/assets/index-CYRVxih_.js"></script>
  </head>
</html>
"#;
        let bundle = r#"
const OT=[{rank:1,team:"1384",score:.6311,version:"v2"},{rank:2,team:"1227",score:.5906,version:"v3"}];function zT(n){return n.toFixed(4)}
Latest update: May 7, 2026
"#;

        assert_eq!(
            extract_bundle_path(html).as_deref(),
            Some("/assets/index-CYRVxih_.js")
        );

        let parsed = parse_leaderboard_bundle(bundle).expect("parse leaderboard bundle");
        assert_eq!(parsed.source_updated_at.as_deref(), Some("2026-05-07"));
        assert_eq!(parsed.rows.len(), 2);
        assert_eq!(parsed.rows[0].team_id, "1384");
        assert_eq!(parsed.rows[1].score, 0.5906);
    }
}
