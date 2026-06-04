# mnml-tickets-jira

Jira ticket viewer for [mnml](https://mnml.sh) — terminal TUI with
configurable tabs (JQL queries or auto-resolved release `fixVersion`s).
Runs standalone or, in a follow-up, as an mnml-hosted pane.

```
┌─ tickets ────────────────────────────────────────────────────────┐
│ ▸1.Testing (12)  2.Current (47)  3.Next (8)  4.Mobile (3)  5.Mine │
└──────────────────────────────────────────────────────────────────┘
┌─ Testing ────────────────────────────────────────────────────────┐
│ KEY      STATUS    ASSIGNEE        UPDATED     SUMMARY           │
│ TE-1234  Testing   chrismclennan   2026-06-02  Bufferline drops…│
│ TE-1235  Testing   andrew          2026-06-01  AI panel margin…  │
│ …                                                                │
└──────────────────────────────────────────────────────────────────┘
  refreshing Testing…   1-9 tab · ↑↓/jk move · Enter/o open · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-tickets-jira mnml-tickets-jira
```

(Homebrew tap + binary releases will follow once the binary stabilises.)

## Setup

1. **Get a Jira API token**:
   <https://id.atlassian.com/manage-profile/security/api-tokens>

2. **Save the token** to `~/.config/mnml-tickets-jira/token`
   (`chmod 600`).

3. **Run once** to scaffold the config template:
   ```sh
   mnml-tickets-jira
   ```
   This writes `~/.config/mnml-tickets-jira.toml` and exits with
   instructions. Edit `jira_url`, `email`, and the `[[tabs]]` list.

4. **Re-run** — the TUI launches with your configured tabs.

5. **Verify** the resolved config + auth state:
   ```sh
   mnml-tickets-jira --check
   ```

## Tabs

Each `[[tabs]]` entry is one tab. Either:

```toml
# Literal JQL — full control, you maintain version strings.
[[tabs]]
name = "Mine"
jql  = "reporter = currentUser() ORDER BY updated DESC"
```

…or auto-resolved against the project's release list:

```toml
# Auto-resolve: first unreleased fixVersion of project TE.
[[tabs]]
name    = "Current"
mode    = "current_release"
project = "TE"

# Auto-resolve: second unreleased fixVersion of TE, filtered to the
# Mobile component.
[[tabs]]
name      = "Mobile"
mode      = "next_release"
project   = "TE"
component = "Mobile"
```

Modes:

| `mode`              | Resolves to                                   |
|---------------------|-----------------------------------------------|
| `current_release`   | Earliest unreleased fixVersion of `project`  |
| `next_release`      | Second-earliest unreleased fixVersion (falls back to current if only one exists) |

## Keys

| Chord          | Action                                       |
|----------------|----------------------------------------------|
| `1`-`9`        | Switch to that tab                           |
| `Tab` / `BackTab` | Cycle tabs forward / back                  |
| `↑` / `k`, `↓` / `j` | Move selection                          |
| `PgUp` / `PgDn` | Jump 10 rows                                |
| `g` / `G`      | Top / bottom                                 |
| `Enter` / `o`  | Open focused ticket in browser               |
| `d`            | Toggle right-half ticket detail panel        |
| `Ctrl+u` / `Ctrl+d` | Scroll detail panel up / down (when open) |
| `/`            | Open filter editor (substring match)         |
| `t`            | Open status transition picker for focused ticket |
| `w`            | Toggle watch on focused ticket               |
| `r`            | Refresh active tab (+ detail if open)        |
| `Esc`          | Clear filter → close detail → quit (cascade) |
| `q` / `Ctrl+C` | Quit                                         |

### Watcher toggle

`w` toggles your watch on the focused ticket. Direction is derived
from the cached ticket detail (it includes `isWatching` alongside
the watcher count), so the first press fetches the detail if the
right-half panel hasn't already loaded it.

The detail panel header shows `★ watching (4 total)` or
`☆ 2 watcher(s)` — a glance at how many people are paying attention,
and whether you're one of them.

Unwatch requires Jira's `accountId`, which is fetched once per
session via `/rest/api/3/myself` and cached. If that call fails
(revoked token / scope), `w` toasts the error and falls back to
no-op rather than retrying every keypress.

### Per-tab column override

Each `[[tabs]]` entry can override the default column set via
`columns = [...]`. Default (when unset) is
`["key", "status", "assignee", "updated", "summary"]`. Valid values:

| value          | column            |
|----------------|-------------------|
| `key`          | TE-1234           |
| `status`       | "In Review" etc.  |
| `assignee`     | display name      |
| `reporter`     | display name      |
| `priority`     | "Highest" etc.    |
| `type`         | "Bug", "Task", …  |
| `updated`      | YYYY-MM-DD        |
| `fix_version`  | comma-joined list |
| `summary`      | ticket title      |

`summary` is the only column that fills remaining width — put it
last. Example, replacing assignee with priority on the "Mine" tab:

```toml
[[tabs]]
name = "Mine"
jql  = "reporter = currentUser() ORDER BY updated DESC"
columns = ["key", "priority", "status", "updated", "summary"]
```

### Status transition picker

`t` opens a centered modal listing the workflow transitions available
for the focused ticket — exactly what Jira's web UI shows in the
"Status" dropdown, just keyboard-driven. Numbered `1`-`9` for direct
selection, `↑↓` / `jk` to move, Enter to commit, Esc to cancel.

The list is per-issue: Jira's workflow engine is graph-based, so the
options depend on the current status (what edges leave this node) and
your project permissions. A terminal state with no outgoing edges
renders as `(no transitions available)` — that's normal, not a bug.

On success the modal closes, the active tab refreshes (so the new
status chip shows up in the table), and any cached ticket detail
gets dropped so a re-fetch picks up the moved-to state. On failure
(permission denied, validation required) the error surfaces inside
the modal and the picker stays open.

### Filter

`/` opens a 1-row filter editor above the table. Substring match —
case-insensitive — against both `KEY` and `SUMMARY`. The visible row
count updates live as you type; `Enter` commits the filter (it stays
applied while you scroll, switch tabs, etc.); `Esc` cancels and drops
the filter.

Selection stays consistent across filter changes: arrow-keys step
through the *visible* rows only, and the cursor never lands on a
filtered-out row. Tab title shows `(<visible>/<total>)` while a
filter is active.

`Esc` cascades through state when there's more than one thing to
unwind:

1. If a filter is committed, Esc clears it.
2. Otherwise, if the detail panel is open, Esc closes it.
3. Otherwise, Esc quits the app.

So Esc-Esc-Esc safely leaves a "filter + detail + done" state.

### Detail panel

`d` opens a right-half panel for the focused ticket: type / status /
priority / assignee / reporter / fixVersion header, then description,
then up to the last 10 comments (most-recent first). The narrative
content (description + comments) is lazy-loaded on first focus and
cached per-issue key — arrow-keying through a long list only fetches
once per ticket.

`r` while the detail panel is open invalidates the cached detail for
the focused ticket and re-fetches both the list and the narrative —
useful after the ticket got a transition or a new comment server-side.

Atlassian Document Format (Jira's rich-text JSON) is rendered as plain
text in v1; inline marks (bold, italic, links) are stripped and block
structure (paragraphs, bullet lists, code blocks) is preserved by
newlines.

## Status & roadmap

**v0.1 (this release):**
- Standalone TUI mode
- Configurable JQL or auto-resolved release tabs
- 1-9 tab switching · ↑↓ navigation · open-in-browser · refresh

**v0.2 (current main):**
- Blit mode (`--blit <socket>`) — mnml / tmnl can host the binary as a pane
- Right-half ticket detail panel (`d`) — type / status / priority /
  assignee / reporter / fixVersion + description + last 10 comments,
  lazy-loaded per issue key
- Atlassian Document Format → plain text for description + comments
- Filter editor (`/`) — substring match on key + summary,
  case-insensitive, applies live; Esc cascade for cleanup
- Status transition picker (`t`) — modal listing the focused
  ticket's available workflow transitions; POSTs the chosen
  one + refreshes the list
- Watcher toggle (`w`) — POST/DELETE `/issue/{key}/watchers`,
  with watcher chip on the detail panel header
- Per-tab column override — `[[tabs]] columns = [...]` from a
  fixed set (key, status, assignee, reporter, priority, type,
  updated, fix_version, summary)

**v0.2 is feature-complete.** Future tracks if useful:
- bulk-transition / bulk-assign across selected rows
- comment posting from the detail panel
- inline-edit assignee / fixVersion

## License

MIT.
