# quim

A TUI (in Rust) for running SQL against MSSQL, PostgreSQL and SQLite,
built for vim fingers.

## Install

**Linux / macOS:**

```sh
curl -fsSL https://github.com/HectorBjernersjo/quim/releases/latest/download/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://github.com/HectorBjernersjo/quim/releases/latest/download/install.ps1 | iex
```

**From source** (requires a Rust toolchain):

```sh
cargo install --git https://github.com/HectorBjernersjo/quim
```

## Run

```bash
cargo build --release
./target/release/quim          # or put target/release/quim in PATH
quim --check                   # test config + connection + schema headlessly
```

## Keys

| | |
| --- | --- |
| `Ctrl+H/J/K/L` | switch pane (also `Ctrl+arrows` and `Alt+HJKL`) |
| `Ctrl+R` / `F5` / `Ctrl+Enter`¹ | run the query |
| `Ctrl+W`, then `h/j/k/l` | resize the focused pane (`h/l` width, `j/k` height); saved to config |
| `j/k`, `gg`/`G`, `Ctrl+D/U` | navigate lists and results |
| `/` | filter databases/tables (`Esc` clears) |
| `a` / `e` / `d` | add / edit / delete a source (in the database list) |
| `Enter` | select database · preview table (`SELECT TOP 100 *` or `LIMIT 100`) · open cell |
| `h/l`, `w/b`, `0`/`$` | move between result columns |
| `V` | select result rows; `y` copies them as JSON, `Y` as a markdown table |
| `y` / `Y` | copy cell / row (TSV) to the clipboard |
| `r` | run the last query again |
| `q` | close the detail pane / quit |
| `?` / `F1` | help |

In the editor:

| | |
| --- | --- |
| vim mode (default) | `i/a/o` insert · `v/V` visual · `d/c/y` + motion · `dd/cc/yy` · text objects `iw/aw`, `i"/i'`, `ib/i(/i[/iB` · `x`, `r`, `J` · `p/P` paste · `u`/`U` undo/redo · `w/b/e`, `f/t`, `gg/G`, `0/^/$` motions · counts |
| `Ctrl+E` / `F2` | open the query in `$EDITOR` (vim!) — save & quit to bring the change back |
| `Ctrl+N/P`, `Tab`/`Enter` | autocomplete: cycle / accept (insert mode) |
| `Ctrl+Space` | trigger autocomplete manually |
| `Ctrl+Backspace` (or `Alt+Backspace`) / `Ctrl+U` | delete word back / to line start (insert mode) |
| `Esc` | close the completion popup → normal mode → jump to results |

Vim mode can be turned off with `"quim": { "vimMode": false }` in the config — the
editor then behaves like a plain textbox (and `Esc` jumps straight to the results).

¹ `Ctrl+Enter` needs a terminal with the kitty keyboard protocol (e.g. WezTerm,
kitty, newer Windows Terminal). `Ctrl+R` works everywhere.

## Managing sources

`a` in the database list opens a form for adding a **server** (one connection
string, many databases — pick `all` or fetch the list and choose) or a standalone
**database**. The form has an engine selector for `mssql`, `postgres` and `sqlite`;
SQLite is a single file, so add it as a standalone database source. `e` edits the
source behind the selected entry, `d` deletes it (with confirmation). Every save
tests the connection first, then writes `config.json` atomically in quim's
format — ids (`srv_`/`db_`), camelCase keys and unknown fields are preserved.

Supported connection string formats:

```json
{
  "servers": [
    {
      "id": "srv_local",
      "name": "Local SQL Server",
      "engine": "mssql",
      "connectionString": "Server=localhost,1433;User Id=sa;Password=...;TrustServerCertificate=True;Encrypt=False",
      "databases": "all"
    },
    {
      "id": "srv_pg",
      "name": "Local Postgres",
      "engine": "postgres",
      "connectionString": "postgres://user:pass@localhost:5432/postgres",
      "databases": "all"
    }
  ],
  "databases": [
    {
      "id": "db_sqlite",
      "name": "Local SQLite",
      "engine": "sqlite",
      "connectionString": "/home/me/app.db"
    }
  ]
}
```

quim's own settings live under the `quim` key in the same file:

```json
"quim": {
  "vimMode": true,
  "layout": { "sidebarWidth": 32, "dbListHeight": 12, "editorHeight": 9, "detailWidth": 50 },
  "activeDb": "s:srv_977xnih:batman"
}
```

Layout values are written when you resize panes (`Ctrl+W` + `hjkl`); omitted
values fall back to the automatic layout. `activeDb` tracks the selected
database so a restart drops you back where you were.

## tmux & vim-tmux-navigator

Works out of the box, without tmux configuration. Two things make it seamless:

- [vim-tmux-navigator](https://github.com/christoomey/vim-tmux-navigator)
  only passes `Ctrl+H/J/K/L` through to processes whose name matches its
  vim pattern — but the pattern allows any prefix ending in `/`, so quim names
  its process **`quim/view`** (via `/proc/self/comm`) and matches stock configs
  as-is.
- Navigating against an edge inside quim (e.g. `Ctrl+H` while already in the
  leftmost pane) forwards the jump to tmux (`select-pane`), exactly like vim
  does. You glide between quim's panes and your tmux panes without friction.

If you have global tmux bindings on `C-e`/`C-w` (e.g. scripts) they never
reach quim — that's why `F2` ($EDITOR) exists as an alternative.

## Features

- **Simple source model**: servers (with `databases: "all"` or a list)
  and standalone databases, same id scheme, same name disambiguation.
- **Schema-aware autocomplete**: tables, columns, schemas and keywords.
  Understands aliases — `FROM dbo.Ackumulator a` makes `a.` suggest the right columns.
- **Modal editing**: a real vim subset in the query editor, toggleable in config.
- **Type-colored results** (string, UUID, date, number, bool, binary, NULL).
  Column names are never truncated; the last
  visible column is clipped mid-column so it's obvious there is more to scroll.
- **Cell detail**: `Enter` on a cell opens a pane on the right; JSON is
  pretty-printed and highlighted, `y` copies the value.
- **Clipboard** works on WSL (`clip.exe`), Wayland/X11 (`wl-copy`/`xclip`/`xsel`),
  macOS (`pbcopy`) and via OSC 52 as a fallback.
- DB calls run on their own thread — the UI never freezes. Connections are
  cached and reconnected automatically on a dropped connection (write queries
  are never re-run).

## Non-goals

Like other SQL consoles, quim runs whatever you feed it (read + write + DDL), with no
safety net.

## Structure

- `src/db.rs` — worker thread with tokio, tiberius for MSSQL and sqlx for PostgreSQL/SQLite
- `src/app.rs` — state + key handling (incl. the source form and resize mode)
- `src/ui.rs` — rendering (ratatui)
- `src/editor.rs` — editor buffer + completion
- `src/vim.rs` — modal editing (normal/visual mode, operators, motions)
- `src/highlight.rs` — SQL/JSON tokenizers
- `src/config.rs` — reads and writes quim's config
- `src/clipboard.rs`, `src/theme.rs`
