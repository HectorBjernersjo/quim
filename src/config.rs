// Reads and writes quim's config: ~/.config/quim/config.json (XDG),
// ~/Library/Application Support/quim on macOS, %APPDATA%\quim on Windows.
// quim's own settings (vim mode, pane layout) live under the "quim" key;
// unknown keys are preserved on save.
//
// A connection is the only kind of source there is: a name plus a connection
// string. The engine and whether the string points at one database or at a
// whole host are both derived from the string itself — see `detect_engine`.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub connections: Vec<ConnectionCfg>,
    #[serde(default)]
    pub quim: QuimCfg,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionCfg {
    pub id: String,
    pub name: String,
    /// Only written when the user overrode the detected engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    pub connection_string: String,
    /// Which of the host's databases to list: "all", or the picked names.
    #[serde(default = "default_selection")]
    pub databases: DbSelection,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl ConnectionCfg {
    /// The engine to talk to: the explicit override, else what the connection
    /// string looks like. Empty when neither is available.
    pub fn engine(&self) -> String {
        match &self.engine {
            Some(e) => normalize_engine(e),
            None => detect_engine(&self.connection_string).unwrap_or_default(),
        }
    }

    /// True when this connection is one database and nothing else — a SQLite
    /// file, or a string that names the single database being listed. Those
    /// render as one row instead of a host with children.
    ///
    /// Hiding a host's databases down to one does not make it one of these:
    /// what the string says is what decides, so the shape only changes when
    /// the string does (or when `r` widens it back to the whole host).
    pub fn is_single_database(&self) -> bool {
        let engine = self.engine();
        if !supports_listing(&engine) {
            return true;
        }
        match (
            &self.databases,
            database_of_engine(&engine, &self.connection_string),
        ) {
            (DbSelection::Named(names), Some(named)) => names == &[named],
            _ => false,
        }
    }

    /// The database to talk to for a single-database connection.
    pub fn single_database(&self) -> String {
        if let DbSelection::Named(names) = &self.databases {
            if names.len() == 1 {
                return names[0].clone();
            }
        }
        database_of_engine(&self.engine(), &self.connection_string).unwrap_or_default()
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(untagged)]
pub enum DbSelection {
    Named(Vec<String>),
    All(String), // the literal string "all"
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct QuimCfg {
    pub vim_mode: bool,
    pub layout: LayoutCfg,
    /// Entry id ("<connectionId>:<database>") re-selected on startup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_db: Option<String>,
    /// Connections folded shut in the sidebar. Expanded is the default, so an
    /// empty list means everything is open.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub collapsed: Vec<String>,
}

impl Default for QuimCfg {
    fn default() -> Self {
        QuimCfg {
            vim_mode: true,
            layout: LayoutCfg::default(),
            active_db: None,
            collapsed: vec![],
        }
    }
}

/// Pane sizes in terminal cells. None = automatic (the pre-resize defaults).
#[derive(Serialize, Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct LayoutCfg {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar_width: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_list_height: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor_height: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_width: Option<u16>,
}

fn default_selection() -> DbSelection {
    DbSelection::All("all".into())
}

pub fn normalize_engine(engine: &str) -> String {
    match engine.trim().to_ascii_lowercase().as_str() {
        "postgresql" => "postgres".into(),
        other => other.into(),
    }
}

pub fn supported_engine(engine: &str) -> bool {
    matches!(
        normalize_engine(engine).as_str(),
        "mssql" | "postgres" | "sqlite"
    )
}

/// Engines that can enumerate the other databases on the same host.
pub fn supports_listing(engine: &str) -> bool {
    matches!(normalize_engine(engine).as_str(), "mssql" | "postgres")
}

/// The engines the form can be forced to, in cycle order.
pub const ENGINES: &[&str] = &["mssql", "postgres", "sqlite"];

const SQLITE_EXTENSIONS: &[&str] = &[".db", ".db3", ".sqlite", ".sqlite3"];

fn looks_like_sqlite_file(value: &str) -> bool {
    let base = value.split('?').next().unwrap_or(value).trim();
    let lower = base.to_ascii_lowercase();
    SQLITE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

/// Key/value pairs of an ADO.NET-style connection string, keys lowercased.
fn ado_pairs(conn_str: &str) -> Vec<(String, String)> {
    conn_str
        .split(';')
        .filter_map(|segment| segment.split_once('='))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect()
}

/// Work out the engine from the shape of the connection string. The three
/// formats quim speaks don't overlap: Postgres is a URL, SQLite is a file path
/// or `sqlite:` URL, and MSSQL is `Key=Value;`. None when it matches nothing —
/// the form then asks for the engine outright.
pub fn detect_engine(conn_str: &str) -> Option<String> {
    let raw = conn_str.trim();
    if raw.is_empty() {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("postgres://") || lower.starts_with("postgresql://") {
        return Some("postgres".into());
    }
    if lower.starts_with("sqlite:") || lower.starts_with("file:") || raw == ":memory:" {
        return Some("sqlite".into());
    }

    let pairs = ado_pairs(raw);
    if !pairs.is_empty() {
        let has = |wanted: &str| pairs.iter().any(|(key, _)| key == wanted);
        let value_of = |wanted: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == wanted)
                .map(|(_, value)| value.as_str())
        };
        // "Data Source=C:\app.db" is valid ADO.NET for SQLite too, so a data
        // source that looks like a database file wins over the MSSQL reading —
        // but only when no genuinely MSSQL-only key is present.
        if !has("server") && !has("initial catalog") && !has("database") {
            if let Some(value) = value_of("data source") {
                if looks_like_sqlite_file(value) {
                    return Some("sqlite".into());
                }
            }
        }
        if has("server") || has("data source") || has("address") || has("addr") {
            return Some("mssql".into());
        }
        return None;
    }

    if looks_like_sqlite_file(raw) || raw.starts_with('/') || raw.starts_with('~') {
        return Some("sqlite".into());
    }
    None
}

/// Where a connection points, for the sidebar and the form's detected line.
pub fn location_of(engine: &str, conn_str: &str) -> String {
    match normalize_engine(engine).as_str() {
        "mssql" => server_of(conn_str).unwrap_or_default(),
        "postgres" => postgres_host_of(conn_str).unwrap_or_default(),
        "sqlite" => conn_str.trim().to_string(),
        _ => String::new(),
    }
}

/// The database to connect to when probing the connection itself rather than
/// one of its databases. MSSQL needs a database to log in to; the others are
/// happy with the string's own default.
pub fn probe_database(engine: &str, conn_str: &str) -> String {
    if let Some(db) = database_of_engine(engine, conn_str) {
        return db;
    }
    match normalize_engine(engine).as_str() {
        "mssql" => "master".into(),
        _ => String::new(),
    }
}

/// The database the connection string names, if it names one.
pub fn database_of_engine(engine: &str, conn_str: &str) -> Option<String> {
    match normalize_engine(engine).as_str() {
        "mssql" => database_of(conn_str),
        "postgres" => postgres_database_of(conn_str),
        "sqlite" => Some("main".into()),
        _ => None,
    }
}

impl DbSelection {
    pub fn is_all(&self) -> bool {
        matches!(self, DbSelection::All(_))
    }

    pub fn all() -> Self {
        default_selection()
    }

    /// True when this database should be listed under its connection.
    pub fn includes(&self, name: &str) -> bool {
        match self {
            DbSelection::All(_) => true,
            DbSelection::Named(names) => names.iter().any(|n| n == name),
        }
    }

    pub fn names(&self) -> &[String] {
        match self {
            DbSelection::All(_) => &[],
            DbSelection::Named(names) => names,
        }
    }
}

pub fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    let base = if cfg!(target_os = "windows") {
        std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(&home).join("AppData").join("Roaming"))
    } else if cfg!(target_os = "macos") {
        PathBuf::from(&home)
            .join("Library")
            .join("Application Support")
    } else {
        std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(&home).join(".config"))
    };
    base.join("quim").join("config.json")
}

/// Point `config_path` at a scratch directory, so tests that build an `App`
/// can't write over the real config. Hold the guard for the whole test: the
/// environment and `save`'s temp file are both process-wide, so two tests
/// saving at once would clobber each other.
#[cfg(test)]
pub fn use_scratch_config_dir() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    static LOCK: Mutex<()> = Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = DIR.get_or_init(|| tempfile::tempdir().unwrap());
    std::env::set_var("XDG_CONFIG_HOME", dir.path());
    std::env::set_var("HOME", dir.path());
    guard
}

pub fn load() -> Result<Config, String> {
    let path = config_path();
    if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
        return serde_json::from_str(&raw)
            .map_err(|e| format!("Broken config {}: {e}", path.display()));
    }
    // Env seed for a fresh setup without adding a connection by hand.
    if let Ok(seed) = std::env::var("QUIM_CONNECTION") {
        return Ok(Config {
            connections: vec![ConnectionCfg {
                id: "conn_env".into(),
                name: "Default".into(),
                engine: None,
                databases: selection_for(&detect_engine(&seed).unwrap_or_default(), &seed),
                connection_string: seed,
                extra: Default::default(),
            }],
            ..Default::default()
        });
    }
    Ok(Config::default())
}

/// What a freshly added connection should list: the one database its string
/// names, or everything on the host when it names none.
pub fn selection_for(engine: &str, conn_str: &str) -> DbSelection {
    match database_of_engine(engine, conn_str) {
        Some(db) => DbSelection::Named(vec![db]),
        None => DbSelection::all(),
    }
}

/// Write the config back, pretty-printed, via a temp file + rename so a crash
/// mid-write can't truncate it.
pub fn save(cfg: &Config) -> Result<(), String> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json + "\n")
        .map_err(|e| format!("Could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("Could not save {}: {e}", path.display()))
}

/// New connection id: prefix + 7 base-36 chars.
pub fn new_id(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut n = (nanos as u64) ^ (std::process::id() as u64) << 32;
    const TABLE: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut suffix = String::with_capacity(7);
    for _ in 0..7 {
        // xorshift so consecutive calls within the same nanosecond still differ
        n ^= n << 13;
        n ^= n >> 7;
        n ^= n << 17;
        suffix.push(TABLE[(n % 36) as usize] as char);
    }
    format!("{prefix}_{suffix}")
}

/// Pull Server=/Data Source= out of an ADO.NET connection string.
pub fn server_of(conn_str: &str) -> Option<String> {
    for (key, value) in ado_pairs(conn_str) {
        if key == "server" || key == "data source" || key == "address" || key == "addr" {
            return Some(value);
        }
    }
    None
}

/// Pull Database=/Initial Catalog= out of an ADO.NET connection string.
pub fn database_of(conn_str: &str) -> Option<String> {
    for (key, value) in ado_pairs(conn_str) {
        if key == "database" || key == "initial catalog" {
            return Some(value);
        }
    }
    None
}

pub fn postgres_database_of(conn_str: &str) -> Option<String> {
    let raw = conn_str.trim();
    if !(raw.starts_with("postgres://") || raw.starts_with("postgresql://")) {
        return None;
    }
    let base = raw.split_once('?').map(|(base, _)| base).unwrap_or(raw);
    let rest = base.split_once("://").map(|(_, rest)| rest)?;
    let path = rest.split_once('/').map(|(_, path)| path)?;
    let db = path.split('/').next().unwrap_or("").trim();
    if db.is_empty() {
        None
    } else {
        Some(db.to_string())
    }
}

pub fn postgres_host_of(conn_str: &str) -> Option<String> {
    let raw = conn_str.trim();
    if !(raw.starts_with("postgres://") || raw.starts_with("postgresql://")) {
        return None;
    }
    let rest = raw.split_once("://").map(|(_, rest)| rest)?;
    let authority = rest.split(['/', '?']).next().unwrap_or("");
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    if host_port.is_empty() {
        None
    } else {
        Some(host_port.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_unknown_fields_and_selection() {
        let raw = r#"{
            "connections": [{
                "id": "conn_abc1234",
                "name": "Local",
                "connectionString": "Server=localhost;User Id=sa;Password=x",
                "databases": "all",
                "customField": 42
            }],
            "someOtherKey": {"nested": true}
        }"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();
        assert!(cfg.connections[0].databases.is_all());
        assert!(cfg.quim.vim_mode); // default on
        let out = serde_json::to_string_pretty(&cfg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["connections"][0]["databases"], "all");
        assert_eq!(v["connections"][0]["customField"], 42);
        // No engine was set, so none is written back — it stays derived.
        assert!(v["connections"][0].get("engine").is_none());
        assert_eq!(v["someOtherKey"]["nested"], true);
        assert_eq!(v["quim"]["vimMode"], true);

        let named = r#"{"connections":[
            {"id":"c","name":"n","connectionString":"c","databases":["a","b"]}]}"#;
        let cfg: Config = serde_json::from_str(named).unwrap();
        let out = serde_json::to_string(&cfg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["connections"][0]["databases"],
            serde_json::json!(["a", "b"])
        );
    }

    #[test]
    fn new_ids_have_prefix_and_are_unique() {
        let a = new_id("conn");
        let b = new_id("conn");
        assert!(a.starts_with("conn_") && a.len() == 12);
        assert_ne!(a, b);
    }

    #[test]
    fn detects_the_engine_from_the_connection_string() {
        let cases = [
            ("postgres://user:pass@localhost:5432/app", Some("postgres")),
            (
                "postgresql://localhost/app?sslmode=disable",
                Some("postgres"),
            ),
            ("Server=localhost,1433;User Id=sa;Password=x", Some("mssql")),
            ("server=db;Initial Catalog=App;Encrypt=False", Some("mssql")),
            ("Data Source=sql01;Initial Catalog=App", Some("mssql")),
            ("/home/h/.cursor/store.db", Some("sqlite")),
            ("~/notes.sqlite3", Some("sqlite")),
            ("sqlite:///tmp/app.db", Some("sqlite")),
            ("file:memdb?mode=memory", Some("sqlite")),
            (":memory:", Some("sqlite")),
            // ADO.NET SQLite: a data source that is plainly a file.
            ("Data Source=C:\\data\\app.db;Version=3", Some("sqlite")),
            // ...but a data source that is a host is MSSQL.
            ("Data Source=sql01.corp", Some("mssql")),
            ("", None),
            ("localhost", None),
            ("mysql://root@localhost/app", None),
        ];
        for (conn, want) in cases {
            assert_eq!(
                detect_engine(conn).as_deref(),
                want,
                "detect_engine({conn:?})"
            );
        }
    }

    #[test]
    fn an_explicit_engine_overrides_detection() {
        let mut c = ConnectionCfg {
            id: "conn_1".into(),
            name: "odd".into(),
            engine: None,
            connection_string: "localhost".into(),
            databases: DbSelection::all(),
            extra: Default::default(),
        };
        assert_eq!(c.engine(), "");
        c.engine = Some("postgresql".into());
        assert_eq!(c.engine(), "postgres");
    }

    #[test]
    fn a_named_database_makes_the_connection_a_single_database() {
        let single = |conn: &str| ConnectionCfg {
            id: "conn_1".into(),
            name: "x".into(),
            engine: None,
            databases: selection_for(&detect_engine(conn).unwrap_or_default(), conn),
            connection_string: conn.into(),
            extra: Default::default(),
        };

        let pg = single("postgres://u:p@localhost:5432/app");
        assert!(pg.is_single_database());
        assert_eq!(pg.single_database(), "app");

        let host = single("Server=localhost,1433;User Id=sa;Password=x");
        assert!(!host.is_single_database());
        assert!(host.databases.is_all());

        let file = single("/tmp/app.db");
        assert!(file.is_single_database());
        assert_eq!(file.single_database(), "main");

        // A host connection stays a host however far it is narrowed — its
        // string names no database, so there is nothing to collapse it to.
        let mut narrowed = host;
        narrowed.databases = DbSelection::Named(vec!["a".into(), "b".into()]);
        assert!(!narrowed.is_single_database());
        narrowed.databases = DbSelection::Named(vec!["a".into()]);
        assert!(!narrowed.is_single_database());

        // ...and listing everything on a host that names one re-opens it.
        let mut widened = pg;
        widened.databases = DbSelection::all();
        assert!(!widened.is_single_database());
    }

    #[test]
    fn probe_database_prefers_the_named_one() {
        assert_eq!(
            probe_database("mssql", "Server=localhost;Database=App"),
            "App"
        );
        assert_eq!(probe_database("mssql", "Server=localhost"), "master");
        assert_eq!(probe_database("postgres", "postgres://h/app"), "app");
        assert_eq!(probe_database("sqlite", "/tmp/a.db"), "main");
    }

    #[test]
    fn location_of_summarises_where_a_connection_points() {
        assert_eq!(
            location_of("mssql", "Server=sql01,1433;Database=App"),
            "sql01,1433"
        );
        assert_eq!(
            location_of("postgres", "postgres://u:p@127.0.0.1:15433/app"),
            "127.0.0.1:15433"
        );
        assert_eq!(location_of("sqlite", "/tmp/a.db"), "/tmp/a.db");
    }
}
