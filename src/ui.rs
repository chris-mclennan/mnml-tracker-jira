//! ratatui rendering + the main event loop. Run from `main.rs` with
//! a fully-initialized `App`.

use crate::app::App;
use crate::keys;
use anyhow::Result;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Tabs},
};
use std::io::Stdout;
use std::time::{Duration, Instant};

pub async fn run(app: &mut App) -> Result<()> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = event_loop(&mut terminal, app).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<()> {
    let mut last_refresh = Instant::now();
    loop {
        terminal.draw(|f| draw(f, app))?;

        // Auto-refresh on interval.
        if app.cfg.refresh_interval_secs > 0
            && last_refresh.elapsed().as_secs() >= app.cfg.refresh_interval_secs
        {
            app.refresh_active().await;
            last_refresh = Instant::now();
        }

        // Poll for keys with a small timeout so the auto-refresh
        // can fire even when the user isn't typing.
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                    if let Some(action) = keys::handle(key, app) {
                        let quit = keys::apply(action, app).await;
                        if quit {
                            break;
                        }
                        last_refresh = Instant::now();
                    }
                }
                Event::Resize(_, _) => { /* terminal handles re-draw */ }
                _ => {}
            }
        }
    }
    Ok(())
}

pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // tab strip
            Constraint::Min(1),    // table
            Constraint::Length(1), // status line
        ])
        .split(size);

    draw_tabs(f, chunks[0], app);
    draw_table(f, chunks[1], app);
    draw_status(f, chunks[2], app);
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let labels: Vec<Line> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let n = t.issues.len();
            let label = if t.last_fetched.is_some() {
                format!("{}.{} ({n})", i + 1, t.name)
            } else {
                format!("{}.{}", i + 1, t.name)
            };
            Line::from(label)
        })
        .collect();
    let tabs = Tabs::new(labels)
        .block(Block::default().borders(Borders::ALL).title(" tickets "))
        .select(app.active_tab)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn draw_table(f: &mut Frame, area: Rect, app: &App) {
    let tab = app.active();
    if let Some(err) = &tab.last_error {
        let p = Paragraph::new(format!("error: {err}\n\nPress `r` to retry."))
            .style(Style::default().fg(Color::Red));
        f.render_widget(p, area);
        return;
    }
    if tab.issues.is_empty() && tab.last_fetched.is_some() {
        let p = Paragraph::new("(no issues match this query)")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }
    if tab.issues.is_empty() {
        let p = Paragraph::new("loading…").style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }

    let header = Row::new(vec![
        Cell::from("KEY"),
        Cell::from("STATUS"),
        Cell::from("ASSIGNEE"),
        Cell::from("UPDATED"),
        Cell::from("SUMMARY"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = tab
        .issues
        .iter()
        .map(|i| {
            let status = i
                .fields
                .status
                .as_ref()
                .map(|s| s.name.as_str())
                .unwrap_or("?");
            let assignee = i
                .fields
                .assignee
                .as_ref()
                .map(|a| a.display_name.as_str())
                .unwrap_or("—");
            let updated = i
                .fields
                .updated
                .as_deref()
                .map(format_updated)
                .unwrap_or_else(|| "—".to_string());
            let summary = i.fields.summary.as_str();
            Row::new(vec![
                Cell::from(i.key.clone()).style(Style::default().fg(Color::Yellow)),
                Cell::from(status.to_string()).style(status_color(status)),
                Cell::from(assignee.to_string()),
                Cell::from(updated),
                Cell::from(summary.to_string()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(10),
        Constraint::Length(14),
        Constraint::Length(20),
        Constraint::Length(12),
        Constraint::Min(20),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", tab.name)),
        )
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    let mut state = TableState::default();
    state.select(Some(tab.selected));
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk move · Enter/o open · r refresh · q quit ";
    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", app.status),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            hint,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// `2026-01-15T12:34:56.789+0000` → `2026-01-15`.
fn format_updated(s: &str) -> String {
    s.split('T').next().unwrap_or(s).to_string()
}

fn status_color(name: &str) -> Style {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "done" | "closed" | "resolved" => Style::default().fg(Color::Green),
        "in progress" | "in review" | "in development" => Style::default().fg(Color::Cyan),
        "testing" | "qa" => Style::default().fg(Color::Magenta),
        "to do" | "open" | "backlog" => Style::default().fg(Color::White),
        "blocked" | "blocker" => Style::default().fg(Color::Red),
        _ => Style::default().fg(Color::Gray),
    }
}
