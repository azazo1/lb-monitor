use std::cmp::{max, min};
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Direction, Layout, Rect},
    prelude::{Color, Modifier, Style},
    symbols,
    text::Line,
    widgets::{
        Axis, Block, Borders, Cell, Chart, Dataset, GraphType, List, ListItem, Paragraph, Row,
        Table, TableState,
    },
};

use crate::config::Config;
use crate::db::{
    ChartPoint, EventViewRow, LeaderboardViewRow, assert_has_snapshots, latest_leaderboard,
    latest_snapshot_id, open_ro, recent_events, team_chart_series,
};

const RECENT_EVENTS_LIMIT: usize = 100;
const CHART_COLORS: [Color; 6] = [
    Color::Cyan,
    Color::Yellow,
    Color::Green,
    Color::Magenta,
    Color::Blue,
    Color::LightRed,
];

pub fn run(config: &Config) -> Result<()> {
    let conn = open_ro(&config.database.path)?;
    assert_has_snapshots(&conn)?;

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let terminal = ratatui::init();

    let result = run_app(terminal, conn, config.tui.refresh_seconds);
    ratatui::restore();
    disable_raw_mode()?;
    execute!(std::io::stdout(), LeaveAlternateScreen)?;
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Table,
    Events,
    Chart,
}

struct App {
    conn: rusqlite::Connection,
    latest_snapshot_id: Option<i64>,
    leaderboard: Vec<LeaderboardViewRow>,
    filtered_indices: Vec<usize>,
    events: Vec<EventViewRow>,
    selected_team: Option<String>,
    compare_teams: BTreeSet<String>,
    chart_series: HashMap<String, Vec<ChartPoint>>,
    focus: Focus,
    table_state: TableState,
    search_mode: bool,
    search_input: String,
    refresh_every: Duration,
    last_refresh: Instant,
    status: String,
}

impl App {
    fn new(conn: rusqlite::Connection, refresh_seconds: u64) -> Result<Self> {
        let mut app = Self {
            conn,
            latest_snapshot_id: None,
            leaderboard: Vec::new(),
            filtered_indices: Vec::new(),
            events: Vec::new(),
            selected_team: None,
            compare_teams: BTreeSet::new(),
            chart_series: HashMap::new(),
            focus: Focus::Table,
            table_state: TableState::default(),
            search_mode: false,
            search_input: String::new(),
            refresh_every: Duration::from_secs(refresh_seconds.max(1)),
            last_refresh: Instant::now() - Duration::from_secs(refresh_seconds.max(1)),
            status: String::new(),
        };
        app.reload(true)?;
        Ok(app)
    }

    fn reload(&mut self, force: bool) -> Result<()> {
        let current_snapshot_id = latest_snapshot_id(&self.conn)?;
        if !force && current_snapshot_id == self.latest_snapshot_id {
            return Ok(());
        }
        self.latest_snapshot_id = current_snapshot_id;
        self.leaderboard = latest_leaderboard(&self.conn)?;
        self.apply_filter();

        let selected_team = self.selected_team.clone();
        self.events = recent_events(&self.conn, selected_team.as_deref(), RECENT_EVENTS_LIMIT)?;
        self.reload_chart_series()?;
        self.last_refresh = Instant::now();
        self.status = format!(
            "snapshot={} teams={} compare={}",
            self.latest_snapshot_id.unwrap_or_default(),
            self.leaderboard.len(),
            self.compare_teams.len()
        );
        Ok(())
    }

    fn reload_chart_series(&mut self) -> Result<()> {
        let teams: Vec<String> = self.compare_teams.iter().cloned().collect();
        self.chart_series = team_chart_series(&self.conn, &teams)?;
        Ok(())
    }

    fn apply_filter(&mut self) {
        let query = self.search_input.to_lowercase();
        self.filtered_indices = self
            .leaderboard
            .iter()
            .enumerate()
            .filter(|(_, row)| row.team_id.to_lowercase().contains(&query))
            .map(|(idx, _)| idx)
            .collect();

        let selected = self
            .table_state
            .selected()
            .unwrap_or(0)
            .min(self.filtered_indices.len().saturating_sub(1));
        if self.filtered_indices.is_empty() {
            self.table_state.select(None);
            self.selected_team = None;
        } else {
            self.table_state.select(Some(selected));
            self.sync_selected_team();
        }
    }

    fn sync_selected_team(&mut self) {
        if let Some(selected) = self.table_state.selected()
            && let Some(idx) = self.filtered_indices.get(selected)
        {
            self.selected_team = Some(self.leaderboard[*idx].team_id.clone());
        }
    }

    fn move_selection(&mut self, delta: isize) -> Result<()> {
        if self.filtered_indices.is_empty() {
            return Ok(());
        }
        let current = self.table_state.selected().unwrap_or(0) as isize;
        let len = self.filtered_indices.len() as isize;
        let next = min(max(current + delta, 0), len - 1) as usize;
        self.table_state.select(Some(next));
        self.sync_selected_team();
        self.events = recent_events(&self.conn, self.selected_team.as_deref(), RECENT_EVENTS_LIMIT)?;
        Ok(())
    }

    fn toggle_compare(&mut self) -> Result<()> {
        let Some(team_id) = self.selected_team.clone() else {
            return Ok(());
        };
        if !self.compare_teams.insert(team_id.clone()) {
            self.compare_teams.remove(&team_id);
        }
        self.reload_chart_series()?;
        Ok(())
    }

    fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Table => Focus::Events,
            Focus::Events => Focus::Chart,
            Focus::Chart => Focus::Table,
        };
    }
}

fn run_app(mut terminal: DefaultTerminal, conn: rusqlite::Connection, refresh_seconds: u64) -> Result<()> {
    let mut app = App::new(conn, refresh_seconds)?;

    loop {
        terminal.draw(|frame| render(frame, &app))?;
        if event::poll(Duration::from_millis(200))? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if app.search_mode {
                match key.code {
                    KeyCode::Esc => {
                        app.search_mode = false;
                        app.search_input.clear();
                        app.apply_filter();
                        app.events =
                            recent_events(&app.conn, app.selected_team.as_deref(), RECENT_EVENTS_LIMIT)?;
                    }
                    KeyCode::Enter => {
                        app.search_mode = false;
                        app.apply_filter();
                        app.events =
                            recent_events(&app.conn, app.selected_team.as_deref(), RECENT_EVENTS_LIMIT)?;
                    }
                    KeyCode::Backspace => {
                        app.search_input.pop();
                        app.apply_filter();
                    }
                    KeyCode::Char(c) => {
                        app.search_input.push(c);
                        app.apply_filter();
                    }
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Char('r') => app.reload(true)?,
                KeyCode::Esc => {
                    if !app.search_input.is_empty() {
                        app.search_input.clear();
                        app.apply_filter();
                        app.events =
                            recent_events(&app.conn, app.selected_team.as_deref(), RECENT_EVENTS_LIMIT)?;
                    }
                }
                KeyCode::Char('/') => {
                    app.search_mode = true;
                    app.status = "search mode".to_string();
                }
                KeyCode::Tab => app.cycle_focus(),
                KeyCode::Down | KeyCode::Char('j') => app.move_selection(1)?,
                KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1)?,
                KeyCode::Enter => {
                    app.events =
                        recent_events(&app.conn, app.selected_team.as_deref(), RECENT_EVENTS_LIMIT)?;
                }
                KeyCode::Char(' ') => app.toggle_compare()?,
                _ => {}
            }
        }

        if app.last_refresh.elapsed() >= app.refresh_every {
            app.reload(false)?;
        }
    }

    Ok(())
}

fn render(frame: &mut Frame<'_>, app: &App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(37), Constraint::Length(3)])
        .split(frame.area());
    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(layout[1]);

    render_table(frame, layout[0], app);
    render_events(frame, bottom[0], app);
    render_chart(frame, bottom[1], app);
    render_status(frame, layout[2], app);
}

fn render_table(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let rows: Vec<Row<'_>> = app
        .filtered_indices
        .iter()
        .filter_map(|idx| app.leaderboard.get(*idx))
        .map(|row| {
            let delta = if row.is_new {
                "NEW".to_string()
            } else {
                format_rank_delta(row.rank_delta)
            };
            let score_delta = row
                .score_delta
                .map(format_score_delta)
                .unwrap_or_else(|| "-".to_string());
            let marker = if app.compare_teams.contains(&row.team_id) { "*" } else { " " };
            Row::new(vec![
                Cell::from(marker.to_string()),
                Cell::from(row.rank.to_string()),
                Cell::from(delta).style(rank_delta_style(row.is_new, row.rank_delta)),
                Cell::from(row.team_id.clone()),
                Cell::from(format!("{:.4}", row.score)),
                Cell::from(score_delta).style(score_delta_style(row.score_delta)),
                Cell::from(row.version.clone()),
                Cell::from(format_timestamp(&row.fetched_at)),
            ])
        })
        .collect();

    let title = if app.search_mode {
        format!("Leaderboard /{}", app.search_input)
    } else {
        "Leaderboard".to_string()
    };
    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Min(14),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(20),
        ],
    )
    .header(
        Row::new(vec!["*", "Rank", "Delta", "Team", "Score", "Score Delta", "Version", "Last Seen"])
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style(app.focus == Focus::Table)),
    )
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    .highlight_symbol(">> ");
    frame.render_stateful_widget(table, area, &mut app.table_state.clone());
}

fn render_events(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let title = match &app.selected_team {
        Some(team_id) => format!("Events {}", team_id),
        None => "Recent Events".to_string(),
    };
    let items: Vec<ListItem<'_>> = app
        .events
        .iter()
        .take(RECENT_EVENTS_LIMIT)
        .map(|event| ListItem::new(Line::from(format_event(event))))
        .collect();
    let list = List::new(items).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style(app.focus == Focus::Events)),
    );
    frame.render_widget(list, area);
}

fn render_chart(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let title = if app.compare_teams.is_empty() {
        "Score Chart (press Space to add teams)"
    } else {
        "Score Chart"
    };
    let mut chart_data = Vec::new();
    let mut min_x = i64::MAX;
    let mut max_x = i64::MIN;
    let mut max_y = 0.0_f64;
    let mut min_y = f64::MAX;

    for team_id in &app.compare_teams {
        let points = app.chart_series.get(team_id).cloned().unwrap_or_default();
        if points.is_empty() {
            continue;
        }
        let chart_points: Vec<(f64, f64)> = points
            .iter()
            .map(|point| {
                min_x = min(min_x, point.timestamp);
                max_x = max(max_x, point.timestamp);
                min_y = min_y.min(point.score);
                max_y = max_y.max(point.score);
                (point.timestamp as f64, point.score)
            })
            .collect();
        chart_data.push((team_id.clone(), chart_points));
    }

    if chart_data.is_empty() {
        let placeholder = Paragraph::new("No teams selected for comparison")
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(border_style(app.focus == Focus::Chart)),
            )
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(placeholder, area);
        return;
    }

    let datasets: Vec<Dataset<'_>> = chart_data
        .iter()
        .enumerate()
        .map(|(index, (team_id, points))| {
            Dataset::default()
                .name(team_id.clone())
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(CHART_COLORS[index % CHART_COLORS.len()]))
                .data(points.as_slice())
        })
        .collect();

    let x_labels = vec![
        format_timestamp_short_unix(min_x),
        format_timestamp_short_unix(max_x),
    ];
    let y_start = if min_y.is_finite() { min_y.floor() } else { 0.0 };
    let y_end = if max_y.is_finite() { max_y.ceil() } else { 1.0 };
    let chart = Chart::new(datasets)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_style(app.focus == Focus::Chart)),
        )
        .x_axis(
            Axis::default()
                .title("Fetched At")
                .bounds([min_x as f64, max_x as f64])
                .labels(x_labels.into_iter().map(Line::from).collect::<Vec<_>>()),
        )
        .y_axis(
            Axis::default()
                .title("Score")
                .bounds([y_start, y_end.max(y_start + 1.0)]),
        );
    frame.render_widget(chart, area);
}

fn render_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let selected = app
        .selected_team
        .as_deref()
        .unwrap_or("-");
    let help = format!(
        "selected={} focus={:?} search=\"{}\" | / search | Space compare | Enter team events | Tab focus | r reload | q quit | {}",
        selected,
        app.focus,
        app.search_input,
        app.status
    );
    let paragraph = Paragraph::new(help).block(Block::default().borders(Borders::ALL).title("Status"));
    frame.render_widget(paragraph, area);
}

fn border_style(active: bool) -> Style {
    if active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn format_rank_delta(delta: Option<i64>) -> String {
    match delta {
        Some(value) if value > 0 => format!("↑{}", value),
        Some(value) if value < 0 => format!("↓{}", value.abs()),
        Some(_) => "-".to_string(),
        None => "-".to_string(),
    }
}

fn rank_delta_style(is_new: bool, delta: Option<i64>) -> Style {
    if is_new {
        return Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    }

    match delta {
        Some(value) if value > 0 => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        Some(value) if value < 0 => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        _ => Style::default().fg(Color::DarkGray),
    }
}

fn format_score_delta(delta: f64) -> String {
    if delta > 0.0 {
        format!("+{delta:.4}")
    } else if delta < 0.0 {
        format!("{delta:.4}")
    } else {
        "-".to_string()
    }
}

fn score_delta_style(delta: Option<f64>) -> Style {
    match delta {
        Some(value) if value > 0.0 => Style::default().fg(Color::Green),
        Some(value) if value < 0.0 => Style::default().fg(Color::Red),
        _ => Style::default().fg(Color::DarkGray),
    }
}

fn format_timestamp(input: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(input)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| input.to_string())
}

fn format_timestamp_short_unix(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|dt| dt.format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|| timestamp.to_string())
}

fn format_event(event: &EventViewRow) -> String {
    let rank = match (event.old_rank, event.new_rank) {
        (Some(old_rank), Some(new_rank)) => format!("rank {old_rank}->{new_rank}"),
        (Some(old_rank), None) => format!("rank {old_rank}->OUT"),
        (None, Some(new_rank)) => format!("rank NEW->{new_rank}"),
        (None, None) => "-".to_string(),
    };
    let score = match (event.old_score, event.new_score) {
        (Some(old_score), Some(new_score)) => format!("score {old_score:.4}->{new_score:.4}"),
        (Some(old_score), None) => format!("score {old_score:.4}->OUT"),
        (None, Some(new_score)) => format!("score NEW->{new_score:.4}"),
        (None, None) => "-".to_string(),
    };
    let version = match (&event.old_version, &event.new_version) {
        (Some(old_version), Some(new_version)) if old_version != new_version => {
            format!("version {old_version}->{new_version}")
        }
        (None, Some(new_version)) => format!("version NEW->{new_version}"),
        (Some(old_version), None) => format!("version {old_version}->OUT"),
        _ => "-".to_string(),
    };
    format!(
        "{} {} {} | {} | {} | {}",
        format_timestamp(&event.fetched_at),
        event.team_id,
        event.event_type,
        rank,
        score,
        version
    )
}
