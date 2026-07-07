// A small SQL editor buffer: insert-mode editing plus schema-aware completion,
// with modal (vim) editing layered on top in vim.rs. Serious editing still
// happens in $EDITOR (Ctrl+E) — this covers the quick-iteration loop.
use crate::db::TableInfo;
use crate::highlight;
use crate::vim::VimState;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Insert,
    Normal,
    Visual,
    VisualLine,
}

struct Snapshot {
    lines: Vec<String>,
    row: usize,
    col: usize,
}

const MAX_UNDO: usize = 200;

pub struct Editor {
    pub lines: Vec<String>,
    pub row: usize,
    pub col: usize, // char index within the line
    pub scroll_row: usize,
    pub scroll_col: usize, // display-column offset
    pub completion: Option<Completion>,
    pub mode: Mode,
    pub vim: VimState,
    pub vanchor: (usize, usize), // visual-mode anchor (row, col)
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    tables: Vec<TableInfo>,
}

pub struct Completion {
    pub items: Vec<CompItem>,
    pub sel: usize,
    /// Char count of the partial word being completed (replaced on accept).
    pub partial_len: usize,
}

#[derive(Clone)]
pub struct CompItem {
    pub insert: String,
    pub label: String,
    pub kind: CompKind,
}

#[derive(Clone, Copy, PartialEq)]
pub enum CompKind {
    Table,
    Column,
    Schema,
    Keyword,
}

const MAX_ITEMS: usize = 50;

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '@' || c == '#' || c == '$'
}

impl Editor {
    pub fn new(text: &str, vim_mode: bool) -> Self {
        let mut ed = Editor {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            scroll_row: 0,
            scroll_col: 0,
            completion: None,
            mode: if vim_mode { Mode::Normal } else { Mode::Insert },
            vim: VimState::default(),
            vanchor: (0, 0),
            undo_stack: vec![],
            redo_stack: vec![],
            tables: vec![],
        };
        ed.set_text(text);
        ed
    }

    pub fn set_tables(&mut self, tables: Vec<TableInfo>) {
        self.tables = tables;
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn set_text(&mut self, text: &str) {
        self.lines = text
            .replace("\r\n", "\n")
            .split('\n')
            .map(str::to_string)
            .collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.row = self.lines.len() - 1;
        self.col = self.lines[self.row].chars().count();
        self.completion = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        if self.mode != Mode::Insert {
            self.mode = Mode::Normal;
            self.clamp_normal_col();
        }
    }

    pub fn line_len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    fn byte_idx(&self, row: usize, col: usize) -> usize {
        self.lines[row]
            .char_indices()
            .nth(col)
            .map(|(i, _)| i)
            .unwrap_or(self.lines[row].len())
    }

    /// Public alias used by the vim layer.
    pub fn byte_index(&self, row: usize, col: usize) -> usize {
        self.byte_idx(row, col)
    }

    // --- Vim support -----------------------------------------------------------

    /// In normal/visual mode the cursor sits on a char, never past the end.
    pub fn clamp_normal_col(&mut self) {
        self.row = self.row.min(self.lines.len() - 1);
        self.col = self.col.min(self.line_len(self.row).saturating_sub(1));
    }

    /// One step left within the line (normal-mode h).
    pub fn norm_left(&mut self) -> (usize, usize) {
        (self.row, self.col.saturating_sub(1))
    }

    /// One step right within the line (normal-mode l).
    pub fn norm_right(&mut self) -> (usize, usize) {
        (
            self.row,
            (self.col + 1).min(self.line_len(self.row).saturating_sub(1)),
        )
    }

    pub fn push_undo(&mut self) {
        self.undo_stack.push(Snapshot {
            lines: self.lines.clone(),
            row: self.row,
            col: self.col,
        });
        if self.undo_stack.len() > MAX_UNDO {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    pub fn undo(&mut self) {
        if let Some(snap) = self.undo_stack.pop() {
            self.redo_stack.push(Snapshot {
                lines: self.lines.clone(),
                row: self.row,
                col: self.col,
            });
            self.lines = snap.lines;
            self.row = snap.row.min(self.lines.len() - 1);
            self.col = snap.col;
            self.clamp_normal_col();
            self.completion = None;
        }
    }

    pub fn redo(&mut self) {
        if let Some(snap) = self.redo_stack.pop() {
            self.undo_stack.push(Snapshot {
                lines: self.lines.clone(),
                row: self.row,
                col: self.col,
            });
            self.lines = snap.lines;
            self.row = snap.row.min(self.lines.len() - 1);
            self.col = snap.col;
            self.clamp_normal_col();
            self.completion = None;
        }
    }

    // --- Edits ---------------------------------------------------------------

    pub fn insert_char(&mut self, c: char) {
        let idx = self.byte_idx(self.row, self.col);
        self.lines[self.row].insert(idx, c);
        self.col += 1;
        if is_word(c) || c == '.' {
            self.update_completion(false);
        } else {
            self.completion = None;
        }
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.replace("\r\n", "\n").replace('\r', "\n").chars() {
            if c == '\n' {
                self.newline(false);
            } else {
                let idx = self.byte_idx(self.row, self.col);
                self.lines[self.row].insert(idx, c);
                self.col += 1;
            }
        }
        self.completion = None;
    }

    pub fn newline(&mut self, auto_indent: bool) {
        let idx = self.byte_idx(self.row, self.col);
        let rest = self.lines[self.row].split_off(idx);
        let indent = if auto_indent {
            self.lines[self.row]
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect()
        } else {
            String::new()
        };
        self.row += 1;
        self.col = indent.chars().count();
        self.lines.insert(self.row, indent + &rest);
        self.completion = None;
    }

    pub fn backspace(&mut self) {
        if self.col > 0 {
            let idx = self.byte_idx(self.row, self.col - 1);
            self.lines[self.row].remove(idx);
            self.col -= 1;
            if self.completion.is_some() {
                self.update_completion(false);
            }
        } else if self.row > 0 {
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.line_len(self.row);
            self.lines[self.row].push_str(&cur);
            self.completion = None;
        }
    }

    pub fn delete(&mut self) {
        if self.col < self.line_len(self.row) {
            let idx = self.byte_idx(self.row, self.col);
            self.lines[self.row].remove(idx);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
        self.completion = None;
    }

    pub fn delete_word_back(&mut self) {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut c = self.col;
        while c > 0 && chars[c - 1].is_whitespace() {
            c -= 1;
        }
        if c > 0 && is_word(chars[c - 1]) {
            while c > 0 && is_word(chars[c - 1]) {
                c -= 1;
            }
        } else if c > 0 {
            c -= 1;
        }
        let start = self.byte_idx(self.row, c);
        let end = self.byte_idx(self.row, self.col);
        self.lines[self.row].replace_range(start..end, "");
        self.col = c;
        self.completion = None;
    }

    pub fn delete_word_forward(&mut self) {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let len = chars.len();
        let mut c = self.col;
        while c < len && chars[c].is_whitespace() {
            c += 1;
        }
        if c < len && is_word(chars[c]) {
            while c < len && is_word(chars[c]) {
                c += 1;
            }
        } else if c < len {
            c += 1;
        }
        let start = self.byte_idx(self.row, self.col);
        let end = self.byte_idx(self.row, c);
        self.lines[self.row].replace_range(start..end, "");
        self.completion = None;
    }

    pub fn kill_to_line_start(&mut self) {
        let end = self.byte_idx(self.row, self.col);
        self.lines[self.row].replace_range(..end, "");
        self.col = 0;
        self.completion = None;
    }

    // --- Movement ------------------------------------------------------------

    pub fn move_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.line_len(self.row);
        }
        self.completion = None;
    }

    pub fn move_right(&mut self) {
        if self.col < self.line_len(self.row) {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
        self.completion = None;
    }

    pub fn move_up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.col = self.col.min(self.line_len(self.row));
        }
        self.completion = None;
    }

    pub fn move_down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.col.min(self.line_len(self.row));
        }
        self.completion = None;
    }

    pub fn home(&mut self) {
        // Smart home: first non-blank, then column 0.
        let first = self.lines[self.row]
            .chars()
            .take_while(|c| c.is_whitespace())
            .count();
        self.col = if self.col == first { 0 } else { first };
        self.completion = None;
    }

    pub fn end(&mut self) {
        self.col = self.line_len(self.row);
        self.completion = None;
    }

    pub fn doc_start(&mut self) {
        self.row = 0;
        self.col = 0;
        self.completion = None;
    }

    pub fn doc_end(&mut self) {
        self.row = self.lines.len() - 1;
        self.col = self.line_len(self.row);
        self.completion = None;
    }

    pub fn word_left(&mut self) {
        if self.col == 0 {
            self.move_left();
            return;
        }
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut c = self.col;
        while c > 0 && !is_word(chars[c - 1]) {
            c -= 1;
        }
        while c > 0 && is_word(chars[c - 1]) {
            c -= 1;
        }
        self.col = c;
        self.completion = None;
    }

    pub fn word_right(&mut self) {
        let len = self.line_len(self.row);
        if self.col >= len {
            self.move_right();
            return;
        }
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut c = self.col;
        while c < len && is_word(chars[c]) {
            c += 1;
        }
        while c < len && !is_word(chars[c]) {
            c += 1;
        }
        self.col = c;
        self.completion = None;
    }

    // --- Completion ------------------------------------------------------------

    /// The word under the cursor: (qualifier before a '.', partial word, start col).
    fn current_word(&self) -> (Option<String>, String, usize) {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut start = self.col;
        while start > 0 && is_word(chars[start - 1]) {
            start -= 1;
        }
        let partial: String = chars[start..self.col].iter().collect();
        let mut qual = None;
        if start > 0 && chars[start - 1] == '.' {
            let mut qs = start - 1;
            while qs > 0 && (is_word(chars[qs - 1]) || chars[qs - 1] == ']' || chars[qs - 1] == '[')
            {
                qs -= 1;
            }
            let q: String = chars[qs..start - 1]
                .iter()
                .filter(|c| **c != '[' && **c != ']')
                .collect();
            if !q.is_empty() {
                qual = Some(q);
            }
        }
        (qual, partial, start)
    }

    pub fn trigger_completion(&mut self) {
        self.update_completion(true);
    }

    fn update_completion(&mut self, manual: bool) {
        let (qual, partial, _) = self.current_word();
        let min_len = if manual || qual.is_some() { 0 } else { 2 };
        if partial.chars().count() < min_len {
            self.completion = None;
            return;
        }
        let items = self.candidates(qual.as_deref(), &partial);
        self.completion = if items.is_empty() {
            None
        } else {
            Some(Completion {
                items,
                sel: 0,
                partial_len: partial.chars().count(),
            })
        };
    }

    fn candidates(&self, qual: Option<&str>, partial: &str) -> Vec<CompItem> {
        let p = partial.to_lowercase();
        let matches = |s: &str| p.is_empty() || s.to_lowercase().starts_with(&p);
        let mut items: Vec<CompItem> = vec![];

        if let Some(q) = qual {
            let q = q.to_lowercase();
            // alias.column or table.column
            if let Some(table) = self.resolve_table(&q) {
                for col in &table.columns {
                    if matches(&col.name) {
                        items.push(CompItem {
                            insert: col.name.clone(),
                            label: col.name.clone(),
                            kind: CompKind::Column,
                        });
                    }
                }
            } else {
                // schema.table
                for t in self.tables.iter().filter(|t| t.schema.to_lowercase() == q) {
                    if matches(&t.name) {
                        items.push(CompItem {
                            insert: t.name.clone(),
                            label: t.name.clone(),
                            kind: CompKind::Table,
                        });
                    }
                }
            }
        } else {
            let mut seen_cols = std::collections::HashSet::new();
            for t in &self.tables {
                if matches(&t.name) {
                    let label = format!("{}.{}", t.schema, t.name);
                    items.push(CompItem {
                        insert: t.name.clone(),
                        label,
                        kind: CompKind::Table,
                    });
                }
            }
            let mut schemas: Vec<&str> = self.tables.iter().map(|t| t.schema.as_str()).collect();
            schemas.sort_unstable();
            schemas.dedup();
            for s in schemas {
                if s != "dbo" && matches(s) {
                    items.push(CompItem {
                        insert: s.to_string(),
                        label: s.to_string(),
                        kind: CompKind::Schema,
                    });
                }
            }
            for t in &self.tables {
                for col in &t.columns {
                    if matches(&col.name) && seen_cols.insert(col.name.to_lowercase()) {
                        items.push(CompItem {
                            insert: col.name.clone(),
                            label: col.name.clone(),
                            kind: CompKind::Column,
                        });
                    }
                }
            }
            if !p.is_empty() {
                for kw in highlight::keywords() {
                    if kw.to_lowercase().starts_with(&p) {
                        items.push(CompItem {
                            insert: kw.to_string(),
                            label: kw.to_string(),
                            kind: CompKind::Keyword,
                        });
                    }
                }
            }
        }

        let kind_rank = |k: CompKind| match k {
            CompKind::Table => 0,
            CompKind::Column => 1,
            CompKind::Schema => 2,
            CompKind::Keyword => 3,
        };
        items.sort_by(|a, b| {
            kind_rank(a.kind)
                .cmp(&kind_rank(b.kind))
                .then(a.insert.len().cmp(&b.insert.len()))
                .then(a.insert.cmp(&b.insert))
        });
        items.truncate(MAX_ITEMS);
        items
    }

    /// Resolve a qualifier (lowercase) to a table: by name, schema.name, or a
    /// FROM/JOIN alias found in the query text.
    fn resolve_table(&self, q: &str) -> Option<&TableInfo> {
        if let Some(t) = self.tables.iter().find(|t| {
            t.name.to_lowercase() == q || format!("{}.{}", t.schema, t.name).to_lowercase() == q
        }) {
            return Some(t);
        }
        for (alias, table) in self.aliases() {
            if alias == q {
                return self.tables.iter().find(|t| {
                    t.name.to_lowercase() == table
                        || format!("{}.{}", t.schema, t.name).to_lowercase() == table
                });
            }
        }
        None
    }

    /// Scan the buffer for `FROM/JOIN <table> [AS] <alias>` pairs.
    fn aliases(&self) -> Vec<(String, String)> {
        let text = self.text().to_lowercase();
        let tokens: Vec<String> = {
            let mut out = vec![];
            let mut cur = String::new();
            for c in text.chars() {
                if is_word(c) || c == '.' {
                    cur.push(c);
                } else if c != '[' && c != ']' {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
            }
            if !cur.is_empty() {
                out.push(cur);
            }
            out
        };
        let mut pairs = vec![];
        let mut i = 0;
        while i < tokens.len() {
            if tokens[i] == "from" || tokens[i] == "join" {
                if let Some(table) = tokens.get(i + 1) {
                    let mut j = i + 2;
                    if tokens.get(j).map(String::as_str) == Some("as") {
                        j += 1;
                    }
                    if let Some(alias) = tokens.get(j) {
                        if !highlight::is_keyword(alias) && !alias.contains('.') {
                            pairs.push((alias.clone(), table.clone()));
                        }
                    }
                }
            }
            i += 1;
        }
        pairs
    }

    pub fn completion_move(&mut self, delta: isize) {
        if let Some(c) = &mut self.completion {
            let len = c.items.len() as isize;
            c.sel = ((c.sel as isize + delta).rem_euclid(len)) as usize;
        }
    }

    pub fn accept_completion(&mut self) -> bool {
        let Some(c) = self.completion.take() else {
            return false;
        };
        let Some(item) = c.items.get(c.sel) else {
            return false;
        };
        let start = self.col - c.partial_len;
        let b_start = self.byte_idx(self.row, start);
        let b_end = self.byte_idx(self.row, self.col);
        let insert = item.insert.clone();
        self.lines[self.row].replace_range(b_start..b_end, &insert);
        self.col = start + insert.chars().count();
        true
    }
}
