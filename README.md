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
| `r`            | Refresh active tab (+ detail if open)        |
| `Esc`          | Close detail panel (or quit if already closed) |
| `q` / `Ctrl+C` | Quit                                         |

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

**Planned:**
- Status transition picker (`t` opens "move to → " menu)
- Search/filter overlay within current tab (`/`)
- Watcher / star toggle
- Per-tab column override

## License

MIT.
