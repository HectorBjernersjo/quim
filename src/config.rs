// Reads and writes quim's config: ~/.config/quim/config.json (XDG),
// ~/Library/Application Support/quim on macOS, %APPDATA%\quim on Windows.
// quim's own settings (vim mode, pane layout) live under the "quim" key;
// unknown keys are preserved on save.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub servers: Vec<ServerCfg>,
    #[serde(default)]
    pub databases: Vec<DatabaseCfg>,
    #[serde(default)]
    pub quim: QuimCfg,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServerCfg {
    pub id: String,
    pub name: String,
    #[serde(default = "default_engine")]
    pub engine: String,
    pub connection_string: String,
    #[serde(default = "default_selection")]
    pub databases: DbSelection,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseCfg {
    pub id: String,
    pub name: String,
    #[serde(default = "default_engine")]
    pub engine: String,
    pub connection_string: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize, Deserialize, Clone)]
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
    /// Entry id ("s:<serverId>:<db>" or "d:<dbId>") re-selected on startup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_db: Option<String>,
}

impl Default for QuimCfg {
    fn default() -> Self {
        QuimCfg {
            vim_mode: true,
            layout: LayoutCfg::default(),
            active_db: None,
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

fn default_engine() -> String {
    "mssql".into()
}

fn default_selection() -> DbSelection {
    DbSelection::All("all".into())
}

pub fn normalize_engine(engine: &str) -> String {
    match engine.trim().to_ascii_lowercase().as_str() {
        "" => "mssql".into(),
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

pub fn supports_server_sources(engine: &str) -> bool {
    normalize_engine(engine) != "sqlite"
}

pub fn server_test_database(engine: &str) -> String {
    match normalize_engine(engine).as_str() {
        "mssql" => "master".into(),
        _ => String::new(),
    }
}

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

pub fn load() -> Result<Config, String> {
    let path = config_path();
    if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
        return serde_json::from_str(&raw)
            .map_err(|e| format!("Broken config {}: {e}", path.display()));
    }
    // Env seed for a fresh setup without opening the settings UI.
    if let Ok(seed) = std::env::var("QUIM_CONNECTION") {
        return Ok(Config {
            servers: vec![ServerCfg {
                id: "srv_env".into(),
                name: "Default".into(),
                engine: "mssql".into(),
                connection_string: seed,
                databases: default_selection(),
                extra: Default::default(),
            }],
            ..Default::default()
        });
    }
    Ok(Config::default())
}

/// Write the config back, pretty-printed, via a temp file + rename so a crash
/// mid-write can't truncate it (the web app writes it non-atomically).
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

/// New source id in quim's format: prefix + 7 base-36 chars.
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
    for segment in conn_str.split(';') {
        let Some((key, value)) = segment.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if key == "server" || key == "data source" || key == "address" || key == "addr" {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// Pull Database=/Initial Catalog= out of an ADO.NET connection string.
pub fn database_of(conn_str: &str) -> Option<String> {
    for segment in conn_str.split(';') {
        let Some((key, value)) = segment.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if key == "database" || key == "initial catalog" {
            return Some(value.trim().to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_unknown_fields_and_selection() {
        let raw = r#"{
            "servers": [{
                "id": "srv_abc1234",
                "name": "Local",
                "engine": "mssql",
                "connectionString": "Server=localhost;User Id=sa;Password=x",
                "databases": "all",
                "customField": 42
            }],
            "databases": [{
                "id": "db_xyz",
                "name": "app",
                "connectionString": "Server=h;Database=app;User Id=u;Password=p"
            }],
            "someWebOnlyKey": {"nested": true}
        }"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();
        assert!(cfg.servers[0].databases.is_all());
        assert!(cfg.quim.vim_mode); // default on
        let out = serde_json::to_string_pretty(&cfg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["servers"][0]["databases"], "all");
        assert_eq!(v["servers"][0]["customField"], 42);
        assert_eq!(
            v["servers"][0]["connectionString"],
            "Server=localhost;User Id=sa;Password=x"
        );
        assert_eq!(v["databases"][0]["engine"], "mssql");
        assert_eq!(v["someWebOnlyKey"]["nested"], true);
        assert_eq!(v["quim"]["vimMode"], true);

        let named =
            r#"{"servers":[{"id":"s","name":"n","connectionString":"c","databases":["a","b"]}]}"#;
        let cfg: Config = serde_json::from_str(named).unwrap();
        let out = serde_json::to_string(&cfg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["servers"][0]["databases"], serde_json::json!(["a", "b"]));
    }

    #[test]
    fn new_ids_have_prefix_and_are_unique() {
        let a = new_id("srv");
        let b = new_id("srv");
        assert!(a.starts_with("srv_") && a.len() == 11);
        assert_ne!(a, b);
    }

    #[test]
    fn engine_helpers_understand_postgres_and_sqlite() {
        assert_eq!(normalize_engine("postgresql"), "postgres");
        assert!(supported_engine("sqlite"));
        assert!(!supports_server_sources("sqlite"));
        assert_eq!(
            postgres_database_of("postgres://user:pass@localhost:5432/app?sslmode=disable")
                .as_deref(),
            Some("app")
        );
        assert_eq!(
            database_of_engine("sqlite", "/tmp/app.db").as_deref(),
            Some("main")
        );
    }

    #[test]
    fn round_trip_preserves_postgres_and_sqlite_engines() {
        let raw = r#"{
            "servers": [{
                "id": "srv_pg",
                "name": "Postgres",
                "engine": "postgres",
                "connectionString": "postgres://user:pass@localhost:5432/app",
                "databases": ["app"]
            }],
            "databases": [{
                "id": "db_sqlite",
                "name": "Local file",
                "engine": "sqlite",
                "connectionString": "/tmp/app.db"
            }]
        }"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();
        let out = serde_json::to_value(&cfg).unwrap();
        assert_eq!(out["servers"][0]["engine"], "postgres");
        assert_eq!(out["databases"][0]["engine"], "sqlite");
    }
}
