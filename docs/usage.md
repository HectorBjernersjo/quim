# quim usage

## Keys

| | |
| --- | --- |
| `Ctrl+H/J/K/L` | switch pane (also `Ctrl+arrows` and `Alt+HJKL`) |
| `Ctrl+R` / `F5` / `Ctrl+Enter`¹ | run the query |
| `Ctrl+W`, then `h/j/k/l` | resize the focused pane (`h/l` width, `j/k` height); saved to config |
| `j/k`, `gg`/`G`, `Ctrl+D/U` | navigate lists and results |
| `/` | filter databases/tables (`Esc` clears) |
| `a` / `e` / `t` | add / edit / test a connection (in the sidebar) |
| `d` | on a database: hide it · on a connection: delete it |
| `h` / `l` / `Space` | fold a connection open or shut |
| `Space` on a database | hide it / show it again |
| `r` | list every database on the host again |
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

## Connections

The sidebar is the connection manager. `a` adds one, `e` edits, `d` deletes
(with confirmation), `t` tests it. A connection is just a **name and a
connection string** — the engine and whether the string points at one database
or at a whole host are both read out of the string:

| Connection string | |
| --- | --- |
| `postgres://user:pass@host:5432/app` | Postgres, the database `app` |
| `postgres://user:pass@host:5432/` | Postgres, every database on the host |
| `Server=sql01,1433;User Id=sa;Password=…` | MSSQL, every database on the host |
| `Server=sql01;Database=App;…` | MSSQL, the database `App` |
| `/home/me/app.db`, `sqlite:///app.db`, `:memory:` | SQLite, one file |

The form shows what it read under the connection string as you type. When a
string is too exotic to place, the `Engine` row on that line switches from
`auto` to a specific engine, and the choice is written to the config.

A connection that points at a host unfolds in the sidebar into the databases it
has; one that points at a single database is a single row. Folds and picks are
remembered across restarts.

### Which databases a host lists

Three ways, from quickest to most thorough:

- `Space` on a database hides it, and hides it back. It stays visible but
  dimmed for the rest of the session so you can undo by eye.
- `d` on a database does the same behind a y/n prompt — the same key deletes a
  whole connection when the cursor is on the connection row, so the prompt says
  which of the two it is about.
- `e` on the connection opens the form, where the `Databases` row switches
  between `auto`, `all` and `pick`. Choosing `pick` fetches the host's current
  list and gives you a checkbox per database (`Space` ticks, `a` flips all).
  This is the one that works after a restart, when the hidden databases are no
  longer on screen to click.

`r` on a connection is the reset: it re-lists the host and turns everything
back on. It also widens a single-database connection into its whole host.

Saving tests the connection first, then writes `config.json` atomically:

```json
{
  "connections": [
    {
      "id": "conn_local",
      "name": "Local SQL Server",
      "connectionString": "Server=localhost,1433;User Id=sa;Password=...;TrustServerCertificate=True;Encrypt=False",
      "databases": "all"
    },
    {
      "id": "conn_pg",
      "name": "platform",
      "connectionString": "postgres://user:pass@localhost:5432/app",
      "databases": ["app"]
    },
    {
      "id": "conn_sqlite",
      "name": "Local SQLite",
      "connectionString": "/home/me/app.db",
      "databases": ["main"]
    }
  ]
}
```

`databases` is what the sidebar lists: `"all"` for everything the host reports,
or the picked names. An `engine` key is only written when `auto` was
overridden.

quim's own settings live under the `quim` key in the same file:

```json
"quim": {
  "vimMode": true,
  "layout": { "sidebarWidth": 32, "dbListHeight": 12, "editorHeight": 9, "detailWidth": 50 },
  "collapsed": ["conn_pg"],
  "activeDb": "conn_local:app"
}
```

Layout values are written when you resize panes (`Ctrl+W` + `hjkl`); omitted
values fall back to the automatic layout. `activeDb` tracks the selected
database so a restart drops you back where you were; `collapsed` lists the
connections that are folded shut.

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

## Feature details

- **One kind of source**: a connection. Engine and scope are derived from the
  connection string, and the sidebar is where they are managed.
- **Schema-aware autocomplete**: tables, columns, schemas and keywords.
  Understands aliases — `FROM games g` makes `g.` suggest the right columns.
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
