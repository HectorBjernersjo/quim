// DB worker: a dedicated thread owning a tokio runtime and a cache of tiberius
// connections, fed requests over an mpsc channel. Keeps the UI thread free.
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use tiberius::{Client, ColumnData, ColumnType, Config, FromSql};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

pub type Conn = Client<Compat<TcpStream>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const QUERY_TIMEOUT: Duration = Duration::from_secs(120);

pub enum DbRequest {
    ListDatabases { server_id: String, conn: String },
    Schema { db_id: String, conn: String, database: String },
    Query { conn: String, database: String, sql: String },
    /// Connect and run SELECT 1; used by the source editor before saving.
    TestConnection { token: String, conn: String, database: String },
}

pub enum DbResponse {
    Databases { server_id: String, result: Result<Vec<String>, String> },
    Schema { db_id: String, result: Result<Vec<TableInfo>, String> },
    Query(QueryOutcome),
    TestResult { token: String, result: Result<(), String> },
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

pub fn spawn_worker() -> (Sender<DbRequest>, Receiver<DbResponse>) {
    let (req_tx, req_rx) = channel::<DbRequest>();
    let (resp_tx, resp_rx) = channel::<DbResponse>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let mut pool: HashMap<String, Conn> = HashMap::new();
        while let Ok(req) = req_rx.recv() {
            let resp = rt.block_on(handle(&mut pool, req));
            if resp_tx.send(resp).is_err() {
                break;
            }
        }
    });
    (req_tx, resp_rx)
}

async fn handle(pool: &mut HashMap<String, Conn>, req: DbRequest) -> DbResponse {
    match req {
        DbRequest::ListDatabases { server_id, conn } => DbResponse::Databases {
            server_id,
            result: list_databases(pool, &conn).await,
        },
        DbRequest::Schema { db_id, conn, database } => DbResponse::Schema {
            db_id,
            result: get_schema(pool, &conn, &database).await,
        },
        DbRequest::Query { conn, database, sql } => {
            let started = Instant::now();
            let result = run_query(pool, &conn, &database, &sql).await;
            let elapsed_ms = started.elapsed().as_millis();
            DbResponse::Query(match result {
                Ok((columns, rows)) => QueryOutcome { columns, rows, error: None, elapsed_ms },
                Err(e) => QueryOutcome { columns: vec![], rows: vec![], error: Some(e), elapsed_ms },
            })
        }
        DbRequest::TestConnection { token, conn, database } => DbResponse::TestResult {
            token,
            result: test_connection(pool, &conn, &database).await,
        },
    }
}

fn pool_key(conn_str: &str, database: &str) -> String {
    format!("{conn_str}||{database}")
}

async fn connect(conn_str: &str, database: &str) -> Result<Conn, tiberius::error::Error> {
    let mut config = Config::from_ado_string(conn_str)?;
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
        let client = match tokio::time::timeout(CONNECT_TIMEOUT, connect(conn_str, database)).await {
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

async fn test_connection(
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

async fn list_databases(pool: &mut HashMap<String, Conn>, conn_str: &str) -> Result<Vec<String>, String> {
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

async fn get_schema(
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

    // Row counts are best-effort, like in querybench.
    let counts: Result<Vec<(String, String, i64)>, String> =
        with_conn_retry!(pool, conn_str, database, conn => async {
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
            if let Some(t) = tables.iter_mut().find(|t| t.schema == schema && t.name == name) {
                t.rows = Some(rows);
            }
        }
    }
    Ok(tables)
}

type QueryRows = (Vec<ColMeta>, Vec<Vec<Option<String>>>);

async fn run_query(
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
        d @ (DateTime(_) | SmallDateTime(_) | DateTime2(_)) => {
            chrono::NaiveDateTime::from_sql(&d)
                .ok()
                .flatten()
                .map(|dt| dt.format("%Y-%m-%d, %H:%M:%S").to_string())
        }
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
