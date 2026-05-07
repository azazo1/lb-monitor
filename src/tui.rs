use std::cmp::{max, min};
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
        MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, size,
    },
};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Direction, Layout, Rect},
    prelude::{Color, Modifier, Style},
    symbols,
    text::Line,
    widgets::{
        Axis, Block, Borders, Cell, Chart, Clear, Dataset, GraphType, List, ListItem, Paragraph,
        Row, Table, TableState,
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
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let terminal = ratatui::init();

    let result = run_app(terminal, conn, config.tui.refresh_seconds);
    ratatui::restore();
    disable_raw_mode()?;
    execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
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
    table_scroll: usize,
    event_scroll: usize,
    search_mode: bool,
    search_input: String,
    chart_fullscreen: bool,
    event_fullscreen: bool,
    show_help: bool,
    refresh_every: Duration,
    last_refresh: Instant,
    status: String,
    last_click: Option<ClickInfo>,
}

#[derive(Debug, Clone)]
struct ClickInfo {
    focus: Focus,
    team_id: Option<String>,
    at: Instant,
}

#[derive(Debug, Clone, Copy)]
struct ViewLayout {
    table: Rect,
    events: Rect,
    chart: Rect,
    status: Rect,
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
            table_scroll: 0,
            event_scroll: 0,
            search_mode: false,
            search_input: String::new(),
            chart_fullscreen: false,
            event_fullscreen: false,
            show_help: false,
            refresh_every: Duration::from_secs(refresh_seconds.max(1)),
            last_refresh: Instant::now() - Duration::from_secs(refresh_seconds.max(1)),
            status: String::new(),
            last_click: None,
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
        self.refresh_events()?;
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
        let teams: Vec<String> = if self.compare_teams.is_empty() {
            self.selected_team
                .iter()
                .cloned()
                .collect()
        } else {
            self.compare_teams.iter().cloned().collect()
        };
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
        self.table_scroll = self.table_scroll.min(self.filtered_indices.len().saturating_sub(1));
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

    fn refresh_events(&mut self) -> Result<()> {
        let selected_team = self.selected_team.clone();
        self.events = recent_events(&self.conn, selected_team.as_deref(), RECENT_EVENTS_LIMIT)?;
        self.event_scroll = self.event_scroll.min(self.events.len().saturating_sub(1));
        Ok(())
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
        self.refresh_events()?;
        self.reload_chart_series()?;
        Ok(())
    }

    fn set_selection(&mut self, selected: usize) -> Result<()> {
        if self.filtered_indices.is_empty() {
            return Ok(());
        }
        let next = selected.min(self.filtered_indices.len().saturating_sub(1));
        self.table_state.select(Some(next));
        self.sync_selected_team();
        self.refresh_events()?;
        self.reload_chart_series()?;
        Ok(())
    }

    fn table_page_capacity(&self) -> usize {
        current_view_layout(self.chart_fullscreen, self.event_fullscreen)
            .map(|layout| table_visible_rows(layout.table))
            .unwrap_or(10)
            .max(1)
    }

    fn event_page_capacity(&self) -> usize {
        current_view_layout(self.chart_fullscreen, self.event_fullscreen)
            .map(|layout| events_visible_rows(layout.events))
            .unwrap_or(8)
            .max(1)
    }

    fn ensure_selection_visible(&mut self) {
        let Some(selected) = self.table_state.selected() else {
            return;
        };
        let page = self.table_page_capacity();
        if selected < self.table_scroll {
            self.table_scroll = selected;
        } else if selected >= self.table_scroll + page {
            self.table_scroll = selected.saturating_sub(page.saturating_sub(1));
        }
    }

    fn scroll_events(&mut self, delta: isize) {
        if self.events.is_empty() {
            self.event_scroll = 0;
            return;
        }
        let max_scroll = self
            .events
            .len()
            .saturating_sub(self.event_page_capacity());
        let next = (self.event_scroll as isize + delta).clamp(0, max_scroll as isize) as usize;
        self.event_scroll = next;
    }

    fn toggle_compare(&mut self) -> Result<()> {
        let Some(team_id) = self.selected_team.clone() else {
            return Ok(());
        };
        if !self.compare_teams.insert(team_id.clone()) {
            self.compare_teams.remove(&team_id);
        }
        self.reload_chart_series()?;
        if !self.filtered_indices.is_empty() {
            self.move_selection(1)?;
            self.ensure_selection_visible();
        }
        Ok(())
    }

    fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Table => Focus::Events,
            Focus::Events => Focus::Chart,
            Focus::Chart => Focus::Table,
        };
    }

    fn clear_search(&mut self) -> Result<()> {
        let preserve_team = self.selected_team.clone();
        self.search_mode = false;
        self.search_input.clear();
        self.apply_filter();
        if let Some(team_id) = preserve_team {
            self.select_team_by_id(&team_id)?;
        } else {
            self.refresh_events()?;
            self.reload_chart_series()?;
        }
        Ok(())
    }

    fn select_team_by_id(&mut self, team_id: &str) -> Result<()> {
        if let Some(selected) = self
            .filtered_indices
            .iter()
            .position(|idx| self.leaderboard[*idx].team_id == team_id)
        {
            self.set_selection(selected)?;
            self.ensure_selection_visible();
        }
        Ok(())
    }

    fn jump_to_top(&mut self) -> Result<()> {
        match self.focus {
            Focus::Table => {
                self.table_scroll = 0;
                self.set_selection(0)?;
            }
            Focus::Events => {
                self.event_scroll = 0;
            }
            Focus::Chart => {}
        }
        Ok(())
    }

    fn jump_to_bottom(&mut self) -> Result<()> {
        match self.focus {
            Focus::Table => {
                let last = self.filtered_indices.len().saturating_sub(1);
                self.set_selection(last)?;
                self.ensure_selection_visible();
            }
            Focus::Events => {
                self.event_scroll = self.events.len().saturating_sub(self.event_page_capacity());
            }
            Focus::Chart => {}
        }
        Ok(())
    }

    fn page_by(&mut self, delta: isize) -> Result<()> {
        match self.focus {
            Focus::Table => {
                let step = self.table_page_capacity() as isize;
                self.move_selection(delta * step)?;
                self.ensure_selection_visible();
            }
            Focus::Events => {
                let step = self.event_page_capacity() as isize;
                self.scroll_events(delta * step);
            }
            Focus::Chart => {}
        }
        Ok(())
    }

    fn toggle_chart_fullscreen(&mut self) {
        self.chart_fullscreen = !self.chart_fullscreen;
        if self.chart_fullscreen {
            self.event_fullscreen = false;
            self.focus = Focus::Chart;
        } else {
            self.focus = Focus::Table;
        }
    }

    fn toggle_event_fullscreen(&mut self) {
        self.event_fullscreen = !self.event_fullscreen;
        if self.event_fullscreen {
            self.chart_fullscreen = false;
            self.focus = Focus::Events;
        } else {
            self.focus = Focus::Table;
        }
    }

    fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        let layout = current_view_layout(self.chart_fullscreen, self.event_fullscreen)?;
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if contains(layout.table, mouse.column, mouse.row) {
                    self.focus = Focus::Table;
                    self.move_selection(-1)?;
                    self.ensure_selection_visible();
                } else if contains(layout.events, mouse.column, mouse.row) {
                    self.focus = Focus::Events;
                    self.scroll_events(-2);
                }
            }
            MouseEventKind::ScrollDown => {
                if contains(layout.table, mouse.column, mouse.row) {
                    self.focus = Focus::Table;
                    self.move_selection(1)?;
                    self.ensure_selection_visible();
                } else if contains(layout.events, mouse.column, mouse.row) {
                    self.focus = Focus::Events;
                    self.scroll_events(2);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if contains(layout.table, mouse.column, mouse.row) {
                    self.focus = Focus::Table;
                    if let Some(clicked) =
                        table_index_from_mouse(layout.table, mouse.row, self.table_scroll)
                        && clicked < self.filtered_indices.len()
                    {
                        self.set_selection(clicked)?;
                        self.ensure_selection_visible();
                        let team_id = self.selected_team.clone();
                        if self.is_double_click(Focus::Table, team_id.as_deref()) {
                            self.toggle_compare()?;
                            self.status = format!(
                                "toggled compare for {}",
                                team_id.unwrap_or_else(|| "-".to_string())
                            );
                        }
                        self.last_click = Some(ClickInfo {
                            focus: Focus::Table,
                            team_id: self.selected_team.clone(),
                            at: Instant::now(),
                        });
                    }
                } else if contains(layout.events, mouse.column, mouse.row) {
                    self.focus = Focus::Events;
                    self.last_click = Some(ClickInfo {
                        focus: Focus::Events,
                        team_id: self.selected_team.clone(),
                        at: Instant::now(),
                    });
                } else if contains(layout.chart, mouse.column, mouse.row) {
                    self.focus = Focus::Chart;
                    self.last_click = Some(ClickInfo {
                        focus: Focus::Chart,
                        team_id: None,
                        at: Instant::now(),
                    });
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn is_double_click(&self, focus: Focus, team_id: Option<&str>) -> bool {
        let Some(last_click) = &self.last_click else {
            return false;
        };
        last_click.focus == focus
            && last_click.team_id.as_deref() == team_id
            && last_click.at.elapsed() <= Duration::from_millis(450)
    }
}

fn run_app(mut terminal: DefaultTerminal, conn: rusqlite::Connection, refresh_seconds: u64) -> Result<()> {
    let mut app = App::new(conn, refresh_seconds)?;

    loop {
        terminal.draw(|frame| render(frame, &app))?;
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) => {
                    if app.show_help {
                        app.toggle_help();
                        continue;
                    }
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    if app.search_mode {
                        match key.code {
                            KeyCode::Esc => {
                                app.clear_search()?;
                            }
                            KeyCode::Enter => {
                                app.search_mode = false;
                                app.apply_filter();
                                app.refresh_events()?;
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
                            if app.chart_fullscreen {
                                app.chart_fullscreen = false;
                                app.focus = Focus::Table;
                            } else if app.event_fullscreen {
                                app.event_fullscreen = false;
                                app.focus = Focus::Table;
                            } else if !app.search_input.is_empty() {
                                app.clear_search()?;
                            }
                        }
                        KeyCode::Char('/') => {
                            app.search_mode = true;
                            app.status = "search mode".to_string();
                        }
                        KeyCode::Char('g') => app.jump_to_top()?,
                        KeyCode::Char('G') => app.jump_to_bottom()?,
                        KeyCode::Char('[') => app.page_by(-1)?,
                        KeyCode::Char(']') => app.page_by(1)?,
                        KeyCode::Char('o') => app.toggle_chart_fullscreen(),
                        KeyCode::Char('O') => app.toggle_event_fullscreen(),
                        KeyCode::Char('h') => app.toggle_help(),
                        KeyCode::Tab => app.cycle_focus(),
                        KeyCode::Char('J') => app.scroll_events(1),
                        KeyCode::Char('K') => app.scroll_events(-1),
                        KeyCode::Down | KeyCode::Char('j') => {
                            match app.focus {
                                Focus::Table => {
                                    app.move_selection(1)?;
                                    app.ensure_selection_visible();
                                }
                                Focus::Events => app.scroll_events(1),
                                Focus::Chart => {}
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            match app.focus {
                                Focus::Table => {
                                    app.move_selection(-1)?;
                                    app.ensure_selection_visible();
                                }
                                Focus::Events => app.scroll_events(-1),
                                Focus::Chart => {}
                            }
                        }
                        KeyCode::Enter => {
                            app.refresh_events()?;
                        }
                        KeyCode::Char(' ') => app.toggle_compare()?,
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => {
                    if app.show_help {
                        app.toggle_help();
                        continue;
                    }
                    app.handle_mouse(mouse)?;
                }
                _ => {
                    if app.show_help {
                        app.toggle_help();
                    }
                }
            }
        }

        if app.last_refresh.elapsed() >= app.refresh_every {
            app.reload(false)?;
        }
    }

    Ok(())
}

fn render(frame: &mut Frame<'_>, app: &App) {
    let layout = split_layout(frame.area(), app.chart_fullscreen, app.event_fullscreen);
    render_table(frame, layout.table, app);
    render_events(frame, layout.events, app);
    render_chart(frame, layout.chart, app);
    render_status(frame, layout.status, app);
    if app.show_help {
        render_help(frame, frame.area(), app.chart_fullscreen || app.event_fullscreen);
    }
}

fn render_table(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let start = app.table_scroll.min(app.filtered_indices.len().saturating_sub(1));
    let visible_rows = table_visible_rows(area);
    let end = min(start + visible_rows, app.filtered_indices.len());
    let rows: Vec<Row<'_>> = app
        .filtered_indices
        .iter()
        .skip(start)
        .take(end.saturating_sub(start))
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
    let mut state = app.table_state.clone();
    state.select(
        app.table_state
            .selected()
            .and_then(|selected| selected.checked_sub(start))
            .filter(|selected| *selected < end.saturating_sub(start)),
    );
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_events(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let title = match &app.selected_team {
        Some(team_id) => format!("Events {}", team_id),
        None => "Recent Events".to_string(),
    };
    let visible_rows = events_visible_rows(area);
    let items: Vec<ListItem<'_>> = app
        .events
        .iter()
        .skip(app.event_scroll)
        .take(visible_rows)
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
    if area.width == 0 || area.height == 0 {
        return;
    }
    let title = if app.compare_teams.is_empty() {
        "Score Chart (current team)"
    } else {
        "Score Chart"
    };
    let mut chart_data = Vec::new();
    let mut min_x = i64::MAX;
    let mut max_x = i64::MIN;
    let mut max_y = 0.0_f64;
    let mut min_y = f64::MAX;

    let chart_team_ids: Vec<String> = if app.compare_teams.is_empty() {
        app.selected_team.iter().cloned().collect()
    } else {
        app.compare_teams.iter().cloned().collect()
    };

    for team_id in &chart_team_ids {
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
        let placeholder = Paragraph::new("No team selected")
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
    let y_mid = y_start + (y_end.max(y_start + 1.0) - y_start) / 2.0;
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
                .bounds([y_start, y_end.max(y_start + 1.0)])
                .labels(
                    vec![
                        Line::from(format!("{y_start:.3}")),
                        Line::from(format!("{y_mid:.3}")),
                        Line::from(format!("{:.3}", y_end.max(y_start + 1.0))),
                    ],
                ),
        );
    frame.render_widget(chart, area);
}

fn render_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let selected = app
        .selected_team
        .as_deref()
        .unwrap_or("-");
    let hints = if app.search_mode {
        "/ search  Esc clear  Enter apply  q quit"
    } else if app.show_help {
        "h/Esc close help  q quit"
    } else {
        "q quit  / search  h help  o chart  O events  [ ] page  g/G jump"
    };
    let help = format!(
        "{} | selected={} focus={:?} | {}",
        hints, selected, app.focus, app.status
    );
    let paragraph = Paragraph::new(help).block(Block::default().borders(Borders::ALL).title("Status"));
    frame.render_widget(paragraph, area);
}

fn render_help(frame: &mut Frame<'_>, area: Rect, chart_fullscreen: bool) {
    let popup = centered_rect(
        if chart_fullscreen { 90 } else { 72 },
        if chart_fullscreen { 55 } else { 60 },
        area,
    );
    let text = vec![
        Line::from("Navigation"),
        Line::from("j/k or Up/Down: move in focused panel"),
        Line::from("J / K: scroll events window up / down"),
        Line::from("[ / ]: page up / page down in table or events"),
        Line::from("g / G: jump to top / bottom in table or events"),
        Line::from("Tab: cycle focus"),
        Line::from(""),
        Line::from("Views"),
        Line::from("Space: add/remove selected team from chart compare"),
        Line::from("o: toggle chart fullscreen"),
        Line::from("O: toggle events fullscreen"),
        Line::from("/: start search"),
        Line::from("Esc: clear search or exit fullscreen"),
        Line::from(""),
        Line::from("Mouse"),
        Line::from("Single click row: select team"),
        Line::from("Double click row: toggle compare team"),
        Line::from("Wheel: scroll table or events"),
        Line::from(""),
        Line::from("General"),
        Line::from("r: reload from sqlite"),
        Line::from("h or Esc: close help"),
        Line::from("q: quit"),
    ];
    frame.render_widget(Clear, popup);
    let paragraph = Paragraph::new(text)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .block(
            Block::default()
                .title("Key Help")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );
    frame.render_widget(paragraph, popup);
}

fn current_view_layout(chart_fullscreen: bool, event_fullscreen: bool) -> Result<ViewLayout> {
    let (width, height) = size()?;
    Ok(split_layout(
        Rect::new(0, 0, width, height),
        chart_fullscreen,
        event_fullscreen,
    ))
}

fn split_layout(area: Rect, chart_fullscreen: bool, event_fullscreen: bool) -> ViewLayout {
    if chart_fullscreen {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(3)])
            .split(area);
        return ViewLayout {
            table: Rect::new(0, 0, 0, 0),
            events: Rect::new(0, 0, 0, 0),
            chart: layout[0],
            status: layout[1],
        };
    }
    if event_fullscreen {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(3)])
            .split(area);
        return ViewLayout {
            table: Rect::new(0, 0, 0, 0),
            events: layout[0],
            chart: Rect::new(0, 0, 0, 0),
            status: layout[1],
        };
    }
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(37), Constraint::Length(3)])
        .split(area);
    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(layout[1]);
    ViewLayout {
        table: layout[0],
        events: bottom[0],
        chart: bottom[1],
        status: layout[2],
    }
}

fn contains(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn table_visible_rows(area: Rect) -> usize {
    area.height.saturating_sub(3) as usize
}

fn events_visible_rows(area: Rect) -> usize {
    area.height.saturating_sub(2) as usize
}

fn table_index_from_mouse(area: Rect, row: u16, scroll: usize) -> Option<usize> {
    let data_start = area.y.saturating_add(2);
    if row < data_start || row >= area.y.saturating_add(area.height.saturating_sub(1)) {
        return None;
    }
    Some(scroll + (row - data_start) as usize)
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
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
