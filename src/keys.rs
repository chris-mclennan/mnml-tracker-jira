//! Keyboard chord → action mapping. Kept small and centralized.

use crate::app::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub enum Action {
    Quit,
    Refresh,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    OpenInBrowser,
    SwitchTab(usize),
    NextTab,
    PrevTab,
    ToggleDetails,
    DetailScrollUp,
    DetailScrollDown,
}

pub fn handle(key: KeyEvent, app: &App) -> Option<Action> {
    let m = key.modifiers;
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        // Esc closes the detail pane when it's open, otherwise quits.
        // Mirrors VS Code's "Esc closes the side panel" muscle memory.
        KeyCode::Esc => {
            if app.details_visible {
                Some(Action::ToggleDetails)
            } else {
                Some(Action::Quit)
            }
        }
        KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
        KeyCode::Char('r') => Some(Action::Refresh),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        // Ctrl+u / Ctrl+d scroll the detail pane while it's open
        // (only meaningful when there is one — otherwise no-op).
        KeyCode::Char('u') if m.contains(KeyModifiers::CONTROL) => Some(Action::DetailScrollUp),
        KeyCode::Char('d') if m.contains(KeyModifiers::CONTROL) => Some(Action::DetailScrollDown),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        KeyCode::Enter | KeyCode::Char('o') => Some(Action::OpenInBrowser),
        KeyCode::Tab => Some(Action::NextTab),
        KeyCode::BackTab => Some(Action::PrevTab),
        // `d` (lowercase, no modifiers) toggles the detail pane.
        // Ctrl+d above takes precedence for scroll-down.
        KeyCode::Char('d') => Some(Action::ToggleDetails),
        KeyCode::Char(c @ '1'..='9') => Some(Action::SwitchTab((c as u8 - b'1') as usize)),
        _ => None,
    }
}

pub async fn apply(action: Action, app: &mut App) -> bool {
    // Track selection movement so we can lazy-fetch a new detail
    // when the user arrow-keys to a different row with the panel open.
    let pre_key = app.focused_key();
    match action {
        Action::Quit => return true,
        Action::Refresh => {
            // `r` while the detail pane is visible re-fetches both the
            // list AND the focused ticket's narrative, so a status
            // transition / new comment shows up.
            if app.details_visible {
                app.invalidate_focused_detail();
            }
            app.refresh_active().await;
            if app.details_visible {
                app.ensure_focused_detail().await;
            }
        }
        Action::Up => app.move_selection(-1),
        Action::Down => app.move_selection(1),
        Action::PageUp => app.move_selection(-10),
        Action::PageDown => app.move_selection(10),
        Action::Home => app.move_selection(-(i32::MAX as isize)),
        Action::End => app.move_selection(i32::MAX as isize),
        Action::OpenInBrowser => app.open_focused(),
        Action::NextTab => {
            let next = (app.active_tab + 1) % app.tabs.len();
            app.switch_tab(next);
            if app.tabs[app.active_tab].last_fetched.is_none() {
                app.refresh_active().await;
            }
        }
        Action::PrevTab => {
            let prev = if app.active_tab == 0 {
                app.tabs.len() - 1
            } else {
                app.active_tab - 1
            };
            app.switch_tab(prev);
            if app.tabs[app.active_tab].last_fetched.is_none() {
                app.refresh_active().await;
            }
        }
        Action::SwitchTab(i) => {
            app.switch_tab(i);
            if app.tabs[app.active_tab].last_fetched.is_none() {
                app.refresh_active().await;
            }
        }
        Action::ToggleDetails => app.toggle_details().await,
        Action::DetailScrollUp => {
            if app.details_visible {
                app.details_scroll = app.details_scroll.saturating_sub(4);
            }
        }
        Action::DetailScrollDown => {
            if app.details_visible {
                app.details_scroll = app.details_scroll.saturating_add(4);
            }
        }
    }
    // After a navigation action, if the focused key changed and the
    // detail pane is open, fetch the new ticket's detail. Reset the
    // pane scroll so a new ticket starts at the top.
    if app.details_visible
        && let post_key = app.focused_key()
        && post_key != pre_key
    {
        app.details_scroll = 0;
        app.ensure_focused_detail().await;
    }
    false
}
