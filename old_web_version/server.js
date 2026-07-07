import express from 'express';
import mssql from 'mssql';
import { createServer as createViteServer } from 'vite';
import { readFileSync, writeFileSync, existsSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { homedir } from 'node:os';
import { exec } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const PORT = Number(process.env.PORT) || 5055;

// Config lives in the OS-standard per-user config dir, not in the repo.
//   Linux:   $XDG_CONFIG_HOME/querybench  (default ~/.config/querybench)
//   macOS:   ~/Library/Application Support/querybench
//   Windows: %APPDATA%\querybench
function configDir() {
  const app = 'querybench';
  if (process.platform === 'win32') {
    return join(process.env.APPDATA || join(homedir(), 'AppData', 'Roaming'), app);
  }
  if (process.platform === 'darwin') {
    return join(homedir(), 'Library', 'Application Support', app);
  }
  return join(process.env.XDG_CONFIG_HOME || join(homedir(), '.config'), app);
}

const CONFIG_DIR = configDir();
const SOURCES_PATH = join(CONFIG_DIR, 'config.json');
// Legacy locations migrated on first run: files that used to sit in the repo.
const LEGACY_SOURCES = join(__dirname, 'sources.json');
const LEGACY_PATH = join(__dirname, 'connections.json');

const newId = (prefix) => `${prefix}_${Math.random().toString(36).slice(2, 9)}`;

// ---------------------------------------------------------------------------
// Connection strings (ADO.NET style) → node-mssql config.
// ---------------------------------------------------------------------------
function parseDotNetConnectionString(raw) {
  const parts = {};
  for (const segment of raw.split(';')) {
    const idx = segment.indexOf('=');
    if (idx === -1) continue;
    const key = segment.slice(0, idx).trim().toLowerCase();
    const value = segment.slice(idx + 1).trim();
    if (key) parts[key] = value;
  }
  return parts;
}

function toMssqlConfig(raw, database) {
  const cs = parseDotNetConnectionString(raw);
  const serverRaw = cs['server'] ?? cs['data source'] ?? 'localhost';
  const user = cs['user id'] ?? cs['uid'] ?? cs['user'];
  const password = cs['password'] ?? cs['pwd'];
  const integrated = /^(true|sspi|yes)$/i.test(cs['integrated security'] ?? cs['trusted_connection'] ?? '');

  if (integrated && !user) {
    throw new Error(
      'Integrated Security (Windows-autentisering) stöds inte på den här plattformen. ' +
      'Ange User Id och Password i connection-stringen.'
    );
  }

  let host = serverRaw.replace(/^tcp:/i, '');
  let port = 1433;
  const commaIdx = host.indexOf(',');
  if (commaIdx !== -1) {
    port = Number(host.slice(commaIdx + 1).trim()) || 1433;
    host = host.slice(0, commaIdx).trim();
  }

  const config = {
    server: host,
    port,
    user,
    password,
    options: {
      trustServerCertificate: /^(true|yes)$/i.test(cs['trustservercertificate'] ?? cs['trust server certificate'] ?? 'true'),
      encrypt: /^(true|yes)$/i.test(cs['encrypt'] ?? 'false'),
      enableArithAbort: true
    },
    pool: { max: 5, min: 0, idleTimeoutMillis: 30000 },
    connectionTimeout: 8000,
    requestTimeout: 120000
  };

  const defaultDb = cs['database'] ?? cs['initial catalog'];
  if (database) config.database = database;
  else if (defaultDb) config.database = defaultDb;
  return config;
}

function redact(raw) {
  const cs = parseDotNetConnectionString(raw);
  return {
    server: cs['server'] ?? cs['data source'] ?? '',
    user: cs['user id'] ?? cs['uid'] ?? cs['user'] ?? '',
    database: cs['database'] ?? cs['initial catalog'] ?? ''
  };
}

function normalizeValue(value) {
  if (value === null || value === undefined) return null;
  if (Buffer.isBuffer(value)) return '0x' + value.toString('hex');
  return value;
}

function categorize(declaration) {
  const d = (declaration || '').toLowerCase();
  if (d === 'uniqueidentifier') return 'uuid';
  if (['char', 'nchar', 'varchar', 'nvarchar', 'text', 'ntext', 'xml', 'sysname'].includes(d)) return 'string';
  if (d === 'bit') return 'boolean';
  if (['tinyint', 'smallint', 'int', 'bigint', 'decimal', 'numeric', 'float', 'real', 'money', 'smallmoney'].includes(d)) return 'number';
  if (['date', 'datetime', 'datetime2', 'smalldatetime', 'datetimeoffset', 'time'].includes(d)) return 'date';
  if (['binary', 'varbinary', 'image', 'timestamp', 'rowversion'].includes(d)) return 'binary';
  return 'other';
}

// ---------------------------------------------------------------------------
// Connection pools, cached per (connection string + database).
// ---------------------------------------------------------------------------
const pools = new Map();

async function poolFor(connectionString, database) {
  const key = connectionString + '||' + (database || 'master');
  if (pools.has(key)) return pools.get(key);
  const pool = new mssql.ConnectionPool(toMssqlConfig(connectionString, database));
  const promise = pool.connect().then(() => pool).catch((err) => {
    pools.delete(key);
    throw err;
  });
  pools.set(key, promise);
  return promise;
}

// ---------------------------------------------------------------------------
// Engine adapters. Only mssql today; postgres/sqlite slot in here later
// without any change to the model, endpoints or UI.
// ---------------------------------------------------------------------------
const engines = {
  mssql: {
    async testConnection(connectionString) {
      const pool = new mssql.ConnectionPool(toMssqlConfig(connectionString));
      await pool.connect();
      await pool.close();
    },

    async listDatabases(connectionString) {
      const pool = await poolFor(connectionString, 'master');
      const result = await pool.request().query(`
        SELECT name FROM sys.databases
        WHERE name NOT IN ('master', 'model', 'msdb', 'tempdb') AND state = 0
        ORDER BY name`);
      return result.recordset.map((r) => r.name);
    },

    async getSchema(connectionString, database) {
      const pool = await poolFor(connectionString, database);
      const result = await pool.request().query(`
        SELECT t.TABLE_SCHEMA, t.TABLE_NAME, c.COLUMN_NAME, c.DATA_TYPE
        FROM INFORMATION_SCHEMA.TABLES t
        LEFT JOIN INFORMATION_SCHEMA.COLUMNS c
          ON c.TABLE_SCHEMA = t.TABLE_SCHEMA AND c.TABLE_NAME = t.TABLE_NAME
        WHERE t.TABLE_TYPE = 'BASE TABLE'
        ORDER BY t.TABLE_SCHEMA, t.TABLE_NAME, c.ORDINAL_POSITION`);

      const byTable = new Map();
      for (const row of result.recordset) {
        const key = `${row.TABLE_SCHEMA}.${row.TABLE_NAME}`;
        let table = byTable.get(key);
        if (!table) {
          table = { schema: row.TABLE_SCHEMA, name: row.TABLE_NAME, rows: null, columns: [] };
          byTable.set(key, table);
        }
        if (row.COLUMN_NAME) table.columns.push({ name: row.COLUMN_NAME, type: row.DATA_TYPE });
      }

      try {
        const counts = await pool.request().query(`
          SELECT s.name AS [schema], t.name AS [table], SUM(p.rows) AS [rows]
          FROM sys.tables t
          JOIN sys.schemas s ON s.schema_id = t.schema_id
          JOIN sys.partitions p ON p.object_id = t.object_id AND p.index_id IN (0, 1)
          GROUP BY s.name, t.name`);
        for (const c of counts.recordset) {
          const table = byTable.get(`${c.schema}.${c.table}`);
          if (table) table.rows = Number(c.rows);
        }
      } catch { /* row counts are best-effort */ }

      return [...byTable.values()];
    },

    async runQuery(connectionString, database, sql) {
      const pool = await poolFor(connectionString, database);
      const request = pool.request();
      request.arrayRowMode = true;
      const result = await request.query(sql);

      const recordset = Array.isArray(result.recordsets) ? result.recordsets[0] : result.recordset;
      const columnsMeta = recordset?.columns ?? result.columns?.[0] ?? [];
      const columns = (Array.isArray(columnsMeta) ? columnsMeta : Object.values(columnsMeta))
        .map((c) => ({ name: c.name, category: categorize(c.type?.declaration) }));
      const rows = (recordset ?? []).map((row) => row.map(normalizeValue));
      const rowsAffected = Array.isArray(result.rowsAffected)
        ? result.rowsAffected.reduce((a, b) => a + b, 0)
        : result.rowsAffected ?? 0;
      return { columns, rows, rowsAffected };
    }
  }
};

const engineFor = (name) => {
  const engine = engines[name];
  if (!engine) throw new Error(`Motorn "${name}" stöds inte ännu.`);
  return engine;
};

// ---------------------------------------------------------------------------
// Store: sources.json = { servers: [...], databases: [...] }
// server:   { id, name, engine, connectionString, databases: "all" | [names] }
// database: { id, name, engine, connectionString }   (standalone / serverless)
// ---------------------------------------------------------------------------
function writeStore(data) {
  mkdirSync(CONFIG_DIR, { recursive: true });
  writeFileSync(SOURCES_PATH, JSON.stringify(data, null, 2));
}

function loadStore() {
  if (existsSync(SOURCES_PATH)) {
    try {
      const parsed = JSON.parse(readFileSync(SOURCES_PATH, 'utf8'));
      return {
        servers: Array.isArray(parsed.servers) ? parsed.servers : [],
        databases: Array.isArray(parsed.databases) ? parsed.databases : []
      };
    } catch { /* fall through */ }
  }

  // Migrate a sources.json that used to live in the repo folder.
  if (existsSync(LEGACY_SOURCES)) {
    try {
      const parsed = JSON.parse(readFileSync(LEGACY_SOURCES, 'utf8'));
      const migrated = {
        servers: Array.isArray(parsed.servers) ? parsed.servers : [],
        databases: Array.isArray(parsed.databases) ? parsed.databases : []
      };
      writeStore(migrated);
      console.log(`Migrated sources.json → ${SOURCES_PATH}`);
      return migrated;
    } catch { /* fall through */ }
  }

  // Migrate the old single-connection format.
  if (existsSync(LEGACY_PATH)) {
    try {
      const old = JSON.parse(readFileSync(LEGACY_PATH, 'utf8'));
      if (Array.isArray(old.connections) && old.connections.length) {
        const servers = old.connections.map((c) => ({
          id: newId('srv'),
          name: c.name,
          engine: 'mssql',
          connectionString: c.connectionString,
          databases: 'all'
        }));
        const migrated = { servers, databases: [] };
        writeStore(migrated);
        console.log(`Migrated ${servers.length} connection(s) from connections.json → ${SOURCES_PATH}`);
        return migrated;
      }
    } catch { /* ignore */ }
  }

  const seed = process.env.QUERYBENCH_CONNECTION || process.env.SQLCONSOLE_CONNECTION;
  if (seed) {
    return {
      servers: [{ id: newId('srv'), name: 'Default', engine: 'mssql', connectionString: seed, databases: 'all' }],
      databases: []
    };
  }
  return { servers: [], databases: [] };
}

let store = loadStore();

function saveStore() {
  writeStore(store);
}

const findServer = (id) => store.servers.find((s) => s.id === id);
const findDatabase = (id) => store.databases.find((d) => d.id === id);

// Resolve a database id from the flat list back to { engine, connectionString, database }.
function resolveDb(dbId) {
  if (dbId?.startsWith('s:')) {
    const rest = dbId.slice(2);
    const sep = rest.indexOf(':');
    const serverId = rest.slice(0, sep);
    const database = rest.slice(sep + 1);
    const server = findServer(serverId);
    if (!server) throw new Error('Okänd server för databasen.');
    return { engine: server.engine, connectionString: server.connectionString, database };
  }
  if (dbId?.startsWith('d:')) {
    const db = findDatabase(dbId.slice(2));
    if (!db) throw new Error('Okänd databas.');
    return { engine: db.engine, connectionString: db.connectionString, database: redact(db.connectionString).database || db.name };
  }
  throw new Error('Ogiltigt databas-id.');
}

// The flat list of every queryable database across all sources.
async function listAllDatabases() {
  const out = [];
  const warnings = [];

  for (const server of store.servers) {
    if (!engines[server.engine]) {
      warnings.push(`${server.name}: motorn "${server.engine}" stöds inte ännu`);
      continue;
    }
    let names = [];
    if (server.databases === 'all') {
      try {
        names = await engineFor(server.engine).listDatabases(server.connectionString);
      } catch (err) {
        warnings.push(`${server.name}: ${err.message}`);
      }
    } else if (Array.isArray(server.databases)) {
      names = server.databases;
    }
    for (const name of names) {
      out.push({ id: `s:${server.id}:${name}`, name, engine: server.engine, server: server.name });
    }
  }

  for (const db of store.databases) {
    out.push({ id: `d:${db.id}`, name: db.name, engine: db.engine, server: null });
  }

  // Disambiguate only on name collisions.
  const counts = {};
  for (const e of out) counts[e.name] = (counts[e.name] || 0) + 1;
  for (const e of out) {
    e.label = counts[e.name] > 1 && e.server ? `${e.name} (${e.server})` : e.name;
  }
  out.sort((a, b) => a.label.localeCompare(b.label, 'sv'));
  return { databases: out, warnings };
}

// ---------------------------------------------------------------------------
// API
// ---------------------------------------------------------------------------
const api = express.Router();
api.use(express.json({ limit: '5mb' }));

// Flat database list for the sidebar.
api.get('/databases', async (_req, res) => {
  try {
    res.json(await listAllDatabases());
  } catch (err) {
    res.status(500).json({ error: err.message });
  }
});

// Full source config for the management window (no passwords).
api.get('/sources', (_req, res) => {
  res.json({
    servers: store.servers.map((s) => ({
      id: s.id, name: s.name, engine: s.engine, ...redact(s.connectionString),
      databases: s.databases,
      databaseCount: Array.isArray(s.databases) ? s.databases.length : null
    })),
    databases: store.databases.map((d) => ({
      id: d.id, name: d.name, engine: d.engine, ...redact(d.connectionString || '')
    }))
  });
});

// Test a server connection string and return its databases (for the picker).
api.post('/servers/preview', async (req, res) => {
  const { connectionString, engine = 'mssql' } = req.body ?? {};
  if (!connectionString) return res.status(400).json({ error: 'connectionString krävs' });
  try {
    const names = await engineFor(engine).listDatabases(connectionString);
    res.json({ databases: names });
  } catch (err) {
    res.status(400).json({ error: 'Kunde inte ansluta: ' + err.message });
  }
});

// Add a server (+ its selected databases, "all" or a name list).
api.post('/servers', async (req, res) => {
  const { name, connectionString, engine = 'mssql', databases = 'all' } = req.body ?? {};
  if (!name || !connectionString) return res.status(400).json({ error: 'name och connectionString krävs' });
  try {
    await engineFor(engine).testConnection(connectionString);
  } catch (err) {
    return res.status(400).json({ error: 'Kunde inte ansluta: ' + err.message });
  }
  const dbs = databases === 'all' ? 'all' : (Array.isArray(databases) ? databases : 'all');
  store.servers.push({ id: newId('srv'), name, engine, connectionString, databases: dbs });
  saveStore();
  res.json({ ok: true });
});

api.patch('/servers/:id', async (req, res) => {
  const server = findServer(req.params.id);
  if (!server) return res.status(404).json({ error: 'Okänd server' });
  const { name, connectionString, databases } = req.body ?? {};
  if (connectionString) {
    try {
      await engineFor(server.engine).testConnection(connectionString);
    } catch (err) {
      return res.status(400).json({ error: 'Kunde inte ansluta: ' + err.message });
    }
    server.connectionString = connectionString;
  }
  if (name) server.name = name;
  if (databases !== undefined) {
    server.databases = databases === 'all' ? 'all' : (Array.isArray(databases) ? databases : server.databases);
  }
  saveStore();
  res.json({ ok: true });
});

api.delete('/servers/:id', (req, res) => {
  store.servers = store.servers.filter((s) => s.id !== req.params.id);
  saveStore();
  res.json({ ok: true });
});

// Add a standalone database (its own connection string).
api.post('/databases', async (req, res) => {
  const { name, connectionString, engine = 'mssql' } = req.body ?? {};
  if (!name || !connectionString) return res.status(400).json({ error: 'name och connectionString krävs' });
  try {
    await engineFor(engine).testConnection(connectionString);
  } catch (err) {
    return res.status(400).json({ error: 'Kunde inte ansluta: ' + err.message });
  }
  store.databases.push({ id: newId('db'), name, engine, connectionString });
  saveStore();
  res.json({ ok: true });
});

api.patch('/databases/:id', async (req, res) => {
  const db = findDatabase(req.params.id);
  if (!db) return res.status(404).json({ error: 'Okänd databas' });
  const { name, connectionString } = req.body ?? {};
  if (connectionString) {
    try {
      await engineFor(db.engine).testConnection(connectionString);
    } catch (err) {
      return res.status(400).json({ error: 'Kunde inte ansluta: ' + err.message });
    }
    db.connectionString = connectionString;
  }
  if (name) db.name = name;
  saveStore();
  res.json({ ok: true });
});

api.delete('/databases/:id', (req, res) => {
  store.databases = store.databases.filter((d) => d.id !== req.params.id);
  saveStore();
  res.json({ ok: true });
});

// Schema + query for a specific database (by flat-list id).
api.get('/schema', async (req, res) => {
  const dbId = req.query.dbId;
  if (!dbId) return res.status(400).json({ error: 'Missing dbId' });
  try {
    const { engine, connectionString, database } = resolveDb(dbId);
    res.json({ tables: await engineFor(engine).getSchema(connectionString, database) });
  } catch (err) {
    res.status(500).json({ error: err.message });
  }
});

api.post('/query', async (req, res) => {
  const { dbId, sql } = req.body ?? {};
  if (!dbId || !sql) return res.status(400).json({ error: 'Missing dbId or sql' });
  const started = Date.now();
  try {
    const { engine, connectionString, database } = resolveDb(dbId);
    const result = await engineFor(engine).runQuery(connectionString, database, sql);
    res.json({ ...result, elapsedMs: Date.now() - started, error: null });
  } catch (err) {
    res.json({ columns: [], rows: [], rowsAffected: -1, elapsedMs: Date.now() - started, error: err.message });
  }
});

// ---------------------------------------------------------------------------
// Server: API + Vite dev middleware (one command, no separate build).
// ---------------------------------------------------------------------------
function openBrowser(url) {
  const commands = ['explorer.exe', 'wslview', 'xdg-open', 'open'];
  const tryNext = (i) => {
    if (i >= commands.length) return;
    exec(`${commands[i]} "${url}"`, (err) => { if (err) tryNext(i + 1); });
  };
  tryNext(0);
}

const app = express();
app.use('/api', api);

const vite = await createViteServer({
  root: __dirname,
  configFile: false,
  appType: 'spa',
  server: { middlewareMode: true, hmr: false, watch: null }
});
app.use(vite.middlewares);

app.listen(PORT, '127.0.0.1', () => {
  const url = `http://localhost:${PORT}`;
  console.log(`\n  querybench running at ${url}`);
  console.log(`  config: ${SOURCES_PATH}`);
  console.log(`  ${store.servers.length} server(s), ${store.databases.length} standalone database(s)\n`);
  openBrowser(url);
});
