use std::collections::HashMap;
use std::sync::mpsc::Sender;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::clipboard;
use crate::config::{self, Config, DatabaseCfg, DbSelection, ServerCfg};
use crate::db::{Category, ColMeta, DbRequest, DbResponse, QueryOutcome, TableInfo};
use crate::editor::{Editor, Mode};
use crate::highlight;
use crate::vim;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Databases,
    Tables,
    Editor,
    Results,
    Detail,
}

pub enum Action {
    None,
    Quit,
    ExternalEdit,
}

#[derive(Clone, Copy)]
enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, PartialEq)]
pub enum StatusKind {
    Info,
    Ok,
    Err,
}

#[derive(Clone)]
pub struct DbEntry {
    pub id: String,
    pub name: String,
    pub label: String,
    pub engine: String,
    pub server: Option<String>,
    pub conn: String,
    pub database: String,
}

#[derive(Default)]
pub struct FilterList {
    pub filter: String,
    pub typing: bool,
    pub sel: usize,
    pub scroll: usize,
}

pub struct QueryView {
    pub cols: Vec<ColMeta>,
    pub rows: Vec<Vec<Option<String>>>,
    pub widths: Vec<u16>,
    pub sel_row: usize,
    pub sel_col: usize,
    pub row_off: usize,
    pub col_off: usize,
    /// Anchor row of a V (row-visual) selection.
    pub vsel: Option<usize>,
    pub error: Option<String>,
    /// Free-text search over the cells (highlight only — no filtering).
    pub search: String,
    /// True while the user is typing the search term in the status bar.
    pub searching: bool,
    /// Cached (row, col) of every matching cell for the current `search`, kept
    /// in sync via `refresh_search` so the render loop never rescans per frame.
    pub search_hits: Vec<(usize, usize)>,
}

impl QueryView {
    /// The inclusive row range selected with V, if any.
    pub fn vsel_range(&self) -> Option<(usize, usize)> {
        self.vsel
            .map(|a| (a.min(self.sel_row), a.max(self.sel_row)))
    }

    /// Recompute the cached match list after `search` changes.
    pub fn refresh_search(&mut self) {
        self.search_hits = self.matches();
    }

    /// Index of the currently selected cell within `search_hits`, if it is a
    /// match — i.e. which hit number the cursor sits on.
    pub fn search_index(&self) -> Option<usize> {
        self.search_hits
            .iter()
            .position(|&m| m == (self.sel_row, self.sel_col))
    }

    /// True when the displayed text of a cell contains the (lowercased) search
    /// term. `needle` must already be lowercased.
    pub fn cell_matches(&self, r: usize, c: usize, needle: &str) -> bool {
        let cell = self.rows.get(r).and_then(|row| row.get(c)).cloned().flatten();
        crate::ui::display_text(&cell, self.cols[c].category)
            .to_lowercase()
            .contains(needle)
    }

    /// (row, col) of every cell matching the current search term, in row-major
    /// order. Empty when no search is active.
    pub fn matches(&self) -> Vec<(usize, usize)> {
        let needle = self.search.to_lowercase();
        if needle.is_empty() {
            return vec![];
        }
        let mut out = vec![];
        for r in 0..self.rows.len() {
            for c in 0..self.cols.len() {
                if self.cell_matches(r, c, &needle) {
                    out.push((r, c));
                }
            }
        }
        out
    }
}

pub struct Detail {
    pub title: String,
    pub value: Option<String>, // None = NULL
    pub pretty_json: Option<String>,
    pub scroll: usize,
}

#[derive(Default, Clone, Copy)]
pub struct Areas {
    pub db_list: Rect,
    pub tbl_list: Rect,
    pub editor: Rect,
    pub results: Rect,
    pub detail: Rect,
}

// --- Source editor (add/edit servers & databases) --------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum SourceKind {
    Server,
    Database,
}

#[derive(Clone, Copy, PartialEq)]
pub enum FormField {
    Kind,
    Engine,
    Name,
    Conn,
    Fetch,
    AllToggle,
    DbList,
    Save,
    Cancel,
}

#[derive(Clone)]
pub struct SourceForm {
    pub kind: SourceKind,
    pub engine: String,
    pub editing_id: Option<String>,
    pub name: String,
    pub conn: String,
    pub cursor: usize, // char index in the focused text field
    pub all_dbs: bool,
    pub db_list: Vec<(String, bool)>,
    pub list_sel: usize,
    pub focus: FormField,
    pub fetching: bool,
    pub saving: bool,
    pub error: Option<String>,
}

impl SourceForm {
    fn new_add() -> Self {
        SourceForm {
            kind: SourceKind::Server,
            engine: "mssql".into(),
            editing_id: None,
            name: String::new(),
            conn: String::new(),
            cursor: 0,
            all_dbs: true,
            db_list: vec![],
            list_sel: 0,
            focus: FormField::Kind,
            fetching: false,
            saving: false,
            error: None,
        }
    }

    /// Visible fields, in tab order.
    pub fn fields(&self) -> Vec<FormField> {
        let mut out = vec![];
        if self.editing_id.is_none() {
            out.push(FormField::Kind);
        }
        out.push(FormField::Engine);
        out.push(FormField::Name);
        out.push(FormField::Conn);
        if self.kind == SourceKind::Server && config::supports_server_sources(&self.engine) {
            out.push(FormField::Fetch);
            out.push(FormField::AllToggle);
            if !self.all_dbs && !self.db_list.is_empty() {
                out.push(FormField::DbList);
            }
        }
        out.push(FormField::Save);
        out.push(FormField::Cancel);
        out
    }

    fn move_focus(&mut self, delta: isize) {
        let fields = self.fields();
        let cur = fields.iter().position(|f| *f == self.focus).unwrap_or(0);
        let next = (cur as isize + delta).rem_euclid(fields.len() as isize) as usize;
        self.focus = fields[next];
        self.cursor = match self.focus {
            FormField::Name => self.name.chars().count(),
            FormField::Conn => self.conn.chars().count(),
            _ => 0,
        };
    }

    fn text_field(&mut self) -> Option<&mut String> {
        match self.focus {
            FormField::Name => Some(&mut self.name),
            FormField::Conn => Some(&mut self.conn),
            _ => None,
        }
    }

    fn cycle_engine(&mut self) {
        const ENGINES: &[&str] = &["mssql", "postgres", "sqlite"];
        let current = config::normalize_engine(&self.engine);
        let idx = ENGINES
            .iter()
            .position(|engine| *engine == current)
            .unwrap_or(0);
        self.engine = ENGINES[(idx + 1) % ENGINES.len()].into();
        if !config::supports_server_sources(&self.engine)
            && self.kind == SourceKind::Server
            && self.editing_id.is_none()
        {
            self.kind = SourceKind::Database;
        }
    }
}

#[derive(Clone)]
pub enum SourceTarget {
    Server(String),
    Database(String),
}

// --- Settings screen --------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum SettingsSection {
    General,
    Servers,
    Databases,
}

pub struct SettingsState {
    pub section: SettingsSection,
    /// false = the section list has focus, true = the section's content.
    pub in_content: bool,
    pub row: usize,
}

/// Rows in the General section, in display order.
pub const GENERAL_ROWS: usize = 2;

pub struct App {
    pub cfg: Config,
    req_tx: Sender<DbRequest>,

    pub dbs: Vec<DbEntry>,
    pub db_list: FilterList,
    pub tbl_list: FilterList,
    server_dbs: HashMap<String, Vec<String>>,
    pub pending_servers: usize,

    pub active: Option<DbEntry>,
    pub tables: Vec<TableInfo>,
    pub tables_loading: bool,

    pub editor: Editor,
    pub view: Option<QueryView>,
    pub running: bool,
    pub detail: Option<Detail>,

    pub focus: Pane,
    pub status: (String, StatusKind),
    pub show_help: bool,
    pub areas: Areas,
    pub tick: usize,
    pending_g: bool,

    pub resize_mode: bool,
    pub form: Option<SourceForm>,
    pub confirm: Option<(String, SourceTarget)>,
    pub settings: Option<SettingsState>,
    form_token: usize,
}

impl App {
    pub fn new(cfg: Config, req_tx: Sender<DbRequest>) -> Self {
        let editor = Editor::new("SELECT 1", cfg.quim.vim_mode);
        let mut app = App {
            cfg,
            req_tx,
            dbs: vec![],
            db_list: FilterList::default(),
            tbl_list: FilterList::default(),
            server_dbs: HashMap::new(),
            pending_servers: 0,
            active: None,
            tables: vec![],
            tables_loading: false,
            editor,
            view: None,
            running: false,
            detail: None,
            focus: Pane::Databases,
            status: (String::new(), StatusKind::Info),
            show_help: false,
            areas: Areas::default(),
            tick: 0,
            pending_g: false,
            resize_mode: false,
            form: None,
            confirm: None,
            settings: None,
            form_token: 0,
        };
        app.bootstrap();
        app
    }

    fn bootstrap(&mut self) {
        if self.cfg.servers.is_empty() && self.cfg.databases.is_empty() {
            self.set_status(
                "No sources — press , to open Settings and add one".into(),
                StatusKind::Err,
            );
            return;
        }
        for server in self.cfg.servers.clone() {
            if !config::supported_engine(&server.engine) {
                self.set_status(
                    format!(
                        "{}: engine \"{}\" is not supported yet",
                        server.name, server.engine
                    ),
                    StatusKind::Err,
                );
                continue;
            }
            if !config::supports_server_sources(&server.engine) {
                self.set_status(
                    format!(
                        "{}: SQLite sources must be standalone databases",
                        server.name
                    ),
                    StatusKind::Err,
                );
                continue;
            }
            if server.databases.is_all() {
                self.pending_servers += 1;
                let _ = self.req_tx.send(DbRequest::ListDatabases {
                    server_id: server.id.clone(),
                    engine: server.engine.clone(),
                    conn: server.connection_string.clone(),
                });
            }
        }
        if self.pending_servers > 0 {
            self.set_status("Loading databases…".into(), StatusKind::Info);
        }
        self.rebuild_entries();
    }

    fn rebuild_entries(&mut self) {
        let mut out: Vec<DbEntry> = vec![];
        for s in &self.cfg.servers {
            if !config::supported_engine(&s.engine) || !config::supports_server_sources(&s.engine) {
                continue;
            }
            let names: Vec<String> = match &s.databases {
                config::DbSelection::Named(list) => list.clone(),
                config::DbSelection::All(_) => {
                    self.server_dbs.get(&s.id).cloned().unwrap_or_default()
                }
            };
            for name in names {
                out.push(DbEntry {
                    id: format!("s:{}:{}", s.id, name),
                    name: name.clone(),
                    label: String::new(),
                    engine: config::normalize_engine(&s.engine),
                    server: Some(s.name.clone()),
                    conn: s.connection_string.clone(),
                    database: name,
                });
            }
        }
        for d in &self.cfg.databases {
            if !config::supported_engine(&d.engine) {
                continue;
            }
            let database = config::database_of_engine(&d.engine, &d.connection_string)
                .unwrap_or_else(|| d.name.clone());
            out.push(DbEntry {
                id: format!("d:{}", d.id),
                name: d.name.clone(),
                label: String::new(),
                engine: config::normalize_engine(&d.engine),
                server: None,
                conn: d.connection_string.clone(),
                database,
            });
        }
        // Disambiguate labels only on name collisions.
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for e in &out {
            *counts.entry(e.name.as_str()).or_default() += 1;
        }
        let labels: Vec<String> = out
            .iter()
            .map(|e| match (&e.server, counts[e.name.as_str()] > 1) {
                (Some(srv), true) => format!("{} ({srv})", e.name),
                _ => e.name.clone(),
            })
            .collect();
        for (e, label) in out.iter_mut().zip(labels) {
            e.label = label;
        }
        out.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
        self.dbs = out;

        if let Some(active) = &self.active {
            if !self.dbs.iter().any(|d| d.id == active.id) {
                self.active = None;
            }
        }
        if self.active.is_none() {
            // Prefer the persisted selection; while server lists are still
            // loading, hold off on the first-entry fallback so the remembered
            // database wins once its server reports in.
            let remembered = self
                .cfg
                .quim
                .active_db
                .as_ref()
                .and_then(|id| self.dbs.iter().find(|d| &d.id == id))
                .cloned();
            if let Some(entry) = remembered {
                self.select_db(entry);
            } else if self.pending_servers == 0 {
                if let Some(first) = self.dbs.first().cloned() {
                    self.select_db(first);
                }
            }
        }
    }

    fn select_db(&mut self, entry: DbEntry) {
        self.tables.clear();
        self.tbl_list = FilterList::default();
        self.tables_loading = true;
        self.editor.set_tables(vec![]);
        self.set_status("Loading schema…".into(), StatusKind::Info);
        let _ = self.req_tx.send(DbRequest::Schema {
            db_id: entry.id.clone(),
            engine: entry.engine.clone(),
            conn: entry.conn.clone(),
            database: entry.database.clone(),
        });
        // Remember the selection across restarts.
        if self.cfg.quim.active_db.as_deref() != Some(entry.id.as_str()) {
            self.cfg.quim.active_db = Some(entry.id.clone());
            if let Err(e) = config::save(&self.cfg) {
                self.set_status(format!("✕ {e}"), StatusKind::Err);
            }
        }
        self.active = Some(entry);
    }

    pub fn set_status(&mut self, text: String, kind: StatusKind) {
        self.status = (text, kind);
    }

    // --- Filtered views --------------------------------------------------------

    pub fn filtered_dbs(&self) -> Vec<usize> {
        let f = self.db_list.filter.to_lowercase();
        self.dbs
            .iter()
            .enumerate()
            .filter(|(_, d)| f.is_empty() || d.label.to_lowercase().contains(&f))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn filtered_tables(&self) -> Vec<usize> {
        let f = self.tbl_list.filter.to_lowercase();
        self.tables
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                f.is_empty()
                    || format!("{}.{}", t.schema, t.name)
                        .to_lowercase()
                        .contains(&f)
            })
            .map(|(i, _)| i)
            .collect()
    }

    // --- DB responses ------------------------------------------------------------

    pub fn on_db_response(&mut self, resp: DbResponse) {
        match resp {
            DbResponse::Databases { server_id, result } => {
                if server_id.starts_with("@form:") {
                    self.on_form_fetch(server_id, result);
                    return;
                }
                self.pending_servers = self.pending_servers.saturating_sub(1);
                match result {
                    Ok(names) => {
                        self.server_dbs.insert(server_id, names);
                        if self.pending_servers == 0 && matches!(self.status.1, StatusKind::Info) {
                            self.set_status(String::new(), StatusKind::Info);
                        }
                    }
                    Err(e) => {
                        let name = self
                            .cfg
                            .servers
                            .iter()
                            .find(|s| s.id == server_id)
                            .map(|s| s.name.clone())
                            .unwrap_or(server_id);
                        self.set_status(format!("⚠ {name}: {e}"), StatusKind::Err);
                    }
                }
                self.rebuild_entries();
            }
            DbResponse::Schema { db_id, result } => {
                if self.active.as_ref().map(|a| a.id.as_str()) != Some(db_id.as_str()) {
                    return;
                }
                self.tables_loading = false;
                match result {
                    Ok(tables) => {
                        self.set_status(format!("{} tables", tables.len()), StatusKind::Info);
                        self.editor.set_tables(tables.clone());
                        self.tables = tables;
                    }
                    Err(e) => self.set_status(format!("✕ {e}"), StatusKind::Err),
                }
            }
            DbResponse::Query(outcome) => {
                self.running = false;
                self.apply_query_result(outcome);
            }
            DbResponse::TestResult { token, result } => self.on_test_result(token, result),
        }
    }

    fn apply_query_result(&mut self, outcome: QueryOutcome) {
        let QueryOutcome {
            columns,
            rows,
            error,
            elapsed_ms,
        } = outcome;
        if let Some(err) = &error {
            self.set_status(format!("✕ {elapsed_ms} ms"), StatusKind::Err);
            self.view = Some(QueryView {
                cols: vec![],
                rows: vec![],
                widths: vec![],
                sel_row: 0,
                sel_col: 0,
                row_off: 0,
                col_off: 0,
                vsel: None,
                error: Some(err.clone()),
                search: String::new(),
                searching: false,
                search_hits: vec![],
            });
            return;
        }
        if columns.is_empty() {
            self.set_status(format!("✓ Done · {elapsed_ms} ms"), StatusKind::Ok);
            self.view = None;
            return;
        }
        self.set_status(
            format!("✓ {} rows · {elapsed_ms} ms", fmt_count(rows.len() as i64)),
            StatusKind::Ok,
        );
        let widths = compute_widths(&columns, &rows);
        self.detail = None;
        self.view = Some(QueryView {
            cols: columns,
            rows,
            widths,
            sel_row: 0,
            sel_col: 0,
            row_off: 0,
            col_off: 0,
            vsel: None,
            error: None,
            search: String::new(),
            searching: false,
            search_hits: vec![],
        });
    }

    // --- Commands ------------------------------------------------------------

    pub fn run_query(&mut self) {
        if self.running {
            return;
        }
        let sql = self.editor.text().trim().to_string();
        let Some(active) = self.active.clone() else {
            self.set_status("No database selected".into(), StatusKind::Err);
            return;
        };
        if sql.is_empty() {
            return;
        }
        self.running = true;
        self.set_status("Running…".into(), StatusKind::Info);
        let _ = self.req_tx.send(DbRequest::Query {
            engine: active.engine,
            conn: active.conn,
            database: active.database,
            sql,
        });
    }

    fn yank(&mut self, text: String, what: &str) {
        match clipboard::copy(&text) {
            Ok(()) => self.set_status(format!("Copied {what}"), StatusKind::Ok),
            Err(e) => self.set_status(format!("✕ Clipboard: {e}"), StatusKind::Err),
        }
    }

    fn open_detail(&mut self) {
        let Some(view) = &self.view else { return };
        let Some(row) = view.rows.get(view.sel_row) else {
            return;
        };
        let Some(cell) = row.get(view.sel_col) else {
            return;
        };
        let title = view.cols[view.sel_col].name.clone();
        let pretty_json = cell.as_deref().and_then(highlight::try_pretty_json);
        self.detail = Some(Detail {
            title,
            value: cell.clone(),
            pretty_json,
            scroll: 0,
        });
        self.focus = Pane::Detail;
    }

    fn close_detail(&mut self) {
        self.detail = None;
        if self.focus == Pane::Detail {
            self.focus = Pane::Results;
        }
    }

    fn move_focus(&mut self, dir: Dir) {
        use Pane::*;
        let detail_open = self.detail.is_some();
        let next = match (self.focus, dir) {
            (Databases, Dir::Down) => Tables,
            (Tables, Dir::Up) => Databases,
            (Databases, Dir::Right) => Editor,
            (Tables, Dir::Right) => Results,
            (Editor, Dir::Left) => Databases,
            (Results, Dir::Left) => Tables,
            (Editor, Dir::Down) => Results,
            (Results, Dir::Up) => Editor,
            (Editor, Dir::Right) if detail_open => Detail,
            (Results, Dir::Right) if detail_open => Detail,
            (Detail, Dir::Left) => Results,
            (p, _) => p,
        };
        if next == self.focus {
            // At the edge — hand over to the surrounding tmux pane, the same
            // way vim-tmux-navigator does from inside vim.
            forward_to_tmux(dir);
        }
        self.focus = next;
    }

    // --- Keys ------------------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        if self.show_help {
            self.show_help = false;
            return Action::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        if self.resize_mode {
            return self.resize_key(key, ctrl);
        }
        if self.form.is_some() {
            return self.form_key(key, ctrl);
        }
        if self.confirm.is_some() {
            return self.confirm_key(key);
        }
        if self.settings.is_some() {
            return self.settings_key(key);
        }

        if ctrl {
            match key.code {
                KeyCode::Char('h') | KeyCode::Left => {
                    self.move_focus(Dir::Left);
                    return Action::None;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.move_focus(Dir::Down);
                    return Action::None;
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.move_focus(Dir::Up);
                    return Action::None;
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    self.move_focus(Dir::Right);
                    return Action::None;
                }
                KeyCode::Char('r') | KeyCode::Enter => {
                    self.run_query();
                    return Action::None;
                }
                KeyCode::Char('w') => {
                    self.resize_mode = true;
                    return Action::None;
                }
                _ => {}
            }
        }
        // Alt+hjkl navigates panes too (Ctrl variants may be taken by tmux).
        if key.modifiers.contains(KeyModifiers::ALT) {
            match key.code {
                KeyCode::Char('h') => {
                    self.move_focus(Dir::Left);
                    return Action::None;
                }
                KeyCode::Char('j') => {
                    self.move_focus(Dir::Down);
                    return Action::None;
                }
                KeyCode::Char('k') => {
                    self.move_focus(Dir::Up);
                    return Action::None;
                }
                KeyCode::Char('l') => {
                    self.move_focus(Dir::Right);
                    return Action::None;
                }
                // Readline-style word delete, for when tmux owns the Ctrl variant.
                KeyCode::Backspace if self.focus == Pane::Editor => {
                    self.editor.delete_word_back();
                    return Action::None;
                }
                _ => {}
            }
        }
        if key.code == KeyCode::F(5) {
            self.run_query();
            return Action::None;
        }
        // F2 opens $EDITOR from anywhere, for when tmux owns Ctrl+E.
        if key.code == KeyCode::F(2) {
            return Action::ExternalEdit;
        }
        if key.code == KeyCode::F(1) {
            self.show_help = true;
            return Action::None;
        }
        // , opens the settings screen — everywhere except the editor, where the
        // comma keeps its meaning (vim's reverse find repeat / plain text input).
        let filter_typing = match self.focus {
            Pane::Databases => self.db_list.typing,
            Pane::Tables => self.tbl_list.typing,
            Pane::Results => self.view.as_ref().is_some_and(|v| v.searching),
            _ => false,
        };
        if key.code == KeyCode::Char(',')
            && !ctrl
            && !key.modifiers.contains(KeyModifiers::ALT)
            && self.focus != Pane::Editor
            && !filter_typing
        {
            self.open_settings();
            return Action::None;
        }

        let pending_g = self.pending_g;
        self.pending_g = false;

        match self.focus {
            Pane::Databases => self.db_list_key(key, pending_g),
            Pane::Tables => self.tbl_list_key(key, pending_g),
            Pane::Editor => self.editor_key(key, ctrl),
            Pane::Results => self.results_key(key, ctrl, pending_g),
            Pane::Detail => self.detail_key(key, ctrl, pending_g),
        }
    }

    // --- Resize mode -------------------------------------------------------------

    fn resize_key(&mut self, key: KeyEvent, ctrl: bool) -> Action {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.exit_resize(),
            KeyCode::Char('w') if ctrl => self.exit_resize(),
            KeyCode::Char('h') | KeyCode::Left => self.adjust_size(-2, 0),
            KeyCode::Char('l') | KeyCode::Right => self.adjust_size(2, 0),
            KeyCode::Char('j') | KeyCode::Down => self.adjust_size(0, 1),
            KeyCode::Char('k') | KeyCode::Up => self.adjust_size(0, -1),
            _ => {}
        }
        Action::None
    }

    fn exit_resize(&mut self) {
        self.resize_mode = false;
        match config::save(&self.cfg) {
            Ok(()) => self.set_status("Layout saved".into(), StatusKind::Ok),
            Err(e) => self.set_status(format!("✕ {e}"), StatusKind::Err),
        }
    }

    /// Moves the wall between the focused pane and its neighbour in the pressed
    /// direction: `dw > 0` moves a vertical wall right, `dh > 0` moves a
    /// horizontal wall down — the same direction regardless of which side of the
    /// wall the focused pane sits on. Since growing `sidebar_width` pushes its
    /// wall right but growing `detail_width` pushes the detail wall left, the
    /// detail wall takes `-dw`.
    fn adjust_size(&mut self, dw: i32, dh: i32) {
        fn bump(slot: &mut Option<u16>, current: u16, delta: i32) {
            let base = slot.unwrap_or(current) as i32;
            *slot = Some((base + delta).clamp(3, 500) as u16);
        }
        let areas = self.areas;
        let detail_open = self.detail.is_some();
        let lay = &mut self.cfg.quim.layout;
        match self.focus {
            // right wall = sidebar boundary, bottom wall = db/tables boundary
            Pane::Databases => {
                if dw != 0 {
                    bump(&mut lay.sidebar_width, areas.db_list.width, dw);
                }
                if dh != 0 {
                    bump(&mut lay.db_list_height, areas.db_list.height, dh);
                }
            }
            // right wall = sidebar boundary, top wall = db/tables boundary
            Pane::Tables => {
                if dw != 0 {
                    bump(&mut lay.sidebar_width, areas.db_list.width, dw);
                }
                if dh != 0 {
                    bump(&mut lay.db_list_height, areas.db_list.height, dh);
                }
            }
            // left wall = sidebar boundary, bottom wall = editor/results boundary
            Pane::Editor => {
                if dw != 0 {
                    bump(&mut lay.sidebar_width, areas.db_list.width, dw);
                }
                if dh != 0 {
                    bump(&mut lay.editor_height, areas.editor.height, dh);
                }
            }
            // top wall = editor/results boundary; horizontal wall = detail
            // boundary (right) when the detail pane is open, else sidebar (left)
            Pane::Results => {
                if dw != 0 {
                    if detail_open {
                        bump(&mut lay.detail_width, areas.detail.width, -dw);
                    } else {
                        bump(&mut lay.sidebar_width, areas.db_list.width, dw);
                    }
                }
                if dh != 0 {
                    bump(&mut lay.editor_height, areas.editor.height, dh);
                }
            }
            // left wall = detail boundary; the detail pane owns no horizontal wall
            Pane::Detail => {
                if dw != 0 {
                    bump(&mut lay.detail_width, areas.detail.width, -dw);
                }
            }
        }
    }

    // --- Settings screen -------------------------------------------------------

    fn open_settings(&mut self) {
        self.settings = Some(SettingsState {
            section: SettingsSection::General,
            in_content: false,
            row: 0,
        });
    }

    fn settings_key(&mut self, key: KeyEvent) -> Action {
        let Some(st) = self.settings.as_ref() else {
            return Action::None;
        };
        let (section, in_content) = (st.section, st.in_content);
        match key.code {
            KeyCode::Char('q') | KeyCode::Char(',') => {
                self.settings = None;
                return Action::None;
            }
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.show_help = true;
                return Action::None;
            }
            _ => {}
        }

        if !in_content {
            let st = self.settings.as_mut().unwrap();
            match key.code {
                KeyCode::Esc => self.settings = None,
                KeyCode::Char('j') | KeyCode::Down => {
                    st.section = match st.section {
                        SettingsSection::General => SettingsSection::Servers,
                        _ => SettingsSection::Databases,
                    };
                    st.row = 0;
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    st.section = match st.section {
                        SettingsSection::Databases => SettingsSection::Servers,
                        _ => SettingsSection::General,
                    };
                    st.row = 0;
                }
                KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter | KeyCode::Tab => {
                    st.in_content = true;
                }
                _ => {}
            }
            return Action::None;
        }

        let len = match section {
            SettingsSection::General => GENERAL_ROWS,
            SettingsSection::Servers => self.cfg.servers.len(),
            SettingsSection::Databases => self.cfg.databases.len(),
        };
        {
            let st = self.settings.as_mut().unwrap();
            match key.code {
                KeyCode::Esc
                | KeyCode::Char('h')
                | KeyCode::Left
                | KeyCode::Tab
                | KeyCode::BackTab => {
                    st.in_content = false;
                    return Action::None;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    st.row = (st.row + 1).min(len.saturating_sub(1));
                    return Action::None;
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    st.row = st.row.saturating_sub(1);
                    return Action::None;
                }
                _ => {}
            }
        }

        let row = self.settings.as_ref().unwrap().row;
        match section {
            SettingsSection::General => {
                if matches!(key.code, KeyCode::Char(' ') | KeyCode::Enter) {
                    match row {
                        0 => self.toggle_vim_mode(),
                        1 => self.reset_layout(),
                        _ => {}
                    }
                }
            }
            SettingsSection::Servers | SettingsSection::Databases => match key.code {
                KeyCode::Char('a') => self.settings_add(section),
                KeyCode::Char('e') | KeyCode::Enter => self.settings_edit(section, row),
                KeyCode::Char('d') => self.settings_delete(section, row),
                KeyCode::Char('t') => self.settings_test(section, row),
                _ => {}
            },
        }
        Action::None
    }

    fn toggle_vim_mode(&mut self) {
        let on = !self.cfg.quim.vim_mode;
        self.cfg.quim.vim_mode = on;
        if on {
            self.editor.mode = Mode::Normal;
            self.editor.clamp_normal_col();
        } else {
            self.editor.mode = Mode::Insert;
        }
        match config::save(&self.cfg) {
            Ok(()) => self.set_status(
                format!("Vim mode {}", if on { "on" } else { "off" }),
                StatusKind::Ok,
            ),
            Err(e) => self.set_status(format!("✕ {e}"), StatusKind::Err),
        }
    }

    fn reset_layout(&mut self) {
        self.cfg.quim.layout = config::LayoutCfg::default();
        match config::save(&self.cfg) {
            Ok(()) => self.set_status("Pane layout reset".into(), StatusKind::Ok),
            Err(e) => self.set_status(format!("✕ {e}"), StatusKind::Err),
        }
    }

    fn settings_add(&mut self, section: SettingsSection) {
        self.form_token += 1;
        let mut form = SourceForm::new_add();
        if section == SettingsSection::Databases {
            form.kind = SourceKind::Database;
        }
        form.focus = FormField::Name;
        self.form = Some(form);
    }

    fn settings_edit(&mut self, section: SettingsSection, row: usize) {
        let target = match section {
            SettingsSection::Servers => self
                .cfg
                .servers
                .get(row)
                .map(|s| SourceTarget::Server(s.id.clone())),
            SettingsSection::Databases => self
                .cfg
                .databases
                .get(row)
                .map(|d| SourceTarget::Database(d.id.clone())),
            SettingsSection::General => None,
        };
        if let Some(target) = target {
            self.open_edit_form_for(target);
        }
    }

    fn settings_delete(&mut self, section: SettingsSection, row: usize) {
        match section {
            SettingsSection::Servers => {
                if let Some(s) = self.cfg.servers.get(row) {
                    self.confirm = Some((
                        format!("Delete server \"{}\" and all its databases? (y/n)", s.name),
                        SourceTarget::Server(s.id.clone()),
                    ));
                }
            }
            SettingsSection::Databases => {
                if let Some(d) = self.cfg.databases.get(row) {
                    self.confirm = Some((
                        format!("Delete database \"{}\"? (y/n)", d.name),
                        SourceTarget::Database(d.id.clone()),
                    ));
                }
            }
            SettingsSection::General => {}
        }
    }

    fn settings_test(&mut self, section: SettingsSection, row: usize) {
        let (name, engine, conn, database) = match section {
            SettingsSection::Servers => {
                let Some(s) = self.cfg.servers.get(row) else {
                    return;
                };
                (
                    s.name.clone(),
                    config::normalize_engine(&s.engine),
                    s.connection_string.clone(),
                    config::server_test_database(&s.engine),
                )
            }
            SettingsSection::Databases => {
                let Some(d) = self.cfg.databases.get(row) else {
                    return;
                };
                let db =
                    config::database_of_engine(&d.engine, &d.connection_string).unwrap_or_default();
                (
                    d.name.clone(),
                    config::normalize_engine(&d.engine),
                    d.connection_string.clone(),
                    db,
                )
            }
            SettingsSection::General => return,
        };
        self.set_status(format!("Testing {name}…"), StatusKind::Info);
        let _ = self.req_tx.send(DbRequest::TestConnection {
            token: format!("@ping:{name}"),
            engine,
            conn,
            database,
        });
    }

    // --- Source form ---------------------------------------------------------------

    fn open_add_form(&mut self) {
        self.form_token += 1;
        self.form = Some(SourceForm::new_add());
    }

    fn open_edit_form(&mut self) {
        let Some(&idx) = self.filtered_dbs().get(self.db_list.sel) else {
            return;
        };
        let entry = self.dbs[idx].clone();
        let Some((target, _)) = self.source_of(&entry) else {
            return;
        };
        self.open_edit_form_for(target);
    }

    fn open_edit_form_for(&mut self, target: SourceTarget) {
        self.form_token += 1;
        let mut form = SourceForm::new_add();
        form.focus = FormField::Name;
        match target {
            SourceTarget::Server(id) => {
                let Some(s) = self.cfg.servers.iter().find(|s| s.id == id) else {
                    return;
                };
                form.kind = SourceKind::Server;
                form.engine = config::normalize_engine(&s.engine);
                form.editing_id = Some(s.id.clone());
                form.name = s.name.clone();
                form.conn = s.connection_string.clone();
                form.all_dbs = s.databases.is_all();
                if let DbSelection::Named(names) = &s.databases {
                    form.db_list = names.iter().map(|n| (n.clone(), true)).collect();
                }
            }
            SourceTarget::Database(id) => {
                let Some(d) = self.cfg.databases.iter().find(|d| d.id == id) else {
                    return;
                };
                form.kind = SourceKind::Database;
                form.engine = config::normalize_engine(&d.engine);
                form.editing_id = Some(d.id.clone());
                form.name = d.name.clone();
                form.conn = d.connection_string.clone();
            }
        }
        form.cursor = form.name.chars().count();
        self.form = Some(form);
    }

    /// The config source behind a database entry, plus a human label for it.
    fn source_of(&self, entry: &DbEntry) -> Option<(SourceTarget, String)> {
        if let Some(rest) = entry.id.strip_prefix("s:") {
            let srv_id = rest.split_once(':').map(|(id, _)| id)?;
            let name = self
                .cfg
                .servers
                .iter()
                .find(|s| s.id == srv_id)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| srv_id.to_string());
            Some((
                SourceTarget::Server(srv_id.to_string()),
                format!("server \"{name}\""),
            ))
        } else if let Some(id) = entry.id.strip_prefix("d:") {
            Some((
                SourceTarget::Database(id.to_string()),
                format!("database \"{}\"", entry.name),
            ))
        } else {
            None
        }
    }

    fn open_confirm_delete(&mut self) {
        let Some(&idx) = self.filtered_dbs().get(self.db_list.sel) else {
            return;
        };
        let entry = self.dbs[idx].clone();
        if let Some((target, label)) = self.source_of(&entry) {
            let note = match &target {
                SourceTarget::Server(_) => " and all its databases",
                SourceTarget::Database(_) => "",
            };
            self.confirm = Some((format!("Delete {label}{note}? (y/n)"), target));
        }
    }

    fn confirm_key(&mut self, key: KeyEvent) -> Action {
        let Some((_, target)) = self.confirm.take() else {
            return Action::None;
        };
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                match &target {
                    SourceTarget::Server(id) => {
                        self.cfg.servers.retain(|s| &s.id != id);
                        self.server_dbs.remove(id);
                    }
                    SourceTarget::Database(id) => self.cfg.databases.retain(|d| &d.id != id),
                }
                match config::save(&self.cfg) {
                    Ok(()) => self.set_status("Source deleted".into(), StatusKind::Ok),
                    Err(e) => self.set_status(format!("✕ {e}"), StatusKind::Err),
                }
                self.rebuild_entries();
            }
            _ => {}
        }
        Action::None
    }

    fn form_key(&mut self, key: KeyEvent, ctrl: bool) -> Action {
        let Some(form) = self.form.as_mut() else {
            return Action::None;
        };
        if form.saving {
            // Only Esc is honored while the connection test runs.
            if key.code == KeyCode::Esc {
                self.form = None;
            }
            return Action::None;
        }
        match key.code {
            KeyCode::Esc => {
                self.form = None;
                return Action::None;
            }
            KeyCode::Tab => {
                form.move_focus(1);
                return Action::None;
            }
            KeyCode::BackTab => {
                form.move_focus(-1);
                return Action::None;
            }
            _ => {}
        }

        match form.focus {
            FormField::Name | FormField::Conn => match key.code {
                KeyCode::Enter => form.move_focus(1),
                KeyCode::Up => form.move_focus(-1),
                KeyCode::Down => form.move_focus(1),
                KeyCode::Left => form.cursor = form.cursor.saturating_sub(1),
                KeyCode::Right => {
                    let len = form.text_field().map(|t| t.chars().count()).unwrap_or(0);
                    form.cursor = (form.cursor + 1).min(len);
                }
                KeyCode::Home => form.cursor = 0,
                KeyCode::End => {
                    form.cursor = form.text_field().map(|t| t.chars().count()).unwrap_or(0)
                }
                KeyCode::Backspace => {
                    let cur = form.cursor;
                    if cur > 0 {
                        if let Some(text) = form.text_field() {
                            let idx = char_byte(text, cur - 1);
                            text.remove(idx);
                        }
                        form.cursor = cur - 1;
                    }
                }
                KeyCode::Delete => {
                    let cur = form.cursor;
                    if let Some(text) = form.text_field() {
                        if cur < text.chars().count() {
                            let idx = char_byte(text, cur);
                            text.remove(idx);
                        }
                    }
                }
                KeyCode::Char('u') if ctrl => {
                    if let Some(text) = form.text_field() {
                        text.clear();
                    }
                    form.cursor = 0;
                }
                KeyCode::Char(c) if !ctrl => {
                    let cur = form.cursor;
                    if let Some(text) = form.text_field() {
                        let idx = char_byte(text, cur);
                        text.insert(idx, c);
                    }
                    form.cursor = cur + 1;
                }
                _ => {}
            },
            FormField::Kind => match key.code {
                KeyCode::Char('h')
                | KeyCode::Char('l')
                | KeyCode::Char(' ')
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Enter => {
                    if form.kind == SourceKind::Server {
                        form.kind = SourceKind::Database;
                    } else if config::supports_server_sources(&form.engine) {
                        form.kind = SourceKind::Server;
                    }
                }
                KeyCode::Up => form.move_focus(-1),
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k') => form.move_focus(1),
                _ => {}
            },
            FormField::Engine => match key.code {
                KeyCode::Char('h')
                | KeyCode::Char('l')
                | KeyCode::Char(' ')
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Enter => form.cycle_engine(),
                KeyCode::Up | KeyCode::Char('k') => form.move_focus(-1),
                KeyCode::Down | KeyCode::Char('j') => form.move_focus(1),
                _ => {}
            },
            FormField::AllToggle => match key.code {
                KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Char('h') | KeyCode::Char('l') => {
                    form.all_dbs = !form.all_dbs;
                }
                KeyCode::Up | KeyCode::Char('k') => form.move_focus(-1),
                KeyCode::Down | KeyCode::Char('j') => form.move_focus(1),
                _ => {}
            },
            FormField::DbList => match key.code {
                KeyCode::Char(' ') | KeyCode::Enter => {
                    if let Some(item) = form.db_list.get_mut(form.list_sel) {
                        item.1 = !item.1;
                    }
                }
                KeyCode::Char('a') => {
                    let all_on = form.db_list.iter().all(|(_, on)| *on);
                    for item in &mut form.db_list {
                        item.1 = !all_on;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    form.list_sel = (form.list_sel + 1).min(form.db_list.len().saturating_sub(1));
                }
                KeyCode::Up | KeyCode::Char('k') => form.list_sel = form.list_sel.saturating_sub(1),
                _ => {}
            },
            FormField::Fetch => match key.code {
                KeyCode::Enter | KeyCode::Char(' ') => self.form_fetch(),
                KeyCode::Up | KeyCode::Char('k') => form.move_focus(-1),
                KeyCode::Down | KeyCode::Char('j') => form.move_focus(1),
                _ => {}
            },
            FormField::Save => match key.code {
                KeyCode::Enter | KeyCode::Char(' ') => self.form_save(),
                KeyCode::Up | KeyCode::Char('k') => form.move_focus(-1),
                KeyCode::Down | KeyCode::Char('j') => form.move_focus(1),
                KeyCode::Char('h') | KeyCode::Left => form.move_focus(-1),
                KeyCode::Char('l') | KeyCode::Right => form.move_focus(1),
                _ => {}
            },
            FormField::Cancel => match key.code {
                KeyCode::Enter | KeyCode::Char(' ') => self.form = None,
                KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('h') | KeyCode::Left => {
                    form.move_focus(-1)
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('l') | KeyCode::Right => {
                    form.move_focus(1)
                }
                _ => {}
            },
        }
        Action::None
    }

    fn form_fetch(&mut self) {
        let token = self.form_token;
        let Some(form) = self.form.as_mut() else {
            return;
        };
        if form.conn.trim().is_empty() {
            form.error = Some("Enter a connection string first".into());
            return;
        }
        if !config::supports_server_sources(&form.engine) {
            form.error =
                Some("SQLite is a single database file; add it as a database source.".into());
            return;
        }
        form.error = None;
        form.fetching = true;
        let _ = self.req_tx.send(DbRequest::ListDatabases {
            server_id: format!("@form:{token}"),
            engine: config::normalize_engine(&form.engine),
            conn: form.conn.trim().to_string(),
        });
    }

    fn on_form_fetch(&mut self, server_id: String, result: Result<Vec<String>, String>) {
        if server_id != format!("@form:{}", self.form_token) {
            return; // stale response from a closed form
        }
        let Some(form) = self.form.as_mut() else {
            return;
        };
        form.fetching = false;
        match result {
            Ok(names) => {
                let previously: HashMap<String, bool> = form.db_list.drain(..).collect();
                form.db_list = names
                    .into_iter()
                    .map(|n| {
                        let on = previously.get(&n).copied().unwrap_or(previously.is_empty());
                        (n, on)
                    })
                    .collect();
                form.list_sel = 0;
                form.error = None;
            }
            Err(e) => form.error = Some(e),
        }
    }

    fn form_save(&mut self) {
        let token = self.form_token;
        let Some(form) = self.form.as_mut() else {
            return;
        };
        form.name = form.name.trim().to_string();
        form.conn = form.conn.trim().to_string();
        if form.name.is_empty() {
            form.error = Some("Name is required".into());
            return;
        }
        if form.conn.is_empty() {
            form.error = Some("Connection string is required".into());
            return;
        }
        form.engine = config::normalize_engine(&form.engine);
        if !config::supported_engine(&form.engine) {
            form.error = Some(format!("Engine \"{}\" is not supported", form.engine));
            return;
        }
        if form.kind == SourceKind::Server && !config::supports_server_sources(&form.engine) {
            form.error =
                Some("SQLite is a single database file; add it as a database source.".into());
            return;
        }
        if form.kind == SourceKind::Server
            && !form.all_dbs
            && !form.db_list.iter().any(|(_, on)| *on)
        {
            form.error = Some("Select at least one database, or use all".into());
            return;
        }
        form.error = None;
        form.saving = true;
        let database = match form.kind {
            SourceKind::Server => config::server_test_database(&form.engine),
            SourceKind::Database => {
                config::database_of_engine(&form.engine, &form.conn).unwrap_or_default()
            }
        };
        let _ = self.req_tx.send(DbRequest::TestConnection {
            token: format!("@save:{token}"),
            engine: form.engine.clone(),
            conn: form.conn.clone(),
            database,
        });
    }

    fn on_test_result(&mut self, token: String, result: Result<(), String>) {
        // Standalone connection test from the settings screen (t on a source).
        if let Some(name) = token.strip_prefix("@ping:") {
            match result {
                Ok(()) => self.set_status(format!("✓ {name}: connection ok"), StatusKind::Ok),
                Err(e) => self.set_status(format!("✕ {name}: {e}"), StatusKind::Err),
            }
            return;
        }
        if token != format!("@save:{}", self.form_token) {
            return;
        }
        let Some(form) = self.form.as_mut() else {
            return;
        };
        form.saving = false;
        match result {
            Err(e) => form.error = Some(format!("Could not connect: {e}")),
            Ok(()) => self.commit_form(),
        }
    }

    fn commit_form(&mut self) {
        let Some(form) = self.form.clone() else {
            return;
        };
        let engine = config::normalize_engine(&form.engine);
        match form.kind {
            SourceKind::Server => {
                let id = form
                    .editing_id
                    .clone()
                    .unwrap_or_else(|| config::new_id("srv"));
                let databases = if form.all_dbs {
                    DbSelection::all()
                } else {
                    DbSelection::Named(
                        form.db_list
                            .iter()
                            .filter(|(_, on)| *on)
                            .map(|(n, _)| n.clone())
                            .collect(),
                    )
                };
                let extra = self
                    .cfg
                    .servers
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| s.extra.clone())
                    .unwrap_or_default();
                let entry = ServerCfg {
                    id: id.clone(),
                    name: form.name.clone(),
                    engine: engine.clone(),
                    connection_string: form.conn.clone(),
                    databases,
                    extra,
                };
                match self.cfg.servers.iter().position(|s| s.id == id) {
                    Some(i) => self.cfg.servers[i] = entry,
                    None => self.cfg.servers.push(entry),
                }
                if let Err(e) = config::save(&self.cfg) {
                    if let Some(f) = self.form.as_mut() {
                        f.error = Some(e);
                    }
                    return;
                }
                self.form = None;
                if form.all_dbs {
                    self.server_dbs.remove(&id);
                    self.pending_servers += 1;
                    let _ = self.req_tx.send(DbRequest::ListDatabases {
                        server_id: id,
                        engine,
                        conn: form.conn.clone(),
                    });
                }
            }
            SourceKind::Database => {
                let id = form
                    .editing_id
                    .clone()
                    .unwrap_or_else(|| config::new_id("db"));
                let extra = self
                    .cfg
                    .databases
                    .iter()
                    .find(|d| d.id == id)
                    .map(|d| d.extra.clone())
                    .unwrap_or_default();
                let entry = DatabaseCfg {
                    id: id.clone(),
                    name: form.name.clone(),
                    engine,
                    connection_string: form.conn.clone(),
                    extra,
                };
                match self.cfg.databases.iter().position(|d| d.id == id) {
                    Some(i) => self.cfg.databases[i] = entry,
                    None => self.cfg.databases.push(entry),
                }
                if let Err(e) = config::save(&self.cfg) {
                    if let Some(f) = self.form.as_mut() {
                        f.error = Some(e);
                    }
                    return;
                }
                self.form = None;
            }
        }
        self.set_status(format!("Saved {}", form.name), StatusKind::Ok);
        self.rebuild_entries();
    }

    // --- Pane keys -------------------------------------------------------------

    fn db_list_key(&mut self, key: KeyEvent, pending_g: bool) -> Action {
        if !self.db_list.typing {
            match key.code {
                KeyCode::Char('a') => {
                    self.open_add_form();
                    return Action::None;
                }
                KeyCode::Char('e') => {
                    self.open_edit_form();
                    return Action::None;
                }
                KeyCode::Char('d') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.open_confirm_delete();
                    return Action::None;
                }
                _ => {}
            }
        }
        let len = self.filtered_dbs().len();
        if let Some(action) = list_common(
            &mut self.db_list,
            len,
            key,
            pending_g,
            &mut self.pending_g,
            &mut self.show_help,
        ) {
            return action;
        }
        if key.code == KeyCode::Enter {
            // A single Enter both confirms an active filter and selects the
            // highlighted db — no need to press it twice.
            self.db_list.typing = false;
            if let Some(&idx) = self.filtered_dbs().get(self.db_list.sel) {
                let entry = self.dbs[idx].clone();
                if self.active.as_ref().map(|a| a.id.as_str()) != Some(entry.id.as_str()) {
                    self.select_db(entry);
                }
            }
        }
        Action::None
    }

    fn tbl_list_key(&mut self, key: KeyEvent, pending_g: bool) -> Action {
        let len = self.filtered_tables().len();
        if let Some(action) = list_common(
            &mut self.tbl_list,
            len,
            key,
            pending_g,
            &mut self.pending_g,
            &mut self.show_help,
        ) {
            return action;
        }
        if key.code == KeyCode::Enter {
            // A single Enter both confirms an active filter and previews the
            // highlighted table — no need to press it twice.
            self.tbl_list.typing = false;
            if let Some(&idx) = self.filtered_tables().get(self.tbl_list.sel) {
                let t = &self.tables[idx];
                let engine = self
                    .active
                    .as_ref()
                    .map(|db| db.engine.as_str())
                    .unwrap_or("mssql");
                let sql = preview_query(engine, &t.schema, &t.name);
                self.editor.set_text(&sql);
                self.focus = Pane::Results;
                self.run_query();
            }
        }
        Action::None
    }

    fn editor_key(&mut self, key: KeyEvent, ctrl: bool) -> Action {
        if ctrl && key.code == KeyCode::Char('e') {
            return Action::ExternalEdit;
        }
        // Modal editing: normal/visual mode keys live in vim.rs.
        if self.cfg.quim.vim_mode && self.editor.mode != Mode::Insert {
            match vim::handle_key(&mut self.editor, key) {
                vim::Outcome::Consumed => {}
                vim::Outcome::RunQuery => self.run_query(),
            }
            return Action::None;
        }
        if ctrl {
            match key.code {
                KeyCode::Char(' ') | KeyCode::Char('@') => {
                    self.editor.trigger_completion();
                    return Action::None;
                }
                KeyCode::Char('n') => {
                    if self.editor.completion.is_some() {
                        self.editor.completion_move(1);
                    } else {
                        self.editor.trigger_completion();
                    }
                    return Action::None;
                }
                KeyCode::Char('p') => {
                    self.editor.completion_move(-1);
                    return Action::None;
                }
                KeyCode::Char('y') => {
                    self.editor.accept_completion();
                    return Action::None;
                }
                KeyCode::Char('a') => {
                    self.editor.home();
                    return Action::None;
                }
                KeyCode::Char('u') => {
                    self.editor.kill_to_line_start();
                    return Action::None;
                }
                // Ctrl+W is the resize prefix now; word deletion moved here.
                KeyCode::Backspace => {
                    self.editor.delete_word_back();
                    return Action::None;
                }
                KeyCode::Delete => {
                    self.editor.delete_word_forward();
                    return Action::None;
                }
                _ => {}
            }
        }
        let word_jump = key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => {
                if self.editor.completion.is_some() {
                    self.editor.completion = None;
                } else if self.cfg.quim.vim_mode {
                    // Like vim: leaving insert mode steps the cursor back one.
                    self.editor.mode = Mode::Normal;
                    self.editor.col = self.editor.col.saturating_sub(1);
                    self.editor.clamp_normal_col();
                } else {
                    self.focus = Pane::Results;
                }
            }
            KeyCode::Enter | KeyCode::Tab if self.editor.completion.is_some() => {
                self.editor.accept_completion();
            }
            KeyCode::Enter => self.editor.newline(true),
            KeyCode::Tab => self.editor.insert_str("  "),
            KeyCode::Backspace => self.editor.backspace(),
            KeyCode::Delete => self.editor.delete(),
            KeyCode::Left if word_jump => self.editor.word_left(),
            KeyCode::Right if word_jump => self.editor.word_right(),
            KeyCode::Left => self.editor.move_left(),
            KeyCode::Right => self.editor.move_right(),
            KeyCode::Up if self.editor.completion.is_some() => self.editor.completion_move(-1),
            KeyCode::Down if self.editor.completion.is_some() => self.editor.completion_move(1),
            KeyCode::Up => self.editor.move_up(),
            KeyCode::Down => self.editor.move_down(),
            KeyCode::Home => self.editor.home(),
            KeyCode::End => self.editor.end(),
            KeyCode::PageUp => self.editor.doc_start(),
            KeyCode::PageDown => self.editor.doc_end(),
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.editor.insert_char(c)
            }
            _ => {}
        }
        Action::None
    }

    /// Move the cursor to the next/previous cell matching the search term.
    /// `include_current` keeps the cursor put if it already sits on a match
    /// (used when committing a fresh search with Enter).
    fn search_jump(&mut self, forward: bool, include_current: bool) {
        let Some(view) = self.view.as_ref() else {
            return;
        };
        if view.search.is_empty() {
            return;
        }
        let hits = &view.search_hits;
        if hits.is_empty() {
            self.set_status(format!("/{}  — no matches", view.search), StatusKind::Info);
            return;
        }
        let cur = (view.sel_row, view.sel_col);
        let target = if forward {
            hits.iter()
                .find(|&&m| if include_current { m >= cur } else { m > cur })
                .copied()
                .unwrap_or(hits[0])
        } else {
            hits.iter()
                .rev()
                .find(|&&m| if include_current { m <= cur } else { m < cur })
                .copied()
                .unwrap_or(*hits.last().unwrap())
        };
        let idx = hits.iter().position(|&m| m == target).unwrap_or(0);
        let total = hits.len();
        let term = view.search.clone();
        let view = self.view.as_mut().unwrap();
        view.sel_row = target.0;
        view.sel_col = target.1;
        self.set_status(format!("/{term}  [{}/{total}]", idx + 1), StatusKind::Info);
    }

    fn results_key(&mut self, key: KeyEvent, ctrl: bool, pending_g: bool) -> Action {
        let page = (self.areas.results.height.saturating_sub(3) as usize).max(1);
        let Some(view) = &mut self.view else {
            return match key.code {
                KeyCode::Char('q') => Action::Quit,
                KeyCode::Char('?') => {
                    self.show_help = true;
                    Action::None
                }
                _ => Action::None,
            };
        };
        // Typing a search term: keystrokes rebuild the match set live, highlight
        // it and jump to the first hit so results track incrementally. Enter
        // commits (keeping the current match), Esc cancels.
        if view.searching {
            let mut term_changed = false;
            match key.code {
                KeyCode::Esc => {
                    view.search.clear();
                    view.searching = false;
                    view.search_hits.clear();
                }
                KeyCode::Backspace => {
                    if view.search.pop().is_none() {
                        view.searching = false;
                    } else {
                        term_changed = true;
                    }
                }
                KeyCode::Enter => view.searching = false,
                KeyCode::Char(c) if !ctrl => {
                    view.search.push(c);
                    term_changed = true;
                }
                _ => {}
            }
            if term_changed {
                view.refresh_search();
                if let Some(&(r, c)) = view.search_hits.first() {
                    view.sel_row = r;
                    view.sel_col = c;
                }
            }
            if key.code == KeyCode::Enter {
                // Refresh the status line to the committed [i/total] readout.
                self.search_jump(true, true);
            }
            return Action::None;
        }
        let max_row = view.rows.len().saturating_sub(1);
        let max_col = view.cols.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => view.sel_row = (view.sel_row + 1).min(max_row),
            KeyCode::Char('k') | KeyCode::Up => view.sel_row = view.sel_row.saturating_sub(1),
            KeyCode::Char('h') | KeyCode::Char('b') | KeyCode::Left => {
                view.sel_col = view.sel_col.saturating_sub(1)
            }
            KeyCode::Char('l') | KeyCode::Char('w') | KeyCode::Right => {
                view.sel_col = (view.sel_col + 1).min(max_col)
            }
            KeyCode::Char('d') if ctrl => view.sel_row = (view.sel_row + page / 2).min(max_row),
            KeyCode::Char('u') if ctrl => view.sel_row = view.sel_row.saturating_sub(page / 2),
            KeyCode::PageDown => view.sel_row = (view.sel_row + page).min(max_row),
            KeyCode::PageUp => view.sel_row = view.sel_row.saturating_sub(page),
            KeyCode::Char('g') => {
                if pending_g {
                    view.sel_row = 0;
                } else {
                    self.pending_g = true;
                }
            }
            KeyCode::Char('G') | KeyCode::End => view.sel_row = max_row,
            KeyCode::Home => view.sel_row = 0,
            KeyCode::Char('0') | KeyCode::Char('^') => view.sel_col = 0,
            KeyCode::Char('$') => view.sel_col = max_col,
            KeyCode::Char('V') => {
                view.vsel = match view.vsel {
                    Some(_) => None,
                    None if view.rows.is_empty() => None,
                    None => Some(view.sel_row),
                };
            }
            KeyCode::Enter | KeyCode::Char('o') => self.open_detail(),
            KeyCode::Char('y') => {
                if let Some((lo, hi)) = view.vsel_range() {
                    let text = rows_as_json(view, lo, hi);
                    view.vsel = None;
                    let n = hi - lo + 1;
                    self.yank(text, &format!("{n} row(s) as JSON"));
                } else if let Some(cell) = view
                    .rows
                    .get(view.sel_row)
                    .and_then(|r| r.get(view.sel_col))
                {
                    let text = cell.clone().unwrap_or_default();
                    self.yank(text, "cell");
                }
            }
            KeyCode::Char('Y') => {
                if let Some((lo, hi)) = view.vsel_range() {
                    let text = rows_as_markdown(view, lo, hi);
                    view.vsel = None;
                    let n = hi - lo + 1;
                    self.yank(text, &format!("{n} row(s) as markdown"));
                } else if let Some(row) = view.rows.get(view.sel_row) {
                    let text = row
                        .iter()
                        .map(|c| c.clone().unwrap_or_default())
                        .collect::<Vec<_>>()
                        .join("\t");
                    self.yank(text, "row");
                }
            }
            KeyCode::Char('r') => self.run_query(),
            KeyCode::Char('/') => {
                view.search.clear();
                view.search_hits.clear();
                view.searching = true;
            }
            KeyCode::Char('n') => self.search_jump(true, false),
            KeyCode::Char('p') | KeyCode::Char('N') => self.search_jump(false, false),
            KeyCode::Esc => {
                if view.searching {
                    view.searching = false;
                } else if !view.search.is_empty() {
                    view.search.clear();
                    view.search_hits.clear();
                } else if view.vsel.is_some() {
                    view.vsel = None;
                } else {
                    self.close_detail();
                }
            }
            KeyCode::Char('q') => {
                if self.detail.is_some() {
                    self.close_detail();
                } else {
                    return Action::Quit;
                }
            }
            KeyCode::Char('?') => self.show_help = true,
            _ => {}
        }
        Action::None
    }

    fn detail_key(&mut self, key: KeyEvent, ctrl: bool, pending_g: bool) -> Action {
        let page = (self.areas.detail.height.saturating_sub(2) as usize).max(1);
        let Some(detail) = &mut self.detail else {
            self.focus = Pane::Results;
            return Action::None;
        };
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => detail.scroll += 1,
            KeyCode::Char('k') | KeyCode::Up => detail.scroll = detail.scroll.saturating_sub(1),
            KeyCode::Char('d') if ctrl => detail.scroll += page / 2,
            KeyCode::Char('u') if ctrl => detail.scroll = detail.scroll.saturating_sub(page / 2),
            KeyCode::PageDown => detail.scroll += page,
            KeyCode::PageUp => detail.scroll = detail.scroll.saturating_sub(page),
            KeyCode::Char('g') => {
                if pending_g {
                    detail.scroll = 0;
                } else {
                    self.pending_g = true;
                }
            }
            KeyCode::Char('G') => detail.scroll = usize::MAX / 2, // clamped at draw
            KeyCode::Char('y') => {
                let text = detail
                    .pretty_json
                    .clone()
                    .or_else(|| detail.value.clone())
                    .unwrap_or_default();
                self.yank(text, "value");
            }
            KeyCode::Esc | KeyCode::Char('q') => self.close_detail(),
            KeyCode::Char('?') => self.show_help = true,
            _ => {}
        }
        Action::None
    }

    pub fn on_paste(&mut self, text: String) {
        match self.focus {
            Pane::Editor => {
                if self.cfg.quim.vim_mode && self.editor.mode != Mode::Insert {
                    self.editor.push_undo();
                }
                self.editor.insert_str(&text);
            }
            Pane::Databases if self.db_list.typing => {
                self.db_list
                    .filter
                    .push_str(text.replace(['\n', '\r'], "").as_str());
                self.db_list.sel = 0;
            }
            Pane::Tables if self.tbl_list.typing => {
                self.tbl_list
                    .filter
                    .push_str(text.replace(['\n', '\r'], "").as_str());
                self.tbl_list.sel = 0;
            }
            _ => {
                if let Some(form) = self.form.as_mut() {
                    let cur = form.cursor;
                    if let Some(field) = form.text_field() {
                        let clean = text.replace(['\n', '\r'], "");
                        let idx = char_byte(field, cur);
                        field.insert_str(idx, &clean);
                        form.cursor = cur + clean.chars().count();
                    }
                }
            }
        }
    }
}

pub fn preview_query(engine: &str, schema: &str, table: &str) -> String {
    match config::normalize_engine(engine).as_str() {
        "mssql" => format!(
            "SELECT TOP 100 *\nFROM {}.{}",
            bracket_ident(schema),
            bracket_ident(table)
        ),
        _ => format!(
            "SELECT *\nFROM {}.{}\nLIMIT 100",
            quote_ident(schema),
            quote_ident(table)
        ),
    }
}

fn bracket_ident(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn char_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Ask the enclosing tmux to move focus in `dir` (no-op outside tmux).
fn forward_to_tmux(dir: Dir) {
    if std::env::var_os("TMUX").is_none() {
        return;
    }
    let flag = match dir {
        Dir::Left => "-L",
        Dir::Down => "-D",
        Dir::Up => "-U",
        Dir::Right => "-R",
    };
    if let Ok(mut child) = std::process::Command::new("tmux")
        .args(["select-pane", flag])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        // Reap in the background so we don't block the UI or leave zombies.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Keys shared by the two filterable lists. Returns Some(action) when handled.
fn list_common(
    list: &mut FilterList,
    len: usize,
    key: KeyEvent,
    pending_g: bool,
    set_pending_g: &mut bool,
    show_help: &mut bool,
) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if list.typing {
        match key.code {
            KeyCode::Esc => {
                list.filter.clear();
                list.typing = false;
                list.sel = 0;
            }
            KeyCode::Enter => return None, // handled by caller (exits typing)
            KeyCode::Backspace => {
                if list.filter.pop().is_none() {
                    list.typing = false;
                }
                list.sel = 0;
            }
            KeyCode::Up => list.sel = list.sel.saturating_sub(1),
            KeyCode::Down => list.sel = (list.sel + 1).min(len.saturating_sub(1)),
            KeyCode::Char(c) if !ctrl => {
                list.filter.push(c);
                list.sel = 0;
            }
            _ => {}
        }
        return Some(Action::None);
    }
    let max = len.saturating_sub(1);
    match key.code {
        KeyCode::Char('/') => {
            list.typing = true;
            list.filter.clear();
            list.sel = 0;
        }
        KeyCode::Char('j') | KeyCode::Down => list.sel = (list.sel + 1).min(max),
        KeyCode::Char('k') | KeyCode::Up => list.sel = list.sel.saturating_sub(1),
        KeyCode::Char('d') if ctrl => list.sel = (list.sel + 10).min(max),
        KeyCode::Char('u') if ctrl => list.sel = list.sel.saturating_sub(10),
        KeyCode::Char('g') => {
            if pending_g {
                list.sel = 0;
            } else {
                *set_pending_g = true;
            }
        }
        KeyCode::Char('G') => list.sel = max,
        KeyCode::Esc => {
            list.filter.clear();
            list.sel = 0;
        }
        KeyCode::Char('q') => return Some(Action::Quit),
        KeyCode::Char('?') => *show_help = true,
        KeyCode::Enter => return None,
        _ => {}
    }
    Some(Action::None)
}

pub fn fmt_count(n: i64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    let digits: Vec<char> = s.chars().collect();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 && c.is_ascii_digit() {
            out.push(' ');
        }
        out.push(*c);
    }
    out
}

/// V-selected rows as a JSON array of objects; numbers/bools become real
/// JSON values when they parse cleanly.
fn rows_as_json(view: &QueryView, lo: usize, hi: usize) -> String {
    use serde_json::{Map, Value};
    let mut arr: Vec<Value> = vec![];
    for row in &view.rows[lo..=hi.min(view.rows.len() - 1)] {
        let mut obj = Map::new();
        for (i, col) in view.cols.iter().enumerate() {
            let cell = row.get(i).cloned().flatten();
            let value = match cell {
                None => Value::Null,
                Some(s) => match col.category {
                    Category::Num => s
                        .parse::<i64>()
                        .map(Value::from)
                        .or_else(|_| s.parse::<f64>().map(Value::from))
                        .unwrap_or(Value::String(s)),
                    Category::Bool => match s.as_str() {
                        "true" | "1" => Value::Bool(true),
                        "false" | "0" => Value::Bool(false),
                        _ => Value::String(s),
                    },
                    _ => Value::String(s),
                },
            };
            obj.insert(col.name.clone(), value);
        }
        arr.push(Value::Object(obj));
    }
    serde_json::to_string_pretty(&arr).unwrap_or_default()
}

/// V-selected rows as a markdown table (NULL shown as empty cells).
fn rows_as_markdown(view: &QueryView, lo: usize, hi: usize) -> String {
    let esc = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('|', "\\|")
            .replace(['\n', '\r'], " ")
    };
    let mut out = String::new();
    out.push_str("| ");
    out.push_str(
        &view
            .cols
            .iter()
            .map(|c| esc(&c.name))
            .collect::<Vec<_>>()
            .join(" | "),
    );
    out.push_str(" |\n| ");
    out.push_str(
        &view
            .cols
            .iter()
            .map(|_| "---")
            .collect::<Vec<_>>()
            .join(" | "),
    );
    out.push_str(" |\n");
    for row in &view.rows[lo..=hi.min(view.rows.len() - 1)] {
        out.push_str("| ");
        let cells: Vec<String> = view
            .cols
            .iter()
            .enumerate()
            .map(|(i, _)| {
                row.get(i)
                    .cloned()
                    .flatten()
                    .map(|s| esc(&s))
                    .unwrap_or_default()
            })
            .collect();
        out.push_str(&cells.join(" | "));
        out.push_str(" |\n");
    }
    out
}

fn compute_widths(cols: &[ColMeta], rows: &[Vec<Option<String>>]) -> Vec<u16> {
    use unicode_width::UnicodeWidthStr;
    cols.iter()
        .enumerate()
        .map(|(i, col)| {
            let mut data_w = 0usize;
            for row in rows.iter().take(500) {
                if let Some(cell) = row.get(i) {
                    let text = crate::ui::display_text(cell, col.category);
                    data_w = data_w.max(text.width());
                }
            }
            // Only the data is capped — the column name is always shown in full.
            data_w
                .clamp(4, 60)
                .max(col.name.width().min(200))
                .max(col.ty.width().min(200)) as u16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_with(rows: Vec<Vec<Option<&str>>>) -> QueryView {
        let cols = vec![
            ColMeta {
                name: "a".into(),
                ty: "text".into(),
                category: Category::Other,
            },
            ColMeta {
                name: "b".into(),
                ty: "text".into(),
                category: Category::Other,
            },
        ];
        let rows = rows
            .into_iter()
            .map(|r| r.into_iter().map(|c| c.map(str::to_string)).collect())
            .collect();
        QueryView {
            cols,
            rows,
            widths: vec![4, 4],
            sel_row: 0,
            sel_col: 0,
            row_off: 0,
            col_off: 0,
            vsel: None,
            error: None,
            search: String::new(),
            searching: false,
            search_hits: vec![],
        }
    }

    #[test]
    fn search_matches_are_case_insensitive_and_row_major() {
        let mut view = view_with(vec![
            vec![Some("Foo"), Some("bar")],
            vec![Some("baz"), Some("FOObar")],
        ]);
        view.search = "foo".into();
        // (0,0) "Foo" and (1,1) "FOObar", in row-major order.
        assert_eq!(view.matches(), vec![(0, 0), (1, 1)]);
    }

    #[test]
    fn search_ignores_null_and_empty_term() {
        let mut view = view_with(vec![vec![None, Some("hit")]]);
        assert!(view.matches().is_empty()); // no term yet
        view.search = "hit".into();
        assert_eq!(view.matches(), vec![(0, 1)]);
    }

    #[test]
    fn refresh_caches_hits_and_reports_current_index() {
        let mut view = view_with(vec![
            vec![Some("apple"), Some("pear")],
            vec![Some("grape"), Some("apple")],
        ]);
        view.search = "apple".into();
        view.refresh_search();
        assert_eq!(view.search_hits, vec![(0, 0), (1, 1)]);
        // Cursor starts at (0,0) — the first hit.
        assert_eq!(view.search_index(), Some(0));
        // Move onto the second hit.
        view.sel_row = 1;
        view.sel_col = 1;
        assert_eq!(view.search_index(), Some(1));
        // A cell that is not a match reports no index.
        view.sel_col = 0;
        assert_eq!(view.search_index(), None);
    }

    #[test]
    fn preview_query_uses_engine_dialect_and_escapes_identifiers() {
        assert_eq!(
            preview_query("mssql", "dbo", "weird]name"),
            "SELECT TOP 100 *\nFROM [dbo].[weird]]name]"
        );
        assert_eq!(
            preview_query("postgres", "public", "weird\"name"),
            "SELECT *\nFROM \"public\".\"weird\"\"name\"\nLIMIT 100"
        );
        assert_eq!(
            preview_query("sqlite", "main", "people"),
            "SELECT *\nFROM \"main\".\"people\"\nLIMIT 100"
        );
    }
}
