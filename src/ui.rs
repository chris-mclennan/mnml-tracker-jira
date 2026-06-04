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
            Constraint::Min(1),    // body (table + optional details)
            Constraint::Length(1), // status line
        ])
        .split(size);

    draw_tabs(f, chunks[0], app);
    if app.details_visible {
        // Horizontal split: 60% list, 40% detail.
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(chunks[1]);
        draw_table(f, body[0], app);
        draw_details(f, body[1], app);
    } else {
        draw_table(f, chunks[1], app);
    }
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
    let hint = if app.details_visible {
        " ↑↓/jk move · Ctrl+u/d scroll detail · d/Esc close · r refresh · o open · q quit "
    } else {
        " 1-9 tab · ↑↓/jk move · d details · Enter/o open · r refresh · q quit "
    };
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

/// Right-half pane: the focused ticket's summary + status / assignee /
/// fixVersion header, then description, then the last N comments.
/// Content is plain text — ADF formatting is stripped to a
/// single-style paragraph (see `jira::adf_to_text`).
fn draw_details(f: &mut Frame, area: Rect, app: &App) {
    let tab = app.active();
    let Some(issue) = tab.issues.get(tab.selected) else {
        let p = Paragraph::new("(no ticket focused)")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(" detail "));
        f.render_widget(p, area);
        return;
    };
    let key = &issue.key;
    let summary = &issue.fields.summary;
    let status = issue
        .fields
        .status
        .as_ref()
        .map(|s| s.name.clone())
        .unwrap_or_else(|| "?".to_string());
    let assignee = issue
        .fields
        .assignee
        .as_ref()
        .map(|a| a.display_name.clone())
        .unwrap_or_else(|| "—".to_string());
    let reporter = issue
        .fields
        .reporter
        .as_ref()
        .map(|a| a.display_name.clone())
        .unwrap_or_else(|| "—".to_string());
    let priority = issue
        .fields
        .priority
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "—".to_string());
    let issuetype = issue
        .fields
        .issuetype
        .as_ref()
        .map(|t| t.name.clone())
        .unwrap_or_else(|| "—".to_string());
    let fix = if issue.fields.fix_versions.is_empty() {
        "—".to_string()
    } else {
        issue
            .fields
            .fix_versions
            .iter()
            .map(|v| v.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };

    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(
                key.clone(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(summary.clone(), Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        meta_line("type", &issuetype),
        meta_line("status", &status),
        meta_line("priority", &priority),
        meta_line("assignee", &assignee),
        meta_line("reporter", &reporter),
        meta_line("fixVersion", &fix),
        Line::from(""),
    ];

    // Body — description + comments, lazy-loaded.
    if let Some(detail) = app.focused_detail() {
        lines.push(section_header("description"));
        match detail.description.as_deref() {
            Some(d) if !d.trim().is_empty() => {
                for raw in d.lines() {
                    lines.push(Line::from(raw.to_string()));
                }
            }
            _ => lines.push(Line::from(Span::styled(
                "(no description)",
                Style::default().fg(Color::DarkGray),
            ))),
        }
        lines.push(Line::from(""));
        lines.push(section_header(&format!(
            "comments ({})",
            detail.comments.len()
        )));
        if detail.comments.is_empty() {
            lines.push(Line::from(Span::styled(
                "(no comments)",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            // Show the most-recent N — Jira returns comments
            // chronologically (oldest first), so reverse + take.
            let take = 10.min(detail.comments.len());
            for c in detail.comments.iter().rev().take(take) {
                let author = c.author.as_deref().unwrap_or("?");
                let when = c
                    .created
                    .as_deref()
                    .and_then(|s| s.split('T').next())
                    .unwrap_or("");
                lines.push(Line::from(vec![
                    Span::styled(author.to_string(), Style::default().fg(Color::Cyan)),
                    Span::styled(
                        format!("  {when}"),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::DIM),
                    ),
                ]));
                for raw in c.body.lines() {
                    lines.push(Line::from(format!("  {raw}")));
                }
                lines.push(Line::from(""));
            }
        }
    } else {
        lines.push(Line::from(Span::styled(
            "loading detail…",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let scroll = app.details_scroll;
    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" detail "))
        .scroll((scroll, 0));
    f.render_widget(p, area);
}

fn meta_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:>10}: "),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
        Span::raw(value.to_string()),
    ])
}

fn section_header(label: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("── {label} "),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    ))
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
