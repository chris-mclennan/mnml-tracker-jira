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
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs},
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
    // Modal overlays last so they sit on top of everything else.
    if app.transition_picker.is_some() {
        draw_transition_picker(f, size, app);
    }
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

    // Split off a 1-row filter strip above the table when there is
    // any filter at all (open or committed). Otherwise the table
    // gets the full body region.
    let (filter_area, table_area) = if app.filter.is_some() {
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(area);
        (Some(parts[0]), parts[1])
    } else {
        (None, area)
    };
    if let Some(a) = filter_area {
        draw_filter_strip(f, a, app);
    }

    // Per-tab column override — falls back to the family default
    // (key, status, assignee, updated, summary). Resolved on every
    // draw so config reloads (future) would pick up changes.
    let columns: Vec<crate::config::Column> = app
        .cfg
        .tabs
        .get(app.active_tab)
        .and_then(|t| t.columns.clone())
        .unwrap_or_else(crate::config::Column::default_set);

    let header = Row::new(
        columns
            .iter()
            .map(|c| Cell::from(c.header()))
            .collect::<Vec<_>>(),
    )
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let visible = app.visible_indices();
    let total = tab.issues.len();
    let rows: Vec<Row> = visible
        .iter()
        .map(|&idx| &tab.issues[idx])
        .map(|i| {
            let cells: Vec<Cell> = columns.iter().map(|c| cell_for_column(i, *c)).collect();
            let mut row = Row::new(cells);
            // Highlight rows whose key is in the bulk-selection set —
            // a magenta tint distinguishes "this is in the operation
            // basket" from the regular cursor's blue/cyan highlight.
            if app.selection.contains(&i.key) {
                row = row.style(
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                );
            }
            row
        })
        .collect();

    let widths: Vec<Constraint> = columns
        .iter()
        .map(|c| match c.width() {
            Some(w) => Constraint::Length(w),
            None => Constraint::Min(20),
        })
        .collect();

    let title = if app.filter.is_some() && visible.len() != total {
        format!(" {} ({}/{}) ", tab.name, visible.len(), total)
    } else {
        format!(" {} ", tab.name)
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    // Translate the raw `issues[]` index in `selected` into the
    // visible-rows index — TableState selects by row position.
    let visible_pos = visible.iter().position(|&i| i == tab.selected);
    let mut state = TableState::default();
    state.select(visible_pos);
    f.render_stateful_widget(table, table_area, &mut state);
}

/// One-row filter strip above the table. Three visual states:
///   editing       → `/<buffer>│`  (cursor block, cyan)
///   committed     → `filter: <buffer>   Esc clears`  (dimmed)
///   no filter     → not drawn (the caller skips when filter is None)
fn draw_filter_strip(f: &mut Frame, area: Rect, app: &App) {
    let Some(filter) = app.filter.as_ref() else {
        return;
    };
    let line = if filter.editing {
        // Render `/buffer│` — the `│` is the cursor block. Truncate
        // the buffer so the cursor stays on-screen on a narrow strip.
        let avail = area.width.saturating_sub(2) as usize;
        let chars: Vec<char> = filter.buffer.chars().collect();
        let cursor = filter.cursor.min(chars.len());
        let start = if cursor >= avail {
            cursor - avail + 1
        } else {
            0
        };
        let end = (start + avail).min(chars.len());
        let head: String = chars[start..cursor].iter().collect();
        let tail: String = chars[cursor..end].iter().collect();
        Line::from(vec![
            Span::styled("/", Style::default().fg(Color::Cyan)),
            Span::styled(head, Style::default().fg(Color::White)),
            Span::styled("│", Style::default().fg(Color::Cyan)),
            Span::styled(
                tail,
                Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                "filter: ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(filter.buffer.clone(), Style::default().fg(Color::Cyan)),
            Span::styled(
                "   Esc to clear · / to refine",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
        ])
    };
    f.render_widget(Paragraph::new(line), area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = if app.comment_editor.is_some() {
        " typing comment · Ctrl+S send · Esc cancel · Enter newline "
    } else if app.transition_picker.is_some() {
        " 1-9 jump · ↑↓/jk move · Enter commit · Esc cancel "
    } else if app.filter.as_ref().map(|f| f.editing).unwrap_or(false) {
        " type to filter · Enter commit · Esc cancel "
    } else if app.details_visible {
        " ↑↓ · Ctrl+u/d scroll · d close · c comment · Space pick · / filter · t move · w watch · r · q "
    } else if !app.selection.is_empty() {
        " ↑↓ · Space pick · t move all · Esc clear · / filter · d details · r refresh · q quit "
    } else {
        " 1-9 tab · ↑↓ · / filter · Space pick · t move · w watch · d details · Enter/o open · r · q "
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
    // Reserve a bottom strip for the comment editor when open.
    let (detail_area, editor_area) = if app.comment_editor.is_some() {
        let editor_h = 8u16.min(area.height.saturating_sub(4));
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(editor_h)])
            .split(area);
        (parts[0], Some(parts[1]))
    } else {
        (area, None)
    };

    let tab = app.active();
    let Some(issue) = tab.issues.get(tab.selected) else {
        let p = Paragraph::new("(no ticket focused)")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(" detail "));
        f.render_widget(p, detail_area);
        if let Some(ea) = editor_area {
            draw_comment_editor(f, ea, app);
        }
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

    // Watcher chip — surfaces alongside the meta lines once the
    // detail is loaded. `★` = watching, `☆` = not.
    if let Some(detail) = app.focused_detail() {
        let glyph = if detail.watching { "★" } else { "☆" };
        let label = if detail.watching {
            format!("watching ({} total)", detail.watch_count)
        } else if detail.watch_count == 0 {
            "no watchers".to_string()
        } else {
            format!("{} watcher(s)", detail.watch_count)
        };
        lines.push(Line::from(vec![
            Span::styled(
                "  watcher: ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
            Span::styled(
                format!("{glyph} "),
                if detail.watching {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            Span::raw(label),
        ]));
        lines.push(Line::from(""));
    }

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
    f.render_widget(p, detail_area);
    if let Some(ea) = editor_area {
        draw_comment_editor(f, ea, app);
    }
}

/// Inline comment editor docked at the bottom of the detail panel.
/// Shows the buffer text, a cursor block, an error line if posting
/// failed, and a hint row.
fn draw_comment_editor(f: &mut Frame, area: Rect, app: &App) {
    let Some(editor) = app.comment_editor.as_ref() else {
        return;
    };
    let mut lines: Vec<Line> = Vec::new();
    // Render the buffer line by line. The cursor block sits at the
    // (row, col) corresponding to `editor.cursor` chars into the buffer.
    let chars: Vec<char> = editor.buffer.chars().collect();
    let cursor = editor.cursor.min(chars.len());
    let mut row = 0usize;
    let mut col = 0usize;
    let mut row_buf = String::new();
    let mut cursor_row = 0usize;
    let mut cursor_col = 0usize;
    for (i, &c) in chars.iter().enumerate() {
        if i == cursor {
            cursor_row = row;
            cursor_col = col;
        }
        if c == '\n' {
            lines.push(Line::from(row_buf.clone()));
            row_buf.clear();
            row += 1;
            col = 0;
        } else {
            row_buf.push(c);
            col += 1;
        }
    }
    if cursor == chars.len() {
        cursor_row = row;
        cursor_col = col;
    }
    // Add the trailing line (with the cursor block appended if it's
    // at end-of-row).
    if cursor_row == row {
        row_buf.insert(cursor_col, '│');
        lines.push(Line::from(row_buf));
    } else {
        lines.push(Line::from(row_buf));
    }
    // If cursor is on an earlier row, re-render that row with the
    // cursor block injected. (We've already pushed it; replace.)
    if cursor_row < row && cursor_row < lines.len() {
        let mut s: String = chars
            .iter()
            .take(cursor)
            .rev()
            .take_while(|&&c| c != '\n')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let tail: String = chars
            .iter()
            .skip(cursor)
            .take_while(|&&c| c != '\n')
            .collect();
        s.push('│');
        s.push_str(&tail);
        lines[cursor_row] = Line::from(s);
    }
    if let Some(err) = editor.error.as_ref() {
        lines.push(Line::from(Span::styled(
            format!("error: {err}"),
            Style::default().fg(Color::Red),
        )));
    }
    let hint = if editor.posting {
        "posting…"
    } else if editor.buffer.trim().is_empty() {
        "type a comment · Esc cancel"
    } else {
        "Ctrl+S send · Esc cancel · Enter newline"
    };
    let title = format!(" comment on {} ", editor.key);
    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_bottom(Line::from(Span::styled(
                hint,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ))),
    );
    f.render_widget(p, area);
}

/// Modal overlay listing the focused ticket's available workflow
/// transitions. Centered ~50% × ~50% in the screen, opaque (Clear
/// widget below) so the table underneath doesn't bleed through.
fn draw_transition_picker(f: &mut Frame, screen: Rect, app: &App) {
    let Some(picker) = app.transition_picker.as_ref() else {
        return;
    };
    // Center a 60-cell × 14-row box (clamped to screen) — wide enough
    // for "Start review → In Review", short enough to feel modal.
    let w = 60.min(screen.width.saturating_sub(4));
    let h = 14.min(screen.height.saturating_sub(4));
    let x = (screen.width.saturating_sub(w)) / 2;
    let y = (screen.height.saturating_sub(h)) / 2;
    let area = Rect::new(x, y, w, h);

    let title = if app.selection.is_empty() {
        format!(" transition {} ", picker.key)
    } else {
        format!(" transition × {} ticket(s) ", app.selection.len())
    };
    let body: Vec<Line> = match picker.transitions.as_ref() {
        None => vec![Line::from(Span::styled(
            "  loading…",
            Style::default().fg(Color::DarkGray),
        ))],
        Some(list) if list.is_empty() => {
            let msg = if let Some(err) = picker.error.as_ref() {
                format!("  error: {err}")
            } else {
                "  (no transitions available — terminal state or no permission)".to_string()
            };
            vec![
                Line::from(Span::styled(msg, Style::default().fg(Color::Red))),
                Line::from(""),
                Line::from(Span::styled(
                    "  Esc to close",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                )),
            ]
        }
        Some(list) => {
            let mut lines: Vec<Line> = list
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let arrow = match t.to_name.as_deref() {
                        Some(dest) => format!("  {arrow} {dest}", arrow = "→"),
                        None => String::new(),
                    };
                    let prefix = if i == picker.selected { "▸ " } else { "  " };
                    // Number-key hint for the first 9 (1-9 jumps).
                    let num = if i < 9 {
                        format!("{}. ", i + 1)
                    } else {
                        "   ".to_string()
                    };
                    let style = if i == picker.selected {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    Line::from(vec![Span::styled(
                        format!("{prefix}{num}{name}{arrow}", name = t.name),
                        style,
                    )])
                })
                .collect();
            if let Some(err) = picker.error.as_ref() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  error: {err}"),
                    Style::default().fg(Color::Red),
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  1-9 jump · ↑↓/jk move · Enter commit · Esc cancel",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
            lines
        }
    };

    // Clear the cells underneath so the table doesn't bleed through.
    f.render_widget(Clear, area);
    let p = Paragraph::new(body).block(
        Block::default()
            .borders(Borders::ALL)
            .style(Style::default().bg(Color::Black))
            .title(title),
    );
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

/// Build a single styled table cell for `issue` × `column`. Handles
/// the per-column missing-data fallback (`—`) and the per-column
/// color theme (yellow KEY, status-themed STATUS, plain for others).
fn cell_for_column(issue: &crate::jira::Issue, column: crate::config::Column) -> Cell<'static> {
    use crate::config::Column;
    let f = &issue.fields;
    match column {
        Column::Key => Cell::from(issue.key.clone()).style(Style::default().fg(Color::Yellow)),
        Column::Status => {
            let s = f.status.as_ref().map(|x| x.name.as_str()).unwrap_or("?");
            Cell::from(s.to_string()).style(status_color(s))
        }
        Column::Assignee => {
            let s = f
                .assignee
                .as_ref()
                .map(|x| x.display_name.as_str())
                .unwrap_or("—");
            Cell::from(s.to_string())
        }
        Column::Reporter => {
            let s = f
                .reporter
                .as_ref()
                .map(|x| x.display_name.as_str())
                .unwrap_or("—");
            Cell::from(s.to_string())
        }
        Column::Priority => {
            let s = f.priority.as_ref().map(|x| x.name.as_str()).unwrap_or("—");
            Cell::from(s.to_string())
        }
        Column::Type => {
            let s = f.issuetype.as_ref().map(|x| x.name.as_str()).unwrap_or("—");
            Cell::from(s.to_string())
        }
        Column::Updated => {
            let s = f
                .updated
                .as_deref()
                .map(format_updated)
                .unwrap_or_else(|| "—".to_string());
            Cell::from(s)
        }
        Column::FixVersion => {
            let s = if f.fix_versions.is_empty() {
                "—".to_string()
            } else {
                f.fix_versions
                    .iter()
                    .map(|v| v.name.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            };
            Cell::from(s)
        }
        Column::Summary => Cell::from(f.summary.clone()),
    }
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
