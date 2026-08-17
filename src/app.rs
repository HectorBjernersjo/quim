use std::collections::HashMap;
use std::sync::mpsc::Sender;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::clipboard;
use crate::config::{self, Config, ConnectionCfg, DbSelection};
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

/// One database quim can point a query at. Every connection contributes at
/// least one; a host contributes one per database it lists.
#[derive(Clone)]
pub struct DbEntry {
    /// "<connectionId>:<database>" — stable across restarts.
    pub id: String,
    /// What the sidebar row says: the database name, or the connection name
    /// when the connection is a single database.
    pub name: String,
    /// Name used while filtering, disambiguated when two connections share it.
    pub label: String,
    pub engine: String,
    pub conn_id: String,
    pub conn_name: String,
    pub conn: String,
    pub database: String,
    /// False for a database the host has but this connection hides.
    pub included: bool,
}

/// One line of the sidebar tree.
#[derive(Clone, Copy, PartialEq)]
pub enum SideRow {
    /// A host, by index into `cfg.connections`.
    Conn(usize),
    /// A database, by index into `App::dbs`.
    Db(usize),
}

pub fn entry_id(conn_id: &str, database: &str) -> String {
    format!("{conn_id}:{database}")
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
        let cell = self
            .rows
            .get(r)
            .and_then(|row| row.get(c))
            .cloned()
            .flatten();
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

// --- Connection editor -------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum FormField {
    Name,
    Conn,
    Engine,
    Scope,
    DbList,
    Save,
    Cancel,
}

/// What the connection should list.
#[derive(Clone, Copy, PartialEq)]
pub enum FormScope {
    /// Whatever the connection string implies — one database if it names one,
    /// otherwise the whole host. Also what leaves an existing pick alone.
    Auto,
    All,
    Pick,
}

/// Add/edit dialog for a connection. Name and connection string are the only
/// things typed in; the engine row shows what was derived and doubles as the
/// override when the string is too exotic to read. The scope row is where a
/// host's databases are picked from the full list.
#[derive(Clone)]
pub struct SourceForm {
    pub editing_id: Option<String>,
    pub name: String,
    pub conn: String,
    pub cursor: usize, // char index in the focused text field
    /// None = whatever the connection string looks like.
    pub engine_override: Option<String>,
    pub scope: FormScope,
    /// Every database the host reported, and whether it is picked. Fetched the
    /// first time the scope is set to Pick, so the list is always the live one
    /// rather than whatever happens to be in the sidebar.
    pub db_list: Vec<(String, bool)>,
    pub list_sel: usize,
    pub fetching: bool,
    pub fetched: bool,
    pub focus: FormField,
    pub saving: bool,
    pub error: Option<String>,
}

impl SourceForm {
    fn new_add() -> Self {
        SourceForm {
            editing_id: None,
            name: String::new(),
            conn: String::new(),
            cursor: 0,
            engine_override: None,
            scope: FormScope::Auto,
            db_list: vec![],
            list_sel: 0,
            fetching: false,
            fetched: false,
            focus: FormField::Name,
            saving: false,
            error: None,
        }
    }

    /// Visible fields, in tab order. Scope only means something for an engine
    /// that can enumerate databases.
    pub fn fields(&self) -> Vec<FormField> {
        let mut out = vec![FormField::Name, FormField::Conn, FormField::Engine];
        if config::supports_listing(&self.engine()) {
            out.push(FormField::Scope);
            if self.scope == FormScope::Pick && !self.db_list.is_empty() {
                out.push(FormField::DbList);
            }
        }
        out.push(FormField::Save);
        out.push(FormField::Cancel);
        out
    }

    pub fn picked(&self) -> Vec<String> {
        self.db_list
            .iter()
            .filter(|(_, on)| *on)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// The engine this connection will use: the override, else what the string
    /// looks like. Empty when neither says anything.
    pub fn engine(&self) -> String {
        match &self.engine_override {
            Some(engine) => engine.clone(),
            None => config::detect_engine(&self.conn).unwrap_or_default(),
        }
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

    /// Step through auto → mssql → postgres → sqlite → auto.
    fn cycle_engine(&mut self, delta: isize) {
        let len = config::ENGINES.len() as isize + 1;
        let cur = match &self.engine_override {
            None => 0,
            Some(engine) => config::ENGINES
                .iter()
                .position(|e| *e == engine)
                .map(|i| i as isize + 1)
                .unwrap_or(0),
        };
        let next = (cur + delta).rem_euclid(len);
        self.engine_override = if next == 0 {
            None
        } else {
            Some(config::ENGINES[(next - 1) as usize].to_string())
        };
    }

    /// Step through auto → all → pick → auto. Returns true when it landed on
    /// Pick without a list yet, so the caller can go and fetch one.
    fn cycle_scope(&mut self, delta: isize) -> bool {
        const ORDER: [FormScope; 3] = [FormScope::Auto, FormScope::All, FormScope::Pick];
        let cur = ORDER.iter().position(|s| *s == self.scope).unwrap_or(0) as isize;
        self.scope = ORDER[(cur + delta).rem_euclid(3) as usize];
        self.list_sel = 0;
        self.scope == FormScope::Pick && !self.fetched && !self.fetching
    }
}

/// What a y/n prompt will do. Deleting a connection throws away a connection
/// string; hiding a database only stops listing it — different weights, so the
/// prompt says which one it is.
#[derive(Clone)]
pub enum ConfirmAction {
    DeleteConnection(String),
    HideDatabase { conn_id: String, database: String },
}

impl ConfirmAction {
    pub fn is_delete(&self) -> bool {
        matches!(self, ConfirmAction::DeleteConnection(_))
    }
}

// --- Settings screen --------------------------------------------------------------

pub struct SettingsState {
    pub row: usize,
}

/// Rows on the settings screen, in display order.
pub const GENERAL_ROWS: usize = 2;

pub struct App {
    pub cfg: Config,
    req_tx: Sender<DbRequest>,

    pub dbs: Vec<DbEntry>,
    /// The sidebar tree: connections with their databases underneath.
    pub rows: Vec<SideRow>,
    pub db_list: FilterList,
    pub tbl_list: FilterList,
    /// Databases each connection reported, by connection id.
    fetched: HashMap<String, Vec<String>>,
    pub pending_lists: usize,

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
    /// (prompt, connection id) while a delete waits for y/n.
    pub confirm: Option<(String, ConfirmAction)>,
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
            rows: vec![],
            db_list: FilterList::default(),
            tbl_list: FilterList::default(),
            fetched: HashMap::new(),
            pending_lists: 0,
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
        if self.cfg.connections.is_empty() {
            self.set_status(
                "No connections — press a to add one".into(),
                StatusKind::Err,
            );
            return;
        }
        for c in self.cfg.connections.clone() {
            let engine = c.engine();
            if !config::supported_engine(&engine) {
                self.set_status(
                    format!("{}: could not tell which engine this is", c.name),
                    StatusKind::Err,
                );
                continue;
            }
            // Only a connection that lists everything needs a round-trip before
            // its databases are known; a narrowed one already names them.
            if c.databases.is_all() && config::supports_listing(&engine) {
                self.request_databases(&c);
            }
        }
        if self.pending_lists > 0 {
            self.set_status("Loading databases…".into(), StatusKind::Info);
        }
        self.rebuild_entries();
    }

    fn request_databases(&mut self, c: &ConnectionCfg) {
        self.pending_lists += 1;
        let _ = self.req_tx.send(DbRequest::ListDatabases {
            conn_id: c.id.clone(),
            engine: c.engine(),
            conn: c.connection_string.clone(),
        });
    }

    /// Rebuild both the flat list of reachable databases and the sidebar tree.
    /// `dbs` holds every database quim knows of — collapsed ones included, so
    /// the filter and the remembered selection still find them.
    fn rebuild_entries(&mut self) {
        let mut dbs: Vec<DbEntry> = vec![];
        let mut rows: Vec<SideRow> = vec![];
        for (ci, c) in self.cfg.connections.iter().enumerate() {
            let engine = c.engine();
            if !config::supported_engine(&engine) {
                // Still listed, so it can be fixed or deleted from the sidebar.
                rows.push(SideRow::Conn(ci));
                continue;
            }
            let push = |dbs: &mut Vec<DbEntry>, name: String, database: String, included| {
                dbs.push(DbEntry {
                    id: entry_id(&c.id, &database),
                    name,
                    label: String::new(),
                    engine: engine.clone(),
                    conn_id: c.id.clone(),
                    conn_name: c.name.clone(),
                    conn: c.connection_string.clone(),
                    database,
                    included,
                });
                dbs.len() - 1
            };

            if c.is_single_database() {
                let i = push(&mut dbs, c.name.clone(), c.single_database(), true);
                rows.push(SideRow::Db(i));
                continue;
            }

            rows.push(SideRow::Conn(ci));
            // What the host reported, plus anything the selection names in case
            // it was never listed (or the listing failed).
            let mut known = self.fetched.get(&c.id).cloned().unwrap_or_default();
            for name in c.databases.names() {
                if !known.contains(name) {
                    known.push(name.clone());
                }
            }
            known.sort_by_key(|name| name.to_lowercase());
            let expanded = !self.cfg.quim.collapsed.contains(&c.id);
            for name in known {
                let included = c.databases.includes(&name);
                let i = push(&mut dbs, name.clone(), name, included);
                if expanded {
                    rows.push(SideRow::Db(i));
                }
            }
        }
        // Only qualify a name when two connections both have it.
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for e in &dbs {
            *counts.entry(e.name.as_str()).or_default() += 1;
        }
        let labels: Vec<String> = dbs
            .iter()
            .map(|e| {
                if counts[e.name.as_str()] > 1 && e.name != e.conn_name {
                    format!("{} ({})", e.name, e.conn_name)
                } else {
                    e.name.clone()
                }
            })
            .collect();
        for (e, label) in dbs.iter_mut().zip(labels) {
            e.label = label;
        }
        self.dbs = dbs;
        self.rows = rows;

        if let Some(active) = &self.active {
            if !self.dbs.iter().any(|d| d.id == active.id) {
                self.active = None;
            }
        }
        if self.active.is_none() {
            // Prefer the persisted selection; while database lists are still
            // loading, hold off on the first-entry fallback so the remembered
            // database wins once its connection reports in.
            let remembered = self
                .cfg
                .quim
                .active_db
                .as_ref()
                .and_then(|id| self.dbs.iter().find(|d| &d.id == id))
                .cloned();
            if let Some(entry) = remembered {
                self.select_db(entry);
            } else if self.pending_lists == 0 {
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
        // Park the cursor on it, so the ● is on screen after a restart even
        // when the remembered database sits far down the tree.
        if let Some(i) = self.dbs.iter().position(|d| d.id == entry.id) {
            if let Some(row) = self
                .visible_rows()
                .iter()
                .position(|r| *r == SideRow::Db(i))
            {
                self.db_list.sel = row;
            }
        }
        self.active = Some(entry);
    }

    pub fn set_status(&mut self, text: String, kind: StatusKind) {
        self.status = (text, kind);
    }

    // --- Filtered views --------------------------------------------------------

    /// The sidebar rows to draw. Filtering drops the tree and jumps straight to
    /// the matching databases — the fast way to reach one by name.
    pub fn visible_rows(&self) -> Vec<SideRow> {
        let f = self.db_list.filter.to_lowercase();
        if f.is_empty() {
            return self.rows.clone();
        }
        let mut hits: Vec<usize> = self
            .dbs
            .iter()
            .enumerate()
            .filter(|(_, d)| d.included && d.label.to_lowercase().contains(&f))
            .map(|(i, _)| i)
            .collect();
        hits.sort_by_key(|&i| self.dbs[i].label.to_lowercase());
        hits.into_iter().map(SideRow::Db).collect()
    }

    /// The row under the cursor.
    pub fn selected_row(&self) -> Option<SideRow> {
        self.visible_rows().get(self.db_list.sel).copied()
    }

    /// The connection the cursor is on, or the one owning the database it is on.
    fn selected_conn(&self) -> Option<usize> {
        match self.selected_row()? {
            SideRow::Conn(ci) => Some(ci),
            SideRow::Db(i) => {
                let id = &self.dbs.get(i)?.conn_id;
                self.cfg.connections.iter().position(|c| &c.id == id)
            }
        }
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
            DbResponse::Databases { conn_id, result } => {
                if conn_id.starts_with("@form:") {
                    self.on_form_fetch(conn_id, result);
                    return;
                }
                self.pending_lists = self.pending_lists.saturating_sub(1);
                match result {
                    Ok(names) => {
                        self.fetched.insert(conn_id, names);
                        if self.pending_lists == 0 && matches!(self.status.1, StatusKind::Info) {
                            self.set_status(String::new(), StatusKind::Info);
                        }
                    }
                    Err(e) => {
                        let name = self
                            .cfg
                            .connections
                            .iter()
                            .find(|c| c.id == conn_id)
                            .map(|c| c.name.clone())
                            .unwrap_or(conn_id);
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
        self.settings = Some(SettingsState { row: 0 });
    }

    fn settings_key(&mut self, key: KeyEvent) -> Action {
        let Some(st) = self.settings.as_mut() else {
            return Action::None;
        };
        match key.code {
            KeyCode::Char('q') | KeyCode::Char(',') | KeyCode::Esc => self.settings = None,
            KeyCode::Char('?') | KeyCode::F(1) => self.show_help = true,
            KeyCode::Char('j') | KeyCode::Down => st.row = (st.row + 1).min(GENERAL_ROWS - 1),
            KeyCode::Char('k') | KeyCode::Up => st.row = st.row.saturating_sub(1),
            KeyCode::Char(' ') | KeyCode::Enter => match st.row {
                0 => self.toggle_vim_mode(),
                _ => self.reset_layout(),
            },
            _ => {}
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

    // --- Managing connections ------------------------------------------------------

    fn save_cfg(&mut self) {
        if let Err(e) = config::save(&self.cfg) {
            self.set_status(format!("✕ {e}"), StatusKind::Err);
        }
    }

    /// Fold a connection open or shut. Purely a display change — the databases
    /// underneath are already known either way.
    fn toggle_expand(&mut self, ci: usize) {
        let Some(c) = self.cfg.connections.get(ci) else {
            return;
        };
        let id = c.id.clone();
        if self.cfg.quim.collapsed.contains(&id) {
            self.cfg.quim.collapsed.retain(|other| other != &id);
        } else {
            self.cfg.quim.collapsed.push(id);
        }
        self.save_cfg();
        self.rebuild_entries();
    }

    /// Space on a database: show it under its connection, or stop showing it.
    fn toggle_included(&mut self, entry_idx: usize) {
        let Some(entry) = self.dbs.get(entry_idx).cloned() else {
            return;
        };
        let Some(ci) = self
            .cfg
            .connections
            .iter()
            .position(|c| c.id == entry.conn_id)
        else {
            return;
        };
        if self.cfg.connections[ci].is_single_database() {
            self.set_status(
                format!("{} is a single database", entry.conn_name),
                StatusKind::Info,
            );
            return;
        }
        // Everything the connection currently shows, whether picked or not.
        let known: Vec<String> = self
            .dbs
            .iter()
            .filter(|d| d.conn_id == entry.conn_id)
            .map(|d| d.database.clone())
            .collect();
        let c = &mut self.cfg.connections[ci];
        let mut names: Vec<String> = match &c.databases {
            DbSelection::All(_) => known.clone(),
            DbSelection::Named(picked) => picked.clone(),
        };
        if names.contains(&entry.database) {
            names.retain(|name| name != &entry.database);
        } else {
            names.push(entry.database.clone());
            names.sort_by_key(|name| name.to_lowercase());
        }
        c.databases = if names.len() == known.len() {
            DbSelection::all()
        } else {
            DbSelection::Named(names)
        };
        self.save_cfg();
        self.rebuild_entries();
    }

    /// r on a connection: ask the host what databases it has and show them all.
    /// Also the way back after hiding too many.
    fn discover_databases(&mut self) {
        let Some(ci) = self.selected_conn() else {
            return;
        };
        let c = self.cfg.connections[ci].clone();
        if !config::supports_listing(&c.engine()) {
            self.set_status(
                format!("{} is a single database file", c.name),
                StatusKind::Info,
            );
            return;
        }
        self.cfg.connections[ci].databases = DbSelection::all();
        self.cfg.quim.collapsed.retain(|id| id != &c.id);
        self.save_cfg();
        self.fetched.remove(&c.id);
        self.request_databases(&c);
        self.set_status(
            format!("Listing databases on {}…", c.name),
            StatusKind::Info,
        );
        self.rebuild_entries();
    }

    fn test_selected(&mut self) {
        let Some(ci) = self.selected_conn() else {
            return;
        };
        let c = &self.cfg.connections[ci];
        let engine = c.engine();
        if !config::supported_engine(&engine) {
            self.set_status(
                format!("✕ {}: unknown engine — set one with e", c.name),
                StatusKind::Err,
            );
            return;
        }
        // Test the database under the cursor when there is one, so the check
        // covers the same thing a query would.
        let database = match self.selected_row() {
            Some(SideRow::Db(i)) => self.dbs[i].database.clone(),
            _ => config::probe_database(&engine, &c.connection_string),
        };
        let (name, conn) = (c.name.clone(), c.connection_string.clone());
        self.set_status(format!("Testing {name}…"), StatusKind::Info);
        let _ = self.req_tx.send(DbRequest::TestConnection {
            token: format!("@ping:{name}"),
            engine,
            conn,
            database,
        });
    }

    // --- Connection form -------------------------------------------------------------

    fn open_add_form(&mut self) {
        self.form_token += 1;
        self.form = Some(SourceForm::new_add());
    }

    fn open_edit_form(&mut self) {
        let Some(ci) = self.selected_conn() else {
            return;
        };
        let c = self.cfg.connections[ci].clone();
        let engine = c.engine();
        // Show the scope the connection actually has. Anything the string
        // already implies stays on auto, so a rename saves as a no-op.
        let scope = if c.databases == config::selection_for(&engine, &c.connection_string) {
            FormScope::Auto
        } else if c.databases.is_all() {
            FormScope::All
        } else {
            FormScope::Pick
        };
        self.form_token += 1;
        self.form = Some(SourceForm {
            editing_id: Some(c.id),
            cursor: c.name.chars().count(),
            name: c.name,
            conn: c.connection_string,
            engine_override: c.engine,
            scope,
            db_list: c
                .databases
                .names()
                .iter()
                .map(|name| (name.clone(), true))
                .collect(),
            ..SourceForm::new_add()
        });
        // Opening on Pick means the saved list is a subset — go and get the
        // rest, so the ones hidden earlier can be ticked back on.
        if scope == FormScope::Pick {
            self.form_fetch();
        }
    }

    /// d asks about whatever the cursor is on: on a database that means hiding
    /// it, on a connection it means throwing the connection string away.
    fn open_confirm_delete(&mut self) {
        if let Some(SideRow::Db(i)) = self.selected_row() {
            let Some(entry) = self.dbs.get(i).cloned() else {
                return;
            };
            let one_row = self
                .cfg
                .connections
                .iter()
                .find(|c| c.id == entry.conn_id)
                .is_some_and(|c| c.is_single_database());
            // A connection that is one database has nothing to hide — d there
            // means the connection itself.
            if !one_row {
                if !entry.included {
                    self.set_status(
                        format!("{} is already hidden — Enter shows it again", entry.name),
                        StatusKind::Info,
                    );
                    return;
                }
                self.confirm = Some((
                    format!(
                        "Hide \"{}\" from {}? (y/n)",
                        entry.database, entry.conn_name
                    ),
                    ConfirmAction::HideDatabase {
                        conn_id: entry.conn_id,
                        database: entry.database,
                    },
                ));
                return;
            }
        }
        let Some(ci) = self.selected_conn() else {
            return;
        };
        let c = &self.cfg.connections[ci];
        let note = if c.is_single_database() {
            ""
        } else {
            " and all its databases"
        };
        self.confirm = Some((
            format!("Delete connection \"{}\"{note}? (y/n)", c.name),
            ConfirmAction::DeleteConnection(c.id.clone()),
        ));
    }

    fn confirm_key(&mut self, key: KeyEvent) -> Action {
        let Some((_, action)) = self.confirm.take() else {
            return Action::None;
        };
        if !matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            return Action::None;
        }
        match action {
            ConfirmAction::DeleteConnection(id) => {
                self.cfg.connections.retain(|c| c.id != id);
                self.cfg.quim.collapsed.retain(|other| other != &id);
                self.fetched.remove(&id);
                self.save_cfg();
                self.set_status("Connection deleted".into(), StatusKind::Ok);
                self.rebuild_entries();
            }
            ConfirmAction::HideDatabase { conn_id, database } => {
                let found = self
                    .dbs
                    .iter()
                    .position(|d| d.conn_id == conn_id && d.database == database);
                if let Some(i) = found {
                    self.toggle_included(i);
                    self.set_status(format!("{database} hidden"), StatusKind::Ok);
                }
            }
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
            // Enter keeps meaning "next field" here — the engine is normally
            // left on auto, so Space/h/l are what change it.
            FormField::Engine => match key.code {
                KeyCode::Char('l') | KeyCode::Char(' ') | KeyCode::Right => form.cycle_engine(1),
                KeyCode::Char('h') | KeyCode::Left => form.cycle_engine(-1),
                KeyCode::Enter => form.move_focus(1),
                KeyCode::Up | KeyCode::Char('k') => form.move_focus(-1),
                KeyCode::Down | KeyCode::Char('j') => form.move_focus(1),
                _ => {}
            },
            FormField::Scope => match key.code {
                KeyCode::Char('l') | KeyCode::Char(' ') | KeyCode::Right => {
                    if form.cycle_scope(1) {
                        self.form_fetch();
                    }
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    if form.cycle_scope(-1) {
                        self.form_fetch();
                    }
                }
                KeyCode::Enter => form.move_focus(1),
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
                // a flips the whole list, for "none of these except…".
                KeyCode::Char('a') => {
                    let all_on = form.db_list.iter().all(|(_, on)| *on);
                    for item in &mut form.db_list {
                        item.1 = !all_on;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    form.list_sel = (form.list_sel + 1).min(form.db_list.len().saturating_sub(1));
                }
                KeyCode::Up | KeyCode::Char('k') if form.list_sel > 0 => {
                    form.list_sel -= 1;
                }
                // Off the top of the list steps back to the scope row.
                KeyCode::Up | KeyCode::Char('k') => form.move_focus(-1),
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

    /// Ask the host what databases it has, so the picker shows the live list
    /// rather than only the ones already saved.
    fn form_fetch(&mut self) {
        let token = self.form_token;
        let Some(form) = self.form.as_mut() else {
            return;
        };
        let engine = form.engine();
        if form.conn.trim().is_empty() {
            form.error = Some("Enter a connection string first".into());
            return;
        }
        if !config::supports_listing(&engine) {
            return;
        }
        form.error = None;
        form.fetching = true;
        let _ = self.req_tx.send(DbRequest::ListDatabases {
            conn_id: format!("@form:{token}"),
            engine,
            conn: form.conn.trim().to_string(),
        });
    }

    fn on_form_fetch(&mut self, conn_id: String, result: Result<Vec<String>, String>) {
        if conn_id != format!("@form:{}", self.form_token) {
            return; // stale response from a form that has since closed
        }
        let Some(form) = self.form.as_mut() else {
            return;
        };
        form.fetching = false;
        match result {
            Ok(names) => {
                // Keep the ticks already made; if nothing was picked yet, the
                // whole host is the sensible starting point.
                let picked: HashMap<String, bool> = form.db_list.drain(..).collect();
                let start_on = picked.is_empty();
                form.db_list = names
                    .into_iter()
                    .map(|name| {
                        let on = picked.get(&name).copied().unwrap_or(start_on);
                        (name, on)
                    })
                    .collect();
                form.list_sel = 0;
                form.fetched = true;
                form.error = None;
            }
            Err(e) => {
                form.error = Some(e);
                form.scope = FormScope::Auto;
            }
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
        let engine = form.engine();
        if !config::supported_engine(&engine) {
            form.error = Some("Could not tell which engine this is — pick one".into());
            form.focus = FormField::Engine;
            return;
        }
        if form.scope == FormScope::Pick && form.picked().is_empty() {
            form.error = Some("Tick at least one database, or switch to all".into());
            form.focus = FormField::Scope;
            return;
        }
        form.error = None;
        form.saving = true;
        let database = config::probe_database(&engine, &form.conn);
        let _ = self.req_tx.send(DbRequest::TestConnection {
            token: format!("@save:{token}"),
            engine,
            conn: form.conn.clone(),
            database,
        });
    }

    fn on_test_result(&mut self, token: String, result: Result<(), String>) {
        // Standalone test of the connection under the cursor (t in the sidebar).
        if let Some(name) = token.strip_prefix("@ping:") {
            match result {
                Ok(()) => {
                    self.set_status(format!("\u{2713} {name}: connection ok"), StatusKind::Ok)
                }
                Err(e) => self.set_status(format!("\u{2715} {name}: {e}"), StatusKind::Err),
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
        let id = form
            .editing_id
            .clone()
            .unwrap_or_else(|| config::new_id("conn"));
        let engine = form.engine();
        let existing = self.cfg.connections.iter().position(|c| c.id == id);
        let extra = existing
            .map(|i| self.cfg.connections[i].extra.clone())
            .unwrap_or_default();
        let databases = match form.scope {
            FormScope::All => DbSelection::all(),
            FormScope::Pick => DbSelection::Named(form.picked()),
            // Auto follows the connection string — except on an edit that left
            // the string alone, where it means "don't touch what I picked in
            // the sidebar".
            FormScope::Auto => match existing.map(|i| &self.cfg.connections[i]) {
                Some(c) if c.connection_string == form.conn => c.databases.clone(),
                _ => config::selection_for(&engine, &form.conn),
            },
        };
        let entry = ConnectionCfg {
            id: id.clone(),
            name: form.name.clone(),
            engine: form.engine_override.clone(),
            connection_string: form.conn.clone(),
            databases,
            extra,
        };
        let lists_everything = entry.databases.is_all() && config::supports_listing(&engine);
        match existing {
            Some(i) => self.cfg.connections[i] = entry.clone(),
            None => self.cfg.connections.push(entry.clone()),
        }
        if let Err(e) = config::save(&self.cfg) {
            if let Some(f) = self.form.as_mut() {
                f.error = Some(e);
            }
            return;
        }
        self.form = None;
        if lists_everything {
            self.fetched.remove(&id);
            self.request_databases(&entry);
        }
        self.set_status(format!("Saved {}", form.name), StatusKind::Ok);
        self.rebuild_entries();
    }

    // --- Pane keys -------------------------------------------------------------

    /// The sidebar is where connections are managed: a/e/d/t act on the
    /// connection under the cursor, h/l fold it, Space picks databases.
    fn db_list_key(&mut self, key: KeyEvent, pending_g: bool) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if !self.db_list.typing && !ctrl {
            match key.code {
                KeyCode::Char('a') => {
                    self.open_add_form();
                    return Action::None;
                }
                KeyCode::Char('e') => {
                    self.open_edit_form();
                    return Action::None;
                }
                KeyCode::Char('d') => {
                    self.open_confirm_delete();
                    return Action::None;
                }
                KeyCode::Char('t') => {
                    self.test_selected();
                    return Action::None;
                }
                KeyCode::Char('r') => {
                    self.discover_databases();
                    return Action::None;
                }
                KeyCode::Char(' ') => {
                    match self.selected_row() {
                        Some(SideRow::Conn(ci)) => self.toggle_expand(ci),
                        Some(SideRow::Db(i)) => self.toggle_included(i),
                        None => {}
                    }
                    return Action::None;
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    if let Some(SideRow::Conn(ci)) = self.selected_row() {
                        if self.is_collapsed(ci) {
                            self.toggle_expand(ci);
                        }
                    }
                    return Action::None;
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    self.collapse_or_go_to_parent();
                    return Action::None;
                }
                _ => {}
            }
        }
        let len = self.visible_rows().len();
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
            match self.selected_row() {
                Some(SideRow::Conn(ci)) => self.toggle_expand(ci),
                Some(SideRow::Db(i)) => self.activate(i),
                None => {}
            }
        }
        Action::None
    }

    fn is_collapsed(&self, ci: usize) -> bool {
        self.cfg
            .connections
            .get(ci)
            .is_some_and(|c| self.cfg.quim.collapsed.contains(&c.id))
    }

    /// h folds the connection shut; on one of its databases it jumps up to the
    /// connection first, the way a file tree does.
    fn collapse_or_go_to_parent(&mut self) {
        match self.selected_row() {
            Some(SideRow::Conn(ci)) => {
                if !self.is_collapsed(ci) {
                    self.toggle_expand(ci);
                }
            }
            Some(SideRow::Db(i)) => {
                let Some(conn_id) = self.dbs.get(i).map(|d| d.conn_id.clone()) else {
                    return;
                };
                let parent = self.visible_rows().iter().position(|row| {
                    matches!(row, SideRow::Conn(ci)
                        if self.cfg.connections[*ci].id == conn_id)
                });
                if let Some(sel) = parent {
                    self.db_list.sel = sel;
                }
            }
            None => {}
        }
    }

    /// Point queries at this database. Picking one the connection hides brings
    /// it back rather than doing nothing.
    fn activate(&mut self, entry_idx: usize) {
        let Some(entry) = self.dbs.get(entry_idx).cloned() else {
            return;
        };
        if !entry.included {
            self.toggle_included(entry_idx);
        }
        if self.active.as_ref().map(|a| a.id.as_str()) != Some(entry.id.as_str()) {
            self.select_db(entry);
        }
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
    use std::sync::mpsc::Receiver;

    type ScratchConfig = std::sync::MutexGuard<'static, ()>;

    fn connection(id: &str, name: &str, conn: &str) -> ConnectionCfg {
        ConnectionCfg {
            id: id.into(),
            name: name.into(),
            engine: None,
            databases: config::selection_for(
                &config::detect_engine(conn).unwrap_or_default(),
                conn,
            ),
            connection_string: conn.into(),
            extra: Default::default(),
        }
    }

    /// An app with one MSSQL host (which has reported three databases), one
    /// Postgres connection pointing at a single database, and one SQLite file.
    /// The third element is the scratch-config guard — keep it alive.
    fn app_with_tree() -> (App, Receiver<DbRequest>, ScratchConfig) {
        let scratch = config::use_scratch_config_dir();
        let (tx, rx) = std::sync::mpsc::channel();
        let cfg = Config {
            connections: vec![
                connection(
                    "conn_sql",
                    "Local server",
                    "Server=localhost,1433;User Id=sa",
                ),
                connection(
                    "conn_pg",
                    "platform",
                    "postgres://u:p@127.0.0.1:15433/gbandit",
                ),
                connection("conn_lite", "Cursor sqlite", "/tmp/store.db"),
            ],
            ..Default::default()
        };
        let mut app = App::new(cfg, tx);
        app.on_db_response(DbResponse::Databases {
            conn_id: "conn_sql".into(),
            result: Ok(vec!["master".into(), "AppDb".into(), "Reporting".into()]),
        });
        (app, rx, scratch)
    }

    /// The tree as "kind:label" lines, which is what the sidebar draws.
    fn rendered(app: &App) -> Vec<String> {
        app.visible_rows()
            .iter()
            .map(|row| match row {
                SideRow::Conn(ci) => format!("conn:{}", app.cfg.connections[*ci].name),
                SideRow::Db(i) => format!(
                    "db:{}{}",
                    app.dbs[*i].name,
                    if app.dbs[*i].included {
                        ""
                    } else {
                        " (hidden)"
                    }
                ),
            })
            .collect()
    }

    #[test]
    fn a_host_gets_children_and_a_single_database_gets_one_row() {
        let (app, _rx, _cfg) = app_with_tree();
        assert_eq!(
            rendered(&app),
            [
                "conn:Local server",
                "db:AppDb",
                "db:master",
                "db:Reporting",
                // Both of these name one database, so they are a row each
                // rather than a connection to unfold.
                "db:platform",
                "db:Cursor sqlite",
            ]
        );
    }

    #[test]
    fn collapsing_hides_the_children_but_keeps_them_selectable() {
        let (mut app, _rx, _cfg) = app_with_tree();
        app.toggle_expand(0);
        assert_eq!(
            rendered(&app),
            ["conn:Local server", "db:platform", "db:Cursor sqlite"]
        );
        // Still known, so the filter and the remembered selection find them.
        assert!(app.dbs.iter().any(|d| d.name == "Reporting"));

        app.toggle_expand(0);
        assert_eq!(rendered(&app).len(), 6);
    }

    #[test]
    fn filtering_flattens_the_tree_to_the_matches() {
        let (mut app, _rx, _cfg) = app_with_tree();
        app.db_list.filter = "re".into();
        assert_eq!(rendered(&app), ["db:Reporting"]);
        app.db_list.filter.clear();
        assert_eq!(rendered(&app).len(), 6);
    }

    #[test]
    fn space_stops_listing_a_database_and_enter_brings_it_back() {
        let (mut app, _rx, _cfg) = app_with_tree();
        let reporting = app.dbs.iter().position(|d| d.name == "Reporting").unwrap();
        app.toggle_included(reporting);

        assert_eq!(
            app.cfg.connections[0].databases.names(),
            ["AppDb", "master"]
        );
        // It stays on screen, dimmed, so it can be picked straight back up.
        assert!(rendered(&app).contains(&"db:Reporting (hidden)".to_string()));
        // ...but it is out of the filter, which only offers what is listed.
        app.db_list.filter = "report".into();
        assert!(rendered(&app).is_empty());
        app.db_list.filter.clear();

        let reporting = app.dbs.iter().position(|d| d.name == "Reporting").unwrap();
        app.activate(reporting);
        assert!(app.cfg.connections[0].databases.is_all());
        assert_eq!(
            app.active.as_ref().map(|a| a.database.as_str()),
            Some("Reporting")
        );
    }

    #[test]
    fn discover_widens_a_single_database_connection_to_its_host() {
        let (mut app, rx, _cfg) = app_with_tree();
        while rx.try_recv().is_ok() {} // drain the startup listing
        let platform = app.dbs.iter().position(|d| d.name == "platform").unwrap();
        let row = app
            .visible_rows()
            .iter()
            .position(|r| *r == SideRow::Db(platform))
            .unwrap();
        app.db_list.sel = row;
        app.discover_databases();

        assert!(app.cfg.connections[1].databases.is_all());
        assert!(matches!(
            rx.try_recv(),
            Ok(DbRequest::ListDatabases { conn_id, .. }) if conn_id == "conn_pg"
        ));
        app.on_db_response(DbResponse::Databases {
            conn_id: "conn_pg".into(),
            result: Ok(vec!["gbandit".into(), "postgres".into()]),
        });
        // No longer a leaf: it unfolds into the databases on that host.
        let rows = rendered(&app);
        assert!(rows.contains(&"conn:platform".to_string()));
        assert!(rows.contains(&"db:postgres".to_string()));
    }

    /// Hiding a host down to its last database must not turn it into a leaf —
    /// the hidden ones would go with it and there would be no way back short
    /// of r.
    #[test]
    fn hiding_all_but_one_database_keeps_the_host_a_host() {
        let (mut app, _rx, _cfg) = app_with_tree();
        for name in ["Reporting", "master"] {
            let i = app.dbs.iter().position(|d| d.name == name).unwrap();
            app.toggle_included(i);
        }
        assert_eq!(app.cfg.connections[0].databases.names(), ["AppDb"]);
        assert_eq!(
            rendered(&app),
            [
                "conn:Local server",
                "db:AppDb",
                "db:master (hidden)",
                "db:Reporting (hidden)",
                "db:platform",
                "db:Cursor sqlite",
            ]
        );
    }

    /// d is delete on a connection but only hide on one of its databases —
    /// the two sit next to each other, so they must not do the same thing.
    #[test]
    fn d_on_a_database_offers_to_hide_it_not_to_delete_the_connection() {
        let (mut app, _rx, _cfg) = app_with_tree();
        let master = app.dbs.iter().position(|d| d.name == "master").unwrap();
        app.db_list.sel = app
            .visible_rows()
            .iter()
            .position(|r| *r == SideRow::Db(master))
            .unwrap();

        app.open_confirm_delete();
        let (prompt, action) = app.confirm.clone().unwrap();
        assert!(prompt.contains("Hide \"master\" from Local server"));
        assert!(!action.is_delete());

        app.confirm_key(KeyEvent::from(KeyCode::Char('y')));
        assert_eq!(app.cfg.connections.len(), 3, "connection must survive");
        assert_eq!(
            app.cfg.connections[0].databases.names(),
            ["AppDb", "Reporting"]
        );

        // On the connection row it still means the connection.
        app.db_list.sel = 0;
        app.open_confirm_delete();
        let (prompt, action) = app.confirm.clone().unwrap();
        assert!(prompt.contains("Delete connection \"Local server\""));
        assert!(action.is_delete());
        app.confirm_key(KeyEvent::from(KeyCode::Char('y')));
        assert_eq!(app.cfg.connections.len(), 2);
    }

    #[test]
    fn n_at_the_prompt_leaves_everything_alone() {
        let (mut app, _rx, _cfg) = app_with_tree();
        app.db_list.sel = 0;
        app.open_confirm_delete();
        app.confirm_key(KeyEvent::from(KeyCode::Char('n')));
        assert!(app.confirm.is_none());
        assert_eq!(app.cfg.connections.len(), 3);
    }

    /// The picker is the durable way to change the set: it fetches the live
    /// list, so databases hidden in an earlier session are ticked back on here.
    #[test]
    fn editing_a_narrowed_connection_opens_the_picker_on_the_full_list() {
        let (mut app, rx, _cfg) = app_with_tree();
        while rx.try_recv().is_ok() {}
        let master = app.dbs.iter().position(|d| d.name == "master").unwrap();
        app.toggle_included(master);

        app.db_list.sel = 0; // the connection row
        app.open_edit_form();
        let form = app.form.as_ref().unwrap();
        assert!(form.scope == FormScope::Pick);
        assert!(form.fetching);
        // Only the saved names are known until the host answers.
        assert_eq!(
            form.db_list,
            [("AppDb".into(), true), ("Reporting".into(), true)]
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(DbRequest::ListDatabases { conn_id, .. }) if conn_id.starts_with("@form:")
        ));

        app.on_db_response(DbResponse::Databases {
            conn_id: format!("@form:{}", app.form_token),
            result: Ok(vec!["AppDb".into(), "master".into(), "Reporting".into()]),
        });
        let form = app.form.as_mut().unwrap();
        assert!(!form.fetching);
        // The hidden one is back on the list, unticked and ready to tick.
        assert_eq!(
            form.db_list,
            [
                ("AppDb".into(), true),
                ("master".into(), false),
                ("Reporting".into(), true)
            ]
        );

        form.db_list[1].1 = true;
        app.form_save();
        app.on_test_result(format!("@save:{}", app.form_token), Ok(()));
        assert_eq!(
            app.cfg.connections[0].databases.names(),
            ["AppDb", "master", "Reporting"]
        );
    }

    #[test]
    fn the_picker_refuses_to_save_an_empty_selection() {
        let (mut app, _rx, _cfg) = app_with_tree();
        app.db_list.sel = 0;
        app.open_edit_form();
        let form = app.form.as_mut().unwrap();
        form.scope = FormScope::Pick;
        form.db_list = vec![("AppDb".into(), false)];
        app.form_save();
        let form = app.form.as_ref().expect("form stays open");
        assert!(form.error.as_deref().unwrap().contains("at least one"));
        assert!(!form.saving);
    }

    /// Typing a name and a string is the whole form; everything else follows.
    #[test]
    fn adding_a_connection_derives_its_engine_and_scope() {
        let (mut app, rx, _cfg) = app_with_tree();
        while rx.try_recv().is_ok() {}
        app.open_add_form();
        let form = app.form.as_mut().unwrap();
        form.name = "warehouse".into();
        form.conn = "postgres://u:p@dw:5432/".into();
        app.form_save();
        assert!(matches!(
            rx.try_recv(),
            Ok(DbRequest::TestConnection { engine, .. }) if engine == "postgres"
        ));
        app.on_test_result(format!("@save:{}", app.form_token), Ok(()));

        assert!(app.form.is_none());
        let added = app.cfg.connections.last().unwrap();
        assert_eq!(added.name, "warehouse");
        assert_eq!(added.engine, None); // derived, so nothing to write down
                                        // No database in the URL, so it is a host and gets listed.
        assert!(added.databases.is_all());
        assert!(matches!(
            rx.try_recv(),
            Ok(DbRequest::ListDatabases { conn_id, .. }) if conn_id == added.id
        ));
    }

    #[test]
    fn editing_from_a_child_row_edits_its_connection() {
        let (mut app, _rx, _cfg) = app_with_tree();
        let appdb = app.dbs.iter().position(|d| d.name == "AppDb").unwrap();
        app.db_list.sel = app
            .visible_rows()
            .iter()
            .position(|r| *r == SideRow::Db(appdb))
            .unwrap();
        app.open_edit_form();
        let form = app.form.as_ref().unwrap();
        assert_eq!(form.editing_id.as_deref(), Some("conn_sql"));
        assert_eq!(form.name, "Local server");
    }

    /// Renaming must not throw away databases picked in the sidebar.
    #[test]
    fn editing_without_touching_the_string_keeps_the_picked_databases() {
        let (mut app, _rx, _cfg) = app_with_tree();
        let reporting = app.dbs.iter().position(|d| d.name == "Reporting").unwrap();
        app.toggle_included(reporting);
        let picked = app.cfg.connections[0].databases.names().to_vec();

        app.db_list.sel = 0; // the connection row
        app.open_edit_form();
        app.form.as_mut().unwrap().name = "prod-sql".into();
        app.form_save();
        app.on_test_result(format!("@save:{}", app.form_token), Ok(()));

        assert_eq!(app.cfg.connections[0].name, "prod-sql");
        assert_eq!(app.cfg.connections[0].databases.names(), picked.as_slice());
    }

    #[test]
    fn sqlite_has_nothing_to_discover() {
        let (mut app, _rx, _cfg) = app_with_tree();
        let lite = app
            .dbs
            .iter()
            .position(|d| d.name == "Cursor sqlite")
            .unwrap();
        let row = app
            .visible_rows()
            .iter()
            .position(|r| *r == SideRow::Db(lite))
            .unwrap();
        app.db_list.sel = row;
        app.discover_databases();
        assert!(app.cfg.connections[2].databases.names() == ["main"]);
        assert_eq!(rendered(&app).len(), 6);
    }

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
