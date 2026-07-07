// DB worker: a dedicated thread owning a tokio runtime and connection caches,
// fed requests over an mpsc channel. Keeps the UI thread free.
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Column, PgPool, Row, SqlitePool, TypeInfo, ValueRef};
use tiberius::{Client, ColumnData, ColumnType, Config as TdsConfig, FromSql};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

pub type Conn = Client<Compat<TcpStream>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const QUERY_TIMEOUT: Duration = Duration::from_secs(120);

pub enum DbRequest {
    ListDatabases {
        server_id: String,
        engine: String,
        conn: String,
    },
    Schema {
        db_id: String,
        engine: String,
        conn: String,
        database: String,
    },
    Query {
        engine: String,
        conn: String,
        database: String,
        sql: String,
    },
    /// Connect and run SELECT 1; used by the source editor before saving.
    TestConnection {
        token: String,
        engine: String,
        conn: String,
        database: String,
    },
}

pub enum DbResponse {
    Databases {
        server_id: String,
        result: Result<Vec<String>, String>,
    },
    Schema {
        db_id: String,
        result: Result<Vec<TableInfo>, String>,
    },
    Query(QueryOutcome),
    TestResult {
        token: String,
        result: Result<(), String>,
    },
}

#[derive(Clone)]
pub struct TableInfo {
    pub schema: String,
    pub name: String,
    pub rows: Option<i64>,
    pub columns: Vec<ColumnInfo>,
}

#[derive(Clone)]
pub struct ColumnInfo {
    pub name: String,
    #[allow(dead_code)]
    pub ty: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Str,
    Uuid,
    Date,
    Num,
    Bool,
    Bin,
    Other,
}

#[derive(Clone)]
pub struct ColMeta {
    pub name: String,
    pub category: Category,
}

pub struct QueryOutcome {
    pub columns: Vec<ColMeta>,
    pub rows: Vec<Vec<Option<String>>>,
    pub error: Option<String>,
    pub elapsed_ms: u128,
}

#[derive(Default)]
struct DbPools {
    mssql: HashMap<String, Conn>,
    postgres: HashMap<String, PgPool>,
    sqlite: HashMap<String, SqlitePool>,
}

pub fn spawn_worker() -> (Sender<DbRequest>, Receiver<DbResponse>) {
    let (req_tx, req_rx) = channel::<DbRequest>();
    let (resp_tx, resp_rx) = channel::<DbResponse>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let mut pools = DbPools::default();
        while let Ok(req) = req_rx.recv() {
            let resp = rt.block_on(handle(&mut pools, req));
            if resp_tx.send(resp).is_err() {
                break;
            }
        }
    });
    (req_tx, resp_rx)
}

async fn handle(pools: &mut DbPools, req: DbRequest) -> DbResponse {
    match req {
        DbRequest::ListDatabases {
            server_id,
            engine,
            conn,
        } => DbResponse::Databases {
            server_id,
            result: list_databases(pools, &engine, &conn).await,
        },
        DbRequest::Schema {
            db_id,
            engine,
            conn,
            database,
        } => DbResponse::Schema {
            db_id,
            result: get_schema(pools, &engine, &conn, &database).await,
        },
        DbRequest::Query {
            engine,
            conn,
            database,
            sql,
        } => {
            let started = Instant::now();
            let result = run_query(pools, &engine, &conn, &database, &sql).await;
            let elapsed_ms = started.elapsed().as_millis();
            DbResponse::Query(match result {
                Ok((columns, rows)) => QueryOutcome {
                    columns,
                    rows,
                    error: None,
                    elapsed_ms,
                },
                Err(e) => QueryOutcome {
                    columns: vec![],
                    rows: vec![],
                    error: Some(e),
                    elapsed_ms,
                },
            })
        }
        DbRequest::TestConnection {
            token,
            engine,
            conn,
            database,
        } => DbResponse::TestResult {
            token,
            result: test_connection(pools, &engine, &conn, &database).await,
        },
    }
}

fn engine_name(engine: &str) -> String {
    engine.trim().to_ascii_lowercase()
}

fn unsupported_engine(engine: &str) -> String {
    format!("Engine \"{engine}\" is not supported")
}

async fn test_connection(
    pools: &mut DbPools,
    engine: &str,
    conn_str: &str,
    database: &str,
) -> Result<(), String> {
    match engine_name(engine).as_str() {
        "mssql" => mssql_test_connection(&mut pools.mssql, conn_str, database).await,
        "postgres" | "postgresql" => {
            pg_test_connection(&mut pools.postgres, conn_str, database).await
        }
        "sqlite" => sqlite_test_connection(&mut pools.sqlite, conn_str, database).await,
        _ => Err(unsupported_engine(engine)),
    }
}

async fn list_databases(
    pools: &mut DbPools,
    engine: &str,
    conn_str: &str,
) -> Result<Vec<String>, String> {
    match engine_name(engine).as_str() {
        "mssql" => mssql_list_databases(&mut pools.mssql, conn_str).await,
        "postgres" | "postgresql" => pg_list_databases(&mut pools.postgres, conn_str).await,
        "sqlite" => {
            Err("SQLite is a single database file; add it as a standalone database.".into())
        }
        _ => Err(unsupported_engine(engine)),
    }
}

async fn get_schema(
    pools: &mut DbPools,
    engine: &str,
    conn_str: &str,
    database: &str,
) -> Result<Vec<TableInfo>, String> {
    match engine_name(engine).as_str() {
        "mssql" => mssql_get_schema(&mut pools.mssql, conn_str, database).await,
        "postgres" | "postgresql" => pg_get_schema(&mut pools.postgres, conn_str, database).await,
        "sqlite" => sqlite_get_schema(&mut pools.sqlite, conn_str, database).await,
        _ => Err(unsupported_engine(engine)),
    }
}

type QueryRows = (Vec<ColMeta>, Vec<Vec<Option<String>>>);

async fn run_query(
    pools: &mut DbPools,
    engine: &str,
    conn_str: &str,
    database: &str,
    sql: &str,
) -> Result<QueryRows, String> {
    match engine_name(engine).as_str() {
        "mssql" => mssql_run_query(&mut pools.mssql, conn_str, database, sql).await,
        "postgres" | "postgresql" => {
            pg_run_query(&mut pools.postgres, conn_str, database, sql).await
        }
        "sqlite" => sqlite_run_query(&mut pools.sqlite, conn_str, database, sql).await,
        _ => Err(unsupported_engine(engine)),
    }
}

// --- MSSQL ------------------------------------------------------------------------

fn pool_key(conn_str: &str, database: &str) -> String {
    format!("{conn_str}||{database}")
}

async fn connect(conn_str: &str, database: &str) -> Result<Conn, tiberius::error::Error> {
    let mut config = TdsConfig::from_ado_string(conn_str)?;
    if !database.is_empty() {
        config.database(database);
    }
    let tcp = TcpStream::connect(config.get_addr()).await?;
    tcp.set_nodelay(true)?;
    Client::connect(config, tcp.compat_write()).await
}

async fn get_conn<'a>(
    pool: &'a mut HashMap<String, Conn>,
    conn_str: &str,
    database: &str,
) -> Result<&'a mut Conn, String> {
    let key = pool_key(conn_str, database);
    if !pool.contains_key(&key) {
        let client = match tokio::time::timeout(CONNECT_TIMEOUT, connect(conn_str, database)).await
        {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => return Err(format!("Could not connect: {}", friendly(&e))),
            Err(_) => return Err("Could not connect: timeout after 8s".into()),
        };
        pool.insert(key.clone(), client);
    }
    Ok(pool.get_mut(&key).unwrap())
}

fn friendly(e: &tiberius::error::Error) -> String {
    use tiberius::error::Error as E;
    match e {
        E::Server(te) => te.message().to_string(),
        other => other.to_string(),
    }
}

fn is_connection_error(e: &tiberius::error::Error) -> bool {
    use tiberius::error::Error as E;
    matches!(e, E::Io { .. } | E::Tls(_) | E::Protocol(_))
}

/// Run `op` against a cached connection; on a connection-level error, drop the
/// cached client and retry once with a fresh one. Server errors (syntax etc.)
/// are never retried — a write must not run twice.
macro_rules! with_conn_retry {
    ($pool:expr, $conn_str:expr, $database:expr, $conn:ident => $op:expr) => {{
        let key = pool_key($conn_str, $database);
        let mut attempt = 0;
        loop {
            let $conn = get_conn($pool, $conn_str, $database).await?;
            let result = tokio::time::timeout(QUERY_TIMEOUT, $op).await;
            match result {
                Ok(Ok(v)) => break Ok(v),
                Ok(Err(e)) => {
                    if is_connection_error(&e) {
                        $pool.remove(&key);
                        if attempt == 0 {
                            attempt += 1;
                            continue;
                        }
                    }
                    break Err(friendly(&e));
                }
                Err(_) => {
                    // The connection has an abandoned in-flight query; discard it.
                    $pool.remove(&key);
                    break Err("Timeout: the query took more than 120s".into());
                }
            }
        }
    }};
}

async fn mssql_test_connection(
    pool: &mut HashMap<String, Conn>,
    conn_str: &str,
    database: &str,
) -> Result<(), String> {
    // Always test on a fresh connection — a cached one would hide auth changes.
    pool.remove(&pool_key(conn_str, database));
    with_conn_retry!(pool, conn_str, database, conn => async {
        conn.simple_query("SELECT 1").await?.into_results().await?;
        Ok::<_, tiberius::error::Error>(())
    })
}

async fn mssql_list_databases(
    pool: &mut HashMap<String, Conn>,
    conn_str: &str,
) -> Result<Vec<String>, String> {
    const SQL: &str = "SELECT name FROM sys.databases \
        WHERE name NOT IN ('master', 'model', 'msdb', 'tempdb') AND state = 0 ORDER BY name";
    with_conn_retry!(pool, conn_str, "master", conn => async {
        let sets = conn.simple_query(SQL).await?.into_results().await?;
        let names = sets
            .into_iter()
            .next()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|row| row.get::<&str, _>(0).map(str::to_string))
            .collect();
        Ok::<_, tiberius::error::Error>(names)
    })
}

async fn mssql_get_schema(
    pool: &mut HashMap<String, Conn>,
    conn_str: &str,
    database: &str,
) -> Result<Vec<TableInfo>, String> {
    const TABLES_SQL: &str = "SELECT t.TABLE_SCHEMA, t.TABLE_NAME, c.COLUMN_NAME, c.DATA_TYPE \
        FROM INFORMATION_SCHEMA.TABLES t \
        LEFT JOIN INFORMATION_SCHEMA.COLUMNS c \
          ON c.TABLE_SCHEMA = t.TABLE_SCHEMA AND c.TABLE_NAME = t.TABLE_NAME \
        WHERE t.TABLE_TYPE = 'BASE TABLE' \
        ORDER BY t.TABLE_SCHEMA, t.TABLE_NAME, c.ORDINAL_POSITION";
    const COUNTS_SQL: &str = "SELECT s.name AS [schema], t.name AS [table], SUM(p.rows) AS [rows] \
        FROM sys.tables t \
        JOIN sys.schemas s ON s.schema_id = t.schema_id \
        JOIN sys.partitions p ON p.object_id = t.object_id AND p.index_id IN (0, 1) \
        GROUP BY s.name, t.name";

    let mut tables: Vec<TableInfo> = with_conn_retry!(pool, conn_str, database, conn => async {
        let sets = conn.simple_query(TABLES_SQL).await?.into_results().await?;
        let mut order: Vec<TableInfo> = vec![];
        let mut index: HashMap<String, usize> = HashMap::new();
        for row in sets.into_iter().next().unwrap_or_default() {
            let schema = row.get::<&str, _>(0).unwrap_or("").to_string();
            let name = row.get::<&str, _>(1).unwrap_or("").to_string();
            let key = format!("{schema}.{name}");
            let idx = *index.entry(key).or_insert_with(|| {
                order.push(TableInfo { schema, name, rows: None, columns: vec![] });
                order.len() - 1
            });
            if let Some(col) = row.get::<&str, _>(2) {
                order[idx].columns.push(ColumnInfo {
                    name: col.to_string(),
                    ty: row.get::<&str, _>(3).unwrap_or("").to_string(),
                });
            }
        }
        Ok::<_, tiberius::error::Error>(order)
    })?;

    // Row counts are best-effort.
    let counts: Result<Vec<(String, String, i64)>, String> = with_conn_retry!(pool, conn_str, database, conn => async {
        let sets = conn.simple_query(COUNTS_SQL).await?.into_results().await?;
        let out = sets
            .into_iter()
            .next()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|row| {
                Some((
                    row.get::<&str, _>(0)?.to_string(),
                    row.get::<&str, _>(1)?.to_string(),
                    row.get::<i64, _>(2)?,
                ))
            })
            .collect();
        Ok::<_, tiberius::error::Error>(out)
    });
    if let Ok(counts) = counts {
        for (schema, name, rows) in counts {
            if let Some(t) = tables
                .iter_mut()
                .find(|t| t.schema == schema && t.name == name)
            {
                t.rows = Some(rows);
            }
        }
    }
    Ok(tables)
}

async fn mssql_run_query(
    pool: &mut HashMap<String, Conn>,
    conn_str: &str,
    database: &str,
    sql: &str,
) -> Result<QueryRows, String> {
    with_conn_retry!(pool, conn_str, database, conn => async {
        let mut stream = conn.simple_query(sql).await?;
        let columns: Vec<ColMeta> = stream
            .columns()
            .await?
            .map(|cols| {
                cols.iter()
                    .map(|c| ColMeta {
                        name: c.name().to_string(),
                        category: categorize(c.column_type()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let sets = stream.into_results().await?;
        let rows = sets
            .into_iter()
            .next()
            .unwrap_or_default()
            .into_iter()
            .map(|row| row.into_iter().map(cell_to_string).collect())
            .collect();
        Ok::<_, tiberius::error::Error>((columns, rows))
    })
}

fn categorize(ty: ColumnType) -> Category {
    use ColumnType::*;
    match ty {
        Bit | Bitn => Category::Bool,
        Int1 | Int2 | Int4 | Int8 | Intn | Float4 | Float8 | Floatn | Decimaln | Numericn
        | Money | Money4 => Category::Num,
        Guid => Category::Uuid,
        Datetime | Datetime4 | Datetimen | Daten | Timen | Datetime2 | DatetimeOffsetn => {
            Category::Date
        }
        BigVarBin | BigBinary | Image => Category::Bin,
        BigVarChar | BigChar | NVarchar | NChar | Text | NText | Xml => Category::Str,
        _ => Category::Other,
    }
}

fn cell_to_string(data: ColumnData<'static>) -> Option<String> {
    use ColumnData::*;
    match data {
        Bit(v) => v.map(|b| b.to_string()),
        U8(v) => v.map(|x| x.to_string()),
        I16(v) => v.map(|x| x.to_string()),
        I32(v) => v.map(|x| x.to_string()),
        I64(v) => v.map(|x| x.to_string()),
        F32(v) => v.map(|x| x.to_string()),
        F64(v) => v.map(|x| x.to_string()),
        Numeric(v) => v.map(|n| n.to_string()),
        String(v) => v.map(|s| s.into_owned()),
        Guid(v) => v.map(|g| g.to_string()),
        Binary(v) => v.map(|b| {
            let mut s = std::string::String::with_capacity(2 + b.len() * 2);
            s.push_str("0x");
            for byte in b.iter() {
                s.push_str(&format!("{byte:02x}"));
            }
            s
        }),
        Xml(v) => v.map(|x| x.as_ref().to_string()),
        d @ (DateTime(_) | SmallDateTime(_) | DateTime2(_)) => chrono::NaiveDateTime::from_sql(&d)
            .ok()
            .flatten()
            .map(|dt| dt.format("%Y-%m-%d, %H:%M:%S").to_string()),
        d @ Date(_) => chrono::NaiveDate::from_sql(&d)
            .ok()
            .flatten()
            .map(|dt| dt.format("%Y-%m-%d").to_string()),
        d @ Time(_) => chrono::NaiveTime::from_sql(&d)
            .ok()
            .flatten()
            .map(|dt| dt.format("%H:%M:%S").to_string()),
        d @ DateTimeOffset(_) => chrono::DateTime::<chrono::Utc>::from_sql(&d)
            .ok()
            .flatten()
            .map(|dt| dt.format("%Y-%m-%d, %H:%M:%S UTC").to_string()),
    }
}

// --- PostgreSQL -------------------------------------------------------------------

fn pg_conn_for_database(conn_str: &str, database: &str) -> String {
    let raw = conn_str.trim();
    let db = database.trim();
    if db.is_empty() {
        return raw.to_string();
    }
    if !(raw.starts_with("postgres://") || raw.starts_with("postgresql://")) {
        return raw.to_string();
    }

    let Some(scheme_idx) = raw.find("://") else {
        return raw.to_string();
    };
    let rest_start = scheme_idx + 3;
    let (without_query, query) = raw
        .split_once('?')
        .map(|(base, q)| (base, format!("?{q}")))
        .unwrap_or((raw, String::new()));
    let rest = &without_query[rest_start..];
    if let Some(path_idx) = rest.find('/') {
        let prefix = &without_query[..rest_start + path_idx];
        format!("{prefix}/{db}{query}")
    } else {
        format!("{without_query}/{db}{query}")
    }
}

async fn get_pg_pool<'a>(
    pools: &'a mut HashMap<String, PgPool>,
    conn_str: &str,
    database: &str,
) -> Result<&'a PgPool, String> {
    let url = pg_conn_for_database(conn_str, database);
    if !pools.contains_key(&url) {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(CONNECT_TIMEOUT)
            .connect(&url)
            .await
            .map_err(sqlx_error)?;
        pools.insert(url.clone(), pool);
    }
    Ok(pools.get(&url).unwrap())
}

async fn pg_test_connection(
    pools: &mut HashMap<String, PgPool>,
    conn_str: &str,
    database: &str,
) -> Result<(), String> {
    let key = pg_conn_for_database(conn_str, database);
    pools.remove(&key);
    let pool = get_pg_pool(pools, conn_str, database).await?;
    query_timeout(async {
        sqlx::query("SELECT 1")
            .execute(pool)
            .await
            .map(|_| ())
            .map_err(sqlx_error)
    })
    .await
}

async fn pg_list_databases(
    pools: &mut HashMap<String, PgPool>,
    conn_str: &str,
) -> Result<Vec<String>, String> {
    const SQL: &str = "SELECT datname FROM pg_database \
        WHERE datallowconn AND NOT datistemplate ORDER BY datname";
    let pool = get_pg_pool(pools, conn_str, "").await?;
    query_timeout(async {
        let rows = sqlx::query(SQL).fetch_all(pool).await.map_err(sqlx_error)?;
        Ok(rows
            .into_iter()
            .filter_map(|row| row.try_get::<String, _>(0).ok())
            .collect())
    })
    .await
}

async fn pg_get_schema(
    pools: &mut HashMap<String, PgPool>,
    conn_str: &str,
    database: &str,
) -> Result<Vec<TableInfo>, String> {
    const TABLES_SQL: &str = "SELECT t.table_schema, t.table_name, c.column_name, c.data_type \
        FROM information_schema.tables t \
        LEFT JOIN information_schema.columns c \
          ON c.table_schema = t.table_schema AND c.table_name = t.table_name \
        WHERE t.table_type = 'BASE TABLE' \
          AND t.table_schema NOT IN ('pg_catalog', 'information_schema') \
        ORDER BY t.table_schema, t.table_name, c.ordinal_position";
    const COUNTS_SQL: &str =
        "SELECT n.nspname, c.relname, GREATEST(c.reltuples::bigint, 0) AS rows \
        FROM pg_class c \
        JOIN pg_namespace n ON n.oid = c.relnamespace \
        WHERE c.relkind = 'r' AND n.nspname NOT IN ('pg_catalog', 'information_schema')";

    let pool = get_pg_pool(pools, conn_str, database).await?;
    let mut tables = query_timeout(async {
        let rows = sqlx::query(TABLES_SQL)
            .fetch_all(pool)
            .await
            .map_err(sqlx_error)?;
        Ok(table_infos_from_rows(rows.into_iter().filter_map(|row| {
            Some((
                row.try_get::<String, _>(0).ok()?,
                row.try_get::<String, _>(1).ok()?,
                row.try_get::<Option<String>, _>(2).ok().flatten(),
                row.try_get::<Option<String>, _>(3)
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
            ))
        })))
    })
    .await?;

    let counts: Result<Vec<(String, String, i64)>, String> = query_timeout(async {
        let rows = sqlx::query(COUNTS_SQL)
            .fetch_all(pool)
            .await
            .map_err(sqlx_error)?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                Some((
                    row.try_get::<String, _>(0).ok()?,
                    row.try_get::<String, _>(1).ok()?,
                    row.try_get::<i64, _>(2).ok()?,
                ))
            })
            .collect())
    })
    .await;
    if let Ok(counts) = counts {
        apply_counts(&mut tables, counts);
    }
    Ok(tables)
}

async fn pg_run_query(
    pools: &mut HashMap<String, PgPool>,
    conn_str: &str,
    database: &str,
    sql: &str,
) -> Result<QueryRows, String> {
    let pool = get_pg_pool(pools, conn_str, database).await?;
    query_timeout(async {
        let rows = sqlx::query(sql).fetch_all(pool).await.map_err(sqlx_error)?;
        let columns = rows
            .first()
            .map(|row| {
                row.columns()
                    .iter()
                    .map(|c| ColMeta {
                        name: c.name().to_string(),
                        category: categorize_decl(c.type_info().name()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let rows = rows
            .iter()
            .map(|row| {
                row.columns()
                    .iter()
                    .enumerate()
                    .map(|(idx, col)| pg_value_to_string(row, idx, col.type_info().name()))
                    .collect()
            })
            .collect();
        Ok((columns, rows))
    })
    .await
}

fn pg_value_to_string(row: &PgRow, idx: usize, ty: &str) -> Option<String> {
    if row.try_get_raw(idx).ok()?.is_null() {
        return None;
    }
    let ty = ty.to_ascii_lowercase();
    match ty.as_str() {
        "bool" => row.try_get::<bool, _>(idx).ok().map(|v| v.to_string()),
        "int2" => row.try_get::<i16, _>(idx).ok().map(|v| v.to_string()),
        "int4" => row.try_get::<i32, _>(idx).ok().map(|v| v.to_string()),
        "int8" => row.try_get::<i64, _>(idx).ok().map(|v| v.to_string()),
        "float4" => row.try_get::<f32, _>(idx).ok().map(|v| v.to_string()),
        "float8" => row.try_get::<f64, _>(idx).ok().map(|v| v.to_string()),
        "numeric" => row
            .try_get::<sqlx::types::BigDecimal, _>(idx)
            .ok()
            .map(|v| v.to_string()),
        "uuid" => row
            .try_get::<sqlx::types::Uuid, _>(idx)
            .ok()
            .map(|v| v.to_string()),
        "date" => row
            .try_get::<chrono::NaiveDate, _>(idx)
            .ok()
            .map(|v| v.format("%Y-%m-%d").to_string()),
        "time" => row
            .try_get::<chrono::NaiveTime, _>(idx)
            .ok()
            .map(|v| v.format("%H:%M:%S").to_string()),
        "timestamp" => row
            .try_get::<chrono::NaiveDateTime, _>(idx)
            .ok()
            .map(|v| v.format("%Y-%m-%d, %H:%M:%S").to_string()),
        "timestamptz" => row
            .try_get::<chrono::DateTime<chrono::Utc>, _>(idx)
            .ok()
            .map(|v| v.format("%Y-%m-%d, %H:%M:%S UTC").to_string()),
        "bytea" => row.try_get::<Vec<u8>, _>(idx).ok().map(|v| hex_bytes(&v)),
        _ => row
            .try_get::<String, _>(idx)
            .ok()
            .or_else(|| row.try_get::<Vec<u8>, _>(idx).ok().map(|v| hex_bytes(&v))),
    }
}

// --- SQLite -----------------------------------------------------------------------

fn sqlite_options(conn_str: &str) -> Result<SqliteConnectOptions, String> {
    let raw = conn_str.trim();
    if raw.is_empty() {
        return Err("SQLite path is empty".into());
    }
    let opts = if raw.starts_with("sqlite:") {
        SqliteConnectOptions::from_str(raw).map_err(|e| e.to_string())?
    } else {
        SqliteConnectOptions::new().filename(raw)
    };
    Ok(opts.create_if_missing(true))
}

async fn get_sqlite_pool<'a>(
    pools: &'a mut HashMap<String, SqlitePool>,
    conn_str: &str,
) -> Result<&'a SqlitePool, String> {
    let key = conn_str.trim().to_string();
    if !pools.contains_key(&key) {
        let options = sqlite_options(conn_str)?;
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .acquire_timeout(CONNECT_TIMEOUT)
            .connect_with(options)
            .await
            .map_err(sqlx_error)?;
        pools.insert(key.clone(), pool);
    }
    Ok(pools.get(&key).unwrap())
}

async fn sqlite_test_connection(
    pools: &mut HashMap<String, SqlitePool>,
    conn_str: &str,
    _database: &str,
) -> Result<(), String> {
    pools.remove(conn_str.trim());
    let pool = get_sqlite_pool(pools, conn_str).await?;
    query_timeout(async {
        sqlx::query("SELECT 1")
            .execute(pool)
            .await
            .map(|_| ())
            .map_err(sqlx_error)
    })
    .await
}

async fn sqlite_get_schema(
    pools: &mut HashMap<String, SqlitePool>,
    conn_str: &str,
    _database: &str,
) -> Result<Vec<TableInfo>, String> {
    let pool = get_sqlite_pool(pools, conn_str).await?;
    let names: Vec<String> = query_timeout(async {
        let rows = sqlx::query(
            "SELECT name FROM sqlite_schema \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(pool)
        .await
        .map_err(sqlx_error)?;
        Ok(rows
            .into_iter()
            .filter_map(|row| row.try_get::<String, _>(0).ok())
            .collect())
    })
    .await?;

    let mut tables = Vec::with_capacity(names.len());
    for name in names {
        let pragma = format!("PRAGMA table_info({})", quote_ident(&name));
        let columns = query_timeout(async {
            let rows = sqlx::query(&pragma)
                .fetch_all(pool)
                .await
                .map_err(sqlx_error)?;
            Ok(rows
                .into_iter()
                .filter_map(|row| {
                    Some(ColumnInfo {
                        name: row.try_get::<String, _>("name").ok()?,
                        ty: row.try_get::<String, _>("type").unwrap_or_default(),
                    })
                })
                .collect::<Vec<_>>())
        })
        .await?;

        let count_sql = format!("SELECT COUNT(*) FROM {}", quote_ident(&name));
        let rows = query_timeout(async {
            sqlx::query(&count_sql)
                .fetch_one(pool)
                .await
                .map_err(sqlx_error)
                .and_then(|row| row.try_get::<i64, _>(0).map_err(sqlx_error))
        })
        .await
        .ok();

        tables.push(TableInfo {
            schema: "main".into(),
            name,
            rows,
            columns,
        });
    }
    Ok(tables)
}

async fn sqlite_run_query(
    pools: &mut HashMap<String, SqlitePool>,
    conn_str: &str,
    _database: &str,
    sql: &str,
) -> Result<QueryRows, String> {
    let pool = get_sqlite_pool(pools, conn_str).await?;
    query_timeout(async {
        let rows = sqlx::query(sql).fetch_all(pool).await.map_err(sqlx_error)?;
        let columns = rows
            .first()
            .map(|row| {
                row.columns()
                    .iter()
                    .map(|c| ColMeta {
                        name: c.name().to_string(),
                        category: categorize_decl(c.type_info().name()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let rows = rows
            .iter()
            .map(|row| {
                row.columns()
                    .iter()
                    .enumerate()
                    .map(|(idx, col)| sqlite_value_to_string(row, idx, col.type_info().name()))
                    .collect()
            })
            .collect();
        Ok((columns, rows))
    })
    .await
}

fn sqlite_value_to_string(row: &SqliteRow, idx: usize, ty: &str) -> Option<String> {
    if row.try_get_raw(idx).ok()?.is_null() {
        return None;
    }
    let ty = ty.to_ascii_lowercase();
    if ty.contains("int") || ty == "boolean" || ty == "bool" {
        return row.try_get::<i64, _>(idx).ok().map(|v| v.to_string());
    }
    if ty.contains("real") || ty.contains("floa") || ty.contains("doub") {
        return row.try_get::<f64, _>(idx).ok().map(|v| v.to_string());
    }
    if ty.contains("blob") {
        return row.try_get::<Vec<u8>, _>(idx).ok().map(|v| hex_bytes(&v));
    }
    row.try_get::<String, _>(idx)
        .ok()
        .or_else(|| row.try_get::<i64, _>(idx).ok().map(|v| v.to_string()))
        .or_else(|| row.try_get::<f64, _>(idx).ok().map(|v| v.to_string()))
        .or_else(|| row.try_get::<Vec<u8>, _>(idx).ok().map(|v| hex_bytes(&v)))
}

// --- Shared SQLx helpers -----------------------------------------------------------

async fn query_timeout<T>(
    fut: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    match tokio::time::timeout(QUERY_TIMEOUT, fut).await {
        Ok(result) => result,
        Err(_) => Err("Timeout: the query took more than 120s".into()),
    }
}

fn sqlx_error(e: sqlx::Error) -> String {
    e.to_string()
}

fn table_infos_from_rows(
    rows: impl IntoIterator<Item = (String, String, Option<String>, String)>,
) -> Vec<TableInfo> {
    let mut order: Vec<TableInfo> = vec![];
    let mut index: HashMap<String, usize> = HashMap::new();
    for (schema, name, col, ty) in rows {
        let key = format!("{schema}.{name}");
        let idx = *index.entry(key).or_insert_with(|| {
            order.push(TableInfo {
                schema,
                name,
                rows: None,
                columns: vec![],
            });
            order.len() - 1
        });
        if let Some(col) = col {
            order[idx].columns.push(ColumnInfo { name: col, ty });
        }
    }
    order
}

fn apply_counts(tables: &mut [TableInfo], counts: Vec<(String, String, i64)>) {
    for (schema, name, rows) in counts {
        if let Some(t) = tables
            .iter_mut()
            .find(|t| t.schema == schema && t.name == name)
        {
            t.rows = Some(rows);
        }
    }
}

fn categorize_decl(ty: &str) -> Category {
    let d = ty.to_ascii_lowercase();
    match d.as_str() {
        "bool" | "boolean" | "bit" => Category::Bool,
        "int2" | "int4" | "int8" | "integer" | "tinyint" | "smallint" | "int" | "bigint"
        | "serial" | "bigserial" | "decimal" | "numeric" | "float4" | "float8" | "real"
        | "double precision" | "double" | "money" => Category::Num,
        "uuid" => Category::Uuid,
        "date" | "time" | "timestamp" | "timestamptz" | "datetime" | "datetime2" => Category::Date,
        "bytea" | "blob" | "binary" | "varbinary" => Category::Bin,
        "char" | "varchar" | "text" | "bpchar" | "name" | "json" | "jsonb" | "xml" => Category::Str,
        _ if d.contains("char") || d.contains("text") => Category::Str,
        _ if d.contains("int") || d.contains("numeric") || d.contains("decimal") => Category::Num,
        _ if d.contains("date") || d.contains("time") => Category::Date,
        _ if d.contains("blob") || d.contains("binary") => Category::Bin,
        _ => Category::Other,
    }
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for byte in bytes {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;

    fn recv(rx: &Receiver<DbResponse>) -> DbResponse {
        rx.recv_timeout(Duration::from_secs(5))
            .expect("db worker response")
    }

    #[test]
    fn sqlite_worker_can_query_and_read_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.db");
        let conn = path.to_string_lossy().to_string();
        let (tx, rx) = spawn_worker();

        for sql in [
            "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN, payload BLOB)",
            "INSERT INTO people (name, active, payload) VALUES ('Ada', 1, X'0A0B')",
        ] {
            tx.send(DbRequest::Query {
                engine: "sqlite".into(),
                conn: conn.clone(),
                database: "main".into(),
                sql: sql.into(),
            })
            .unwrap();
            match recv(&rx) {
                DbResponse::Query(out) => assert_eq!(out.error, None),
                _ => panic!("expected query response"),
            }
        }

        tx.send(DbRequest::Schema {
            db_id: "db".into(),
            engine: "sqlite".into(),
            conn: conn.clone(),
            database: "main".into(),
        })
        .unwrap();
        let tables = match recv(&rx) {
            DbResponse::Schema { result, .. } => result.unwrap(),
            _ => panic!("expected schema response"),
        };
        let people = tables.iter().find(|table| table.name == "people").unwrap();
        assert_eq!(people.schema, "main");
        assert_eq!(people.rows, Some(1));
        assert!(people
            .columns
            .iter()
            .any(|col| col.name == "name" && col.ty.eq_ignore_ascii_case("TEXT")));

        tx.send(DbRequest::Query {
            engine: "sqlite".into(),
            conn,
            database: "main".into(),
            sql: "SELECT id, name, active, payload FROM people".into(),
        })
        .unwrap();
        let out = match recv(&rx) {
            DbResponse::Query(out) => out,
            _ => panic!("expected query response"),
        };
        assert_eq!(out.error, None);
        assert_eq!(
            out.columns
                .iter()
                .map(|col| col.name.as_str())
                .collect::<Vec<_>>(),
            ["id", "name", "active", "payload"]
        );
        assert_eq!(
            out.rows.first().cloned().unwrap(),
            vec![
                Some("1".into()),
                Some("Ada".into()),
                Some("1".into()),
                Some("0x0a0b".into())
            ]
        );
    }
}
