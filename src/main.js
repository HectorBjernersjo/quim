import './style.css';
import { EditorView, keymap, highlightActiveLine, drawSelection, dropCursor, lineNumbers } from '@codemirror/view';
import { EditorState, Compartment, Prec } from '@codemirror/state';
import { history, defaultKeymap, historyKeymap, indentWithTab } from '@codemirror/commands';
import {
  autocompletion, completionKeymap, closeBrackets, closeBracketsKeymap
} from '@codemirror/autocomplete';
import {
  bracketMatching, indentOnInput, syntaxHighlighting, HighlightStyle, foldGutter, foldKeymap
} from '@codemirror/language';
import { tags as t } from '@lezer/highlight';
import { sql, MSSQL } from '@codemirror/lang-sql';
import { json } from '@codemirror/lang-json';

const $ = (id) => document.getElementById(id);

const dbFilter = $('db-filter');
const dbListEl = $('db-list');
const tableFilter = $('table-filter');
const tableList = $('table-list');
const runBtn = $('run-btn');
const statusEl = $('status');
const resultsEl = $('results');
const breadcrumbEl = $('breadcrumb');
const drawerEl = $('cell-drawer');
const drawerTitleEl = drawerEl.querySelector('.drawer-title');
const drawerJsonEl = $('drawer-json');
const drawerTextEl = $('drawer-text');
const appEl = $('app');

let allDatabases = [];   // [{ id, name, label, engine, server }]
let currentTables = [];  // [{ schema, name, rows, columns }]
let activeDbId = null;
let activeEngine = 'mssql';

const TABLE_ICON =
  `<svg class="tbl-icon" width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3">
     <rect x="1.5" y="2.5" width="13" height="11" rx="1"/><line x1="1.5" y1="6" x2="14.5" y2="6"/>
     <line x1="6" y1="6" x2="6" y2="13.5"/><line x1="10.5" y1="6" x2="10.5" y2="13.5"/>
   </svg>`;
const DB_ICON =
  `<svg class="tbl-icon" width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3">
     <ellipse cx="8" cy="3.5" rx="5.5" ry="2"/><path d="M2.5 3.5v9c0 1.1 2.5 2 5.5 2s5.5-.9 5.5-2v-9"/>
     <path d="M2.5 8c0 1.1 2.5 2 5.5 2s5.5-.9 5.5-2"/>
   </svg>`;

// --- Editor -----------------------------------------------------------------
const highlightStyle = HighlightStyle.define([
  { tag: t.keyword, color: '#c678f0' },
  { tag: [t.string, t.special(t.string), t.regexp], color: '#8fd98f' },
  { tag: t.number, color: '#d8a05a' },
  { tag: [t.bool, t.null], color: '#d8a05a' },
  { tag: t.comment, color: '#5c5c6b', fontStyle: 'italic' },
  { tag: [t.operator, t.punctuation, t.separator], color: '#8a8a9a' },
  { tag: [t.propertyName, t.name], color: '#d4d4dc' },
  { tag: t.typeName, color: '#56c8b5' },
  { tag: t.variableName, color: '#d4d4dc' }
]);

const editorTheme = EditorView.theme({
  '&': { height: '100%', backgroundColor: 'transparent', color: '#d4d4dc' },
  '.cm-scroller': { fontFamily: 'var(--mono)', fontSize: '13.5px', lineHeight: '1.6' },
  '.cm-content': { padding: '16px 0', caretColor: '#b6f24a' },
  '.cm-cursor, .cm-dropCursor': { borderLeftColor: '#b6f24a' },
  '&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection': {
    backgroundColor: 'rgba(182, 242, 74, 0.14)'
  },
  '.cm-activeLine': { backgroundColor: 'rgba(255,255,255,0.02)' },
  '.cm-gutters': { display: 'none' },
  '.cm-tooltip': {
    backgroundColor: '#17171b', border: '1px solid #2a2a33', borderRadius: '6px',
    fontFamily: 'var(--mono)', fontSize: '12.5px'
  },
  '.cm-tooltip-autocomplete ul li[aria-selected]': { backgroundColor: 'rgba(182,242,74,0.15)', color: '#eaeaf0' }
}, { dark: true });

const editorSetup = [
  history(), drawSelection(), dropCursor(), indentOnInput(), bracketMatching(),
  closeBrackets(), autocompletion(), highlightActiveLine(),
  syntaxHighlighting(highlightStyle), editorTheme,
  keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...historyKeymap, ...completionKeymap, indentWithTab])
];

const sqlCompartment = new Compartment();

function buildSqlSchema(tables) {
  const schema = {};
  for (const tbl of tables) {
    const cols = tbl.columns.map((c) => c.name);
    schema[tbl.name] = cols;
    schema[`${tbl.schema}.${tbl.name}`] = cols;
  }
  return schema;
}

const sqlExtension = (tables) => sql({ dialect: MSSQL, schema: buildSqlSchema(tables), upperCaseKeywords: true });

const runKeymap = Prec.highest(
  keymap.of([{ key: 'Mod-Enter', preventDefault: true, run: () => { runQuery(); return true; } }])
);

const editor = new EditorView({
  parent: $('editor'),
  state: EditorState.create({
    doc: 'SELECT 1',
    extensions: [runKeymap, editorSetup, sqlCompartment.of(sqlExtension([]))]
  })
});

const setEditorContent = (text) =>
  editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: text } });

// --- Cell detail drawer -----------------------------------------------------
const jsonHighlightStyle = HighlightStyle.define([
  { tag: t.propertyName, color: '#56c8b5' },
  { tag: [t.string, t.special(t.string)], color: '#d8a05a' },
  { tag: t.number, color: '#7fb3e8' },
  { tag: [t.bool, t.null], color: '#b18fd9' },
  { tag: [t.punctuation, t.separator, t.brace, t.bracket], color: '#8a8a9a' }
]);

const jsonViewerTheme = EditorView.theme({
  '&': { height: '100%', backgroundColor: 'transparent', color: '#d4d4dc' },
  '.cm-scroller': { fontFamily: 'var(--mono)', fontSize: '12.5px', lineHeight: '1.55' },
  '.cm-gutters': { backgroundColor: 'transparent', border: 'none', color: '#3a3a44' },
  '.cm-activeLine, .cm-activeLineGutter': { backgroundColor: 'transparent' }
}, { dark: true });

const jsonViewer = new EditorView({
  parent: drawerJsonEl,
  state: EditorState.create({
    doc: '',
    extensions: [
      lineNumbers(), foldGutter(), json(),
      syntaxHighlighting(jsonHighlightStyle), jsonViewerTheme,
      EditorState.readOnly.of(true), EditorView.editable.of(false),
      keymap.of(foldKeymap)
    ]
  })
});

let drawerCopyValue = '';

function tryParseJson(value) {
  if (typeof value !== 'string') return undefined;
  const trimmed = value.trim();
  if (!trimmed || !(trimmed[0] === '{' || trimmed[0] === '[')) return undefined;
  try { return JSON.parse(trimmed); } catch { return undefined; }
}

function openCell(name, category, value) {
  drawerTitleEl.textContent = name;
  appEl.classList.add('drawer-open');
  const parsed = value === null ? undefined : tryParseJson(value);
  if (parsed !== undefined) {
    const pretty = JSON.stringify(parsed, null, 2);
    drawerCopyValue = pretty;
    jsonViewer.dispatch({ changes: { from: 0, to: jsonViewer.state.doc.length, insert: pretty } });
    drawerJsonEl.style.display = '';
    drawerTextEl.style.display = 'none';
  } else {
    const text = value === null ? 'NULL' : String(value);
    drawerCopyValue = value === null ? '' : text;
    drawerTextEl.textContent = text;
    drawerTextEl.className = value === null ? 'null' : '';
    drawerJsonEl.style.display = 'none';
    drawerTextEl.style.display = '';
  }
}

function closeDrawer() {
  appEl.classList.remove('drawer-open');
  document.querySelectorAll('#results td.selected').forEach((el) => el.classList.remove('selected'));
}

$('drawer-close').addEventListener('click', closeDrawer);
$('drawer-copy').addEventListener('click', async () => {
  try {
    await navigator.clipboard.writeText(drawerCopyValue);
    const btn = $('drawer-copy');
    btn.textContent = 'Copied';
    setTimeout(() => { btn.textContent = 'Copy'; }, 1200);
  } catch { /* clipboard unavailable */ }
});

// --- Databases (sidebar list) ----------------------------------------------
async function loadDatabaseList() {
  const res = await fetch('/api/databases');
  const data = await res.json();
  if (data.error) { setStatus(`✕ ${data.error}`, 'error'); return; }
  allDatabases = data.databases || [];
  if (data.warnings?.length) setStatus(`⚠ ${data.warnings.join(' · ')}`, 'error');
  renderDbList();

  // Keep or pick a selection.
  if (activeDbId && !allDatabases.some((d) => d.id === activeDbId)) activeDbId = null;
  if (!activeDbId && allDatabases.length) selectDatabase(allDatabases[0]);
  else if (!allDatabases.length) { currentTables = []; tableList.innerHTML = ''; }
}

function renderDbList() {
  const filter = dbFilter.value.trim().toLowerCase();
  dbListEl.innerHTML = '';
  const items = filter ? allDatabases.filter((d) => d.label.toLowerCase().includes(filter)) : allDatabases;
  for (const db of items) {
    const li = document.createElement('li');
    li.className = 'db-item' + (db.id === activeDbId ? ' active' : '');
    li.innerHTML = `${DB_ICON}<span class="db-name">${db.label}</span>`;
    li.title = db.label;
    li.addEventListener('click', () => selectDatabase(db));
    dbListEl.appendChild(li);
  }
}

function selectDatabase(db) {
  activeDbId = db.id;
  activeEngine = db.engine;
  renderDbList();
  breadcrumbEl.textContent = db.label;
  loadSchema(db.id);
}

async function loadSchema(dbId) {
  setStatus('Laddar schema…');
  tableList.innerHTML = '';
  const res = await fetch(`/api/schema?dbId=${encodeURIComponent(dbId)}`);
  const data = await res.json();
  if (data.error) { setStatus(`✕ ${data.error}`, 'error'); return; }
  currentTables = data.tables;
  editor.dispatch({ effects: sqlCompartment.reconfigure(sqlExtension(currentTables)) });
  renderTableList();
  setStatus(`${currentTables.length} tabeller`);
}

// --- Tables -----------------------------------------------------------------
const formatCount = (n) => (n == null ? '' : n.toLocaleString('sv-SE'));

function previewQuery(engine, schema, table) {
  if (engine === 'mssql') return `SELECT TOP 100 *\nFROM [${schema}].[${table}]`;
  return `SELECT *\nFROM "${schema}"."${table}"\nLIMIT 100`;
}

function renderTableList() {
  const filter = tableFilter.value.trim().toLowerCase();
  tableList.innerHTML = '';
  const tables = filter
    ? currentTables.filter((tbl) => `${tbl.schema}.${tbl.name}`.toLowerCase().includes(filter))
    : currentTables;

  for (const table of tables) {
    const li = document.createElement('li');
    li.className = 'table-item';
    li.title = `${table.schema}.${table.name} — ${table.columns.length} kolumner, ${formatCount(table.rows)} rader`;
    li.innerHTML =
      `${TABLE_ICON}<span class="tbl-name">${table.schema}.${table.name}</span>` +
      `<span class="tbl-count">${formatCount(table.rows)}</span>`;
    li.addEventListener('click', () => {
      setEditorContent(previewQuery(activeEngine, table.schema, table.name));
      runQuery();
    });
    tableList.appendChild(li);
  }
}

// --- Query execution --------------------------------------------------------
async function runQuery() {
  const sqlText = editor.state.doc.toString().trim();
  if (!activeDbId || !sqlText) return;
  setStatus('Kör…');
  runBtn.disabled = true;
  try {
    const res = await fetch('/api/query', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ dbId: activeDbId, sql: sqlText })
    });
    renderResults(await res.json());
  } catch (err) {
    setStatus(`✕ ${err.message}`, 'error');
  } finally {
    runBtn.disabled = false;
  }
}

function formatCell(value, category) {
  if (value === null) return { text: 'NULL', cls: 'v-null' };
  if (category === 'string') return { text: `"${value}"`, cls: 'v-string' };
  if (category === 'uuid') return { text: String(value), cls: 'v-uuid' };
  if (category === 'date') return { text: formatDate(value), cls: 'v-date' };
  if (category === 'number') return { text: String(value), cls: 'v-number' };
  if (category === 'boolean') return { text: value ? 'true' : 'false', cls: 'v-bool' };
  if (category === 'binary') return { text: String(value), cls: 'v-binary' };
  return { text: String(value), cls: 'v-other' };
}

function formatDate(value) {
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return String(value);
  const pad = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}, ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function renderResults(data) {
  resultsEl.innerHTML = '';
  if (data.error) {
    setStatus(`✕ ${data.elapsedMs} ms`, 'error');
    const pre = document.createElement('pre');
    pre.className = 'error';
    pre.textContent = data.error;
    resultsEl.appendChild(pre);
    return;
  }
  if (!data.columns.length) {
    setStatus(`✓ ${data.rowsAffected} rad(er) påverkade · ${data.elapsedMs}ms`, 'ok');
    return;
  }
  setStatus(`✓ ${data.rows.length} rows · ${data.elapsedMs}ms`, 'ok');

  const table = document.createElement('table');
  const thead = document.createElement('thead');
  const headRow = document.createElement('tr');
  headRow.appendChild(document.createElement('th')).className = 'rownum';
  for (const col of data.columns) {
    const th = document.createElement('th');
    th.textContent = col.name;
    headRow.appendChild(th);
  }
  thead.appendChild(headRow);
  table.appendChild(thead);

  const tbody = document.createElement('tbody');
  data.rows.forEach((row, i) => {
    const tr = document.createElement('tr');
    const numTd = document.createElement('td');
    numTd.className = 'rownum';
    numTd.textContent = String(i + 1);
    tr.appendChild(numTd);
    row.forEach((value, c) => {
      const col = data.columns[c];
      const td = document.createElement('td');
      const { text, cls } = formatCell(value, col.category);
      td.className = cls;
      td.textContent = text;
      if (tryParseJson(value) !== undefined) td.classList.add('is-json');
      td.addEventListener('click', () => {
        document.querySelectorAll('#results td.selected').forEach((el) => el.classList.remove('selected'));
        td.classList.add('selected');
        openCell(col.name, col.category, value);
      });
      tr.appendChild(td);
    });
    tbody.appendChild(tr);
  });
  table.appendChild(tbody);
  resultsEl.appendChild(table);
}

function setStatus(text, kind = '') {
  statusEl.textContent = text;
  statusEl.className = 'results-status' + (kind ? ' ' + kind : '');
}

// --- Resizable panels -------------------------------------------------------
const rootStyle = document.documentElement.style;
const SIZES_KEY = 'sqlconsole.sizes';
const clamp = (v, min, max) => Math.max(min, Math.min(max, v));

function loadSizes() {
  try {
    const s = JSON.parse(localStorage.getItem(SIZES_KEY) || '{}');
    if (s.sidebar) rootStyle.setProperty('--sidebar-w', s.sidebar + 'px');
    if (s.drawer) rootStyle.setProperty('--drawer-w', s.drawer + 'px');
    if (s.editor) rootStyle.setProperty('--editor-h', s.editor + 'px');
  } catch { /* ignore */ }
}

function saveSizes() {
  const prev = JSON.parse(localStorage.getItem(SIZES_KEY) || '{}');
  const drawerW = Math.round(drawerEl.getBoundingClientRect().width);
  localStorage.setItem(SIZES_KEY, JSON.stringify({
    sidebar: Math.round($('sidebar').getBoundingClientRect().width),
    editor: Math.round($('editor').getBoundingClientRect().height),
    drawer: drawerW > 10 ? drawerW : prev.drawer
  }));
}

function makeResizer(el, axis, compute) {
  el.addEventListener('pointerdown', (e) => {
    e.preventDefault();
    document.body.classList.add('resizing', axis);
    const move = (ev) => compute(ev);
    const up = () => {
      document.body.classList.remove('resizing', axis);
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', up);
      saveSizes();
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    window.addEventListener('pointercancel', up);
  });
}

makeResizer($('resize-sidebar'), 'col', (e) => {
  rootStyle.setProperty('--sidebar-w', clamp(e.clientX, 160, Math.min(700, window.innerWidth - 320)) + 'px');
});
makeResizer($('resize-drawer'), 'col', (e) => {
  rootStyle.setProperty('--drawer-w', clamp(window.innerWidth - e.clientX, 280, Math.min(1000, window.innerWidth - 380)) + 'px');
});
makeResizer($('resize-editor'), 'row', (e) => {
  const top = $('editor').getBoundingClientRect().top;
  rootStyle.setProperty('--editor-h', clamp(e.clientY - top, 70, window.innerHeight - 160) + 'px');
});

// --- Config / management window ---------------------------------------------
const configModal = $('config-modal');

function showConfigView(name) {
  for (const v of ['config-list', 'server-view', 'db-view']) {
    $(v).classList.toggle('hidden', v !== name);
  }
}

function isOnboarding() {
  return configModal.classList.contains('onboarding') && !configModal.classList.contains('hidden');
}

async function openConfig(onboard = false) {
  configModal.classList.toggle('onboarding', onboard);
  $('config-close').style.display = onboard ? 'none' : '';
  $('config-title').textContent = onboard ? 'Kom igång' : 'Databaser & servrar';
  await renderSources();
  showConfigView('config-list');
  configModal.classList.remove('hidden');
}

function closeConfig() {
  configModal.classList.add('hidden');
}

async function renderSources() {
  const res = await fetch('/api/sources');
  const data = await res.json();
  const empty = !data.servers.length && !data.databases.length;
  $('config-empty').style.display = empty ? '' : 'none';

  const dbList = $('db-manage-list');
  dbList.innerHTML = '';
  for (const db of data.databases) {
    dbList.appendChild(manageRow(
      `${DB_ICON}<span>${db.name}</span><span class="mng-meta">${db.server || db.engine}</span>`,
      { onEdit: () => openDbView('edit', db), onDelete: () => removeSource('databases', db.id) }));
  }
  if (!data.databases.length) dbList.appendChild(emptyRow('Inga fristående databaser'));

  const srvList = $('server-manage-list');
  srvList.innerHTML = '';
  for (const s of data.servers) {
    const dbInfo = s.databases === 'all' ? 'alla databaser' : `${s.databaseCount} databas(er)`;
    srvList.appendChild(manageRow(
      `<span>${s.name}</span><span class="mng-meta">${s.server} · ${dbInfo}</span>`,
      { onEdit: () => openServerView('edit', s), onDelete: () => removeSource('servers', s.id) }));
  }
  if (!data.servers.length) srvList.appendChild(emptyRow('Inga servrar'));
}

function manageRow(html, { onEdit, onDelete }) {
  const li = document.createElement('li');
  li.className = 'manage-item';
  li.innerHTML = `<div class="mng-main">${html}</div>`;
  const actions = document.createElement('div');
  actions.className = 'mng-actions';
  if (onEdit) {
    const edit = document.createElement('button');
    edit.className = 'mng-btn';
    edit.textContent = '✎';
    edit.title = 'Redigera';
    edit.addEventListener('click', onEdit);
    actions.appendChild(edit);
  }
  const del = document.createElement('button');
  del.className = 'mng-btn mng-del';
  del.textContent = '🗑';
  del.title = 'Ta bort';
  del.addEventListener('click', onDelete);
  actions.appendChild(del);
  li.appendChild(actions);
  return li;
}

function emptyRow(text) {
  const li = document.createElement('li');
  li.className = 'manage-item empty';
  li.textContent = text;
  return li;
}

async function removeSource(kind, id) {
  if (!confirm('Ta bort den här posten?')) return;
  await fetch(`/api/${kind}/${encodeURIComponent(id)}`, { method: 'DELETE' });
  await renderSources();
  await loadDatabaseList();
}

// Add / edit server flow
let discoveredDbs = [];
let editState = null; // null | { type: 'server'|'db', id }
const SF_CS_PLACEHOLDER = 'Server=localhost,1433;User Id=sa;Password=…;TrustServerCertificate=True;Encrypt=False';
const DF_CS_PLACEHOLDER = 'Server=…;Database=app;User Id=…;Password=…;Encrypt=True;TrustServerCertificate=True';

function openServerView(mode, server) {
  resetServerForm();
  const isEdit = mode === 'edit';
  editState = isEdit ? { type: 'server', id: server.id } : null;
  document.querySelector('#server-view h4').textContent = isEdit ? 'Redigera server' : 'Lägg till server';
  $('sf-fetch').closest('.inline-actions').style.display = isEdit ? 'none' : '';
  $('sf-cs').placeholder = isEdit ? 'Lämna tomt för att behålla nuvarande' : SF_CS_PLACEHOLDER;
  $('sf-save').textContent = isEdit ? 'Spara ändringar' : 'Spara';
  $('sf-save').disabled = !isEdit; // add-mode enables after fetch; edit-mode enabled directly
  if (isEdit) $('sf-name').value = server.name;
  showConfigView('server-view');
  $('sf-name').focus();
}

function openDbView(mode, db) {
  const isEdit = mode === 'edit';
  editState = isEdit ? { type: 'db', id: db.id } : null;
  $('df-name').value = isEdit ? db.name : '';
  $('df-cs').value = '';
  $('df-cs').placeholder = isEdit ? 'Lämna tomt för att behålla nuvarande' : DF_CS_PLACEHOLDER;
  $('df-error').textContent = '';
  $('df-save').textContent = isEdit ? 'Spara ändringar' : 'Spara & anslut';
  document.querySelector('#db-view h4').textContent = isEdit ? 'Redigera databas' : 'Lägg till databas';
  showConfigView('db-view');
  $('df-name').focus();
}

function resetServerForm() {
  $('sf-name').value = '';
  $('sf-cs').value = '';
  $('sf-error').textContent = '';
  $('sf-fetch-status').textContent = '';
  $('sf-pick').classList.add('hidden');
  $('sf-checks').innerHTML = '';
  $('sf-all').checked = true;
  $('sf-save').disabled = true;
  discoveredDbs = [];
}

async function fetchServerDbs() {
  const connectionString = $('sf-cs').value.trim();
  $('sf-error').textContent = '';
  if (!connectionString) { $('sf-error').textContent = 'Ange en connection string först.'; return; }
  const btn = $('sf-fetch');
  btn.disabled = true;
  $('sf-fetch-status').textContent = 'Ansluter…';
  try {
    const res = await fetch('/api/servers/preview', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ connectionString })
    });
    const data = await res.json();
    if (!res.ok) { $('sf-error').textContent = data.error; $('sf-fetch-status').textContent = ''; return; }
    discoveredDbs = data.databases;
    renderServerChecks();
    $('sf-pick').classList.remove('hidden');
    $('sf-save').disabled = false;
    $('sf-fetch-status').textContent = `${discoveredDbs.length} databaser`;
  } catch (err) {
    $('sf-error').textContent = err.message;
  } finally {
    btn.disabled = false;
  }
}

function renderServerChecks() {
  const box = $('sf-checks');
  box.innerHTML = '';
  for (const name of discoveredDbs) {
    const label = document.createElement('label');
    const cb = document.createElement('input');
    cb.type = 'checkbox';
    cb.checked = true;
    cb.value = name;
    cb.addEventListener('change', () => {
      const all = [...box.querySelectorAll('input')].every((x) => x.checked);
      $('sf-all').checked = all;
    });
    label.append(cb, document.createTextNode(' ' + name));
    box.appendChild(label);
  }
}

async function saveServer() {
  const name = $('sf-name').value.trim();
  const connectionString = $('sf-cs').value.trim();
  $('sf-error').textContent = '';
  if (!name) { $('sf-error').textContent = 'Ange ett namn.'; return; }

  const btn = $('sf-save');
  const restore = editState ? 'Spara ändringar' : 'Spara';
  btn.disabled = true; btn.textContent = 'Sparar…';
  try {
    let res;
    if (editState?.type === 'server') {
      const body = { name };
      if (connectionString) body.connectionString = connectionString;
      res = await fetch(`/api/servers/${encodeURIComponent(editState.id)}`, {
        method: 'PATCH', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body)
      });
    } else {
      const allChecked = $('sf-all').checked;
      const selected = [...$('sf-checks').querySelectorAll('input')].filter((x) => x.checked).map((x) => x.value);
      if (!allChecked && !selected.length) { $('sf-error').textContent = 'Välj minst en databas.'; return; }
      res = await fetch('/api/servers', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name, connectionString, databases: allChecked ? 'all' : selected })
      });
    }
    const data = await res.json();
    if (!res.ok) { $('sf-error').textContent = data.error; return; }
    showConfigView('config-list');
    await renderSources();
    await afterSourcesChanged();
  } catch (err) {
    $('sf-error').textContent = err.message;
  } finally {
    btn.disabled = false; btn.textContent = restore;
  }
}

// Add-database flow
async function saveDatabase() {
  const name = $('df-name').value.trim();
  const connectionString = $('df-cs').value.trim();
  $('df-error').textContent = '';
  const editing = editState?.type === 'db';
  if (!name) { $('df-error').textContent = 'Ange ett namn.'; return; }
  if (!editing && !connectionString) { $('df-error').textContent = 'Ange en connection string.'; return; }

  const btn = $('df-save');
  const restore = editing ? 'Spara ändringar' : 'Spara & anslut';
  btn.disabled = true; btn.textContent = 'Ansluter…';
  try {
    let res;
    if (editing) {
      const body = { name };
      if (connectionString) body.connectionString = connectionString;
      res = await fetch(`/api/databases/${encodeURIComponent(editState.id)}`, {
        method: 'PATCH', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body)
      });
    } else {
      res = await fetch('/api/databases', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name, connectionString })
      });
    }
    const data = await res.json();
    if (!res.ok) { $('df-error').textContent = data.error; return; }
    showConfigView('config-list');
    await renderSources();
    await afterSourcesChanged();
  } catch (err) {
    $('df-error').textContent = err.message;
  } finally {
    btn.disabled = false; btn.textContent = restore;
  }
}

// After adding something: refresh sidebar and, if we now have databases, allow closing.
async function afterSourcesChanged() {
  await loadDatabaseList();
  if (allDatabases.length && isOnboarding()) closeConfig();
  else if (isOnboarding()) { /* still nothing queryable — stay in onboarding */ }
}

// Config wiring
$('manage-btn').addEventListener('click', () => openConfig(false));
$('config-close').addEventListener('click', closeConfig);
$('add-server-btn').addEventListener('click', () => openServerView('add'));
$('add-db-btn').addEventListener('click', () => openDbView('add'));
$('sf-fetch').addEventListener('click', fetchServerDbs);
$('sf-save').addEventListener('click', saveServer);
$('df-save').addEventListener('click', saveDatabase);
$('sf-all').addEventListener('change', () => {
  for (const cb of $('sf-checks').querySelectorAll('input')) cb.checked = $('sf-all').checked;
});
configModal.querySelectorAll('[data-back]').forEach((b) => b.addEventListener('click', () => showConfigView('config-list')));
configModal.addEventListener('click', (e) => { if (e.target === configModal && !isOnboarding()) closeConfig(); });

document.addEventListener('keydown', (e) => {
  if (e.key !== 'Escape') return;
  if (!configModal.classList.contains('hidden')) {
    if (!isOnboarding()) closeConfig();
    return;
  }
  if (appEl.classList.contains('drawer-open')) closeDrawer();
});

// --- Wire up ----------------------------------------------------------------
dbFilter.addEventListener('input', renderDbList);
tableFilter.addEventListener('input', renderTableList);
runBtn.addEventListener('click', runQuery);

loadSizes();
(async () => {
  const res = await fetch('/api/sources');
  const data = await res.json();
  if (!data.servers.length && !data.databases.length) {
    openConfig(true);
  } else {
    await loadDatabaseList();
  }
})();
