// Tiny tokenizers for SQL and JSON. Output is per-line lists of (text, color)
// spans; multi-line tokens (block comments, strings) are split on newlines.
use crate::theme;
use ratatui::style::Color;

pub type SpanLine = Vec<(String, Color)>;

const KEYWORDS: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "FROM", "WHERE", "JOIN", "INNER", "LEFT",
    "RIGHT", "FULL", "OUTER", "CROSS", "APPLY", "ON", "GROUP", "BY", "ORDER", "HAVING", "TOP",
    "DISTINCT", "AS", "AND", "OR", "NOT", "NULL", "IS", "IN", "EXISTS", "BETWEEN", "LIKE",
    "CASE", "WHEN", "THEN", "ELSE", "END", "UNION", "ALL", "VALUES", "INTO", "SET", "CREATE",
    "ALTER", "DROP", "TRUNCATE", "TABLE", "VIEW", "INDEX", "PROCEDURE", "PROC", "FUNCTION",
    "TRIGGER", "DECLARE", "BEGIN", "COMMIT", "ROLLBACK", "TRAN", "TRANSACTION", "WITH", "OVER",
    "PARTITION", "OFFSET", "FETCH", "NEXT", "ROWS", "ONLY", "ASC", "DESC", "EXEC", "EXECUTE",
    "PRINT", "IF", "WHILE", "RETURN", "GO", "USE", "PIVOT", "UNPIVOT", "OUTPUT", "DEFAULT",
    "PRIMARY", "FOREIGN", "KEY", "REFERENCES", "CONSTRAINT", "IDENTITY", "ADD", "COLUMN",
];

const FUNCTIONS: &[&str] = &[
    "COUNT", "SUM", "AVG", "MIN", "MAX", "CAST", "CONVERT", "COALESCE", "ISNULL", "NULLIF",
    "GETDATE", "GETUTCDATE", "SYSDATETIME", "NEWID", "ROW_NUMBER", "RANK", "DENSE_RANK",
    "LEN", "DATALENGTH", "SUBSTRING", "REPLACE", "UPPER", "LOWER", "LTRIM", "RTRIM", "TRIM",
    "CONCAT", "FORMAT", "DATEADD", "DATEDIFF", "DATEPART", "YEAR", "MONTH", "DAY", "IIF",
    "TRY_CAST", "TRY_CONVERT", "STRING_AGG", "STUFF", "CHARINDEX", "ABS", "ROUND", "FLOOR",
    "CEILING", "JSON_VALUE", "JSON_QUERY", "OPENJSON", "OBJECT_ID", "SCOPE_IDENTITY",
];

pub fn is_keyword(word: &str) -> bool {
    let upper = word.to_ascii_uppercase();
    KEYWORDS.contains(&upper.as_str()) || FUNCTIONS.contains(&upper.as_str())
}

pub fn keywords() -> impl Iterator<Item = &'static str> {
    KEYWORDS.iter().chain(FUNCTIONS.iter()).copied()
}

struct Emitter {
    lines: Vec<SpanLine>,
}

impl Emitter {
    fn new() -> Self {
        Emitter { lines: vec![vec![]] }
    }

    fn emit(&mut self, text: &str, color: Color) {
        let mut first = true;
        for part in text.split('\n') {
            if !first {
                self.lines.push(vec![]);
            }
            first = false;
            if part.is_empty() {
                continue;
            }
            let line = self.lines.last_mut().unwrap();
            match line.last_mut() {
                Some((prev, c)) if *c == color => prev.push_str(part),
                _ => line.push((part.to_string(), color)),
            }
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '@' || c == '#'
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '@' || c == '#' || c == '$'
}

pub fn highlight_sql(text: &str) -> Vec<SpanLine> {
    let chars: Vec<char> = text.chars().collect();
    let mut em = Emitter::new();
    let mut i = 0;
    let n = chars.len();

    while i < n {
        let c = chars[i];
        // Line comment
        if c == '-' && i + 1 < n && chars[i + 1] == '-' {
            let start = i;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            em.emit(&chars[start..i].iter().collect::<String>(), theme::SQL_COMMENT);
            continue;
        }
        // Block comment
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            let start = i;
            i += 2;
            while i < n && !(chars[i] == '*' && i + 1 < n && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(n);
            em.emit(&chars[start..i].iter().collect::<String>(), theme::SQL_COMMENT);
            continue;
        }
        // String literal (with '' escape). N'...' handled via ident path fallthrough.
        if c == '\'' {
            let start = i;
            i += 1;
            while i < n {
                if chars[i] == '\'' {
                    if i + 1 < n && chars[i + 1] == '\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            em.emit(&chars[start..i].iter().collect::<String>(), theme::SQL_STRING);
            continue;
        }
        // Bracketed identifier
        if c == '[' {
            let start = i;
            i += 1;
            while i < n && chars[i] != ']' && chars[i] != '\n' {
                i += 1;
            }
            if i < n && chars[i] == ']' {
                i += 1;
            }
            em.emit(&chars[start..i].iter().collect::<String>(), theme::TEXT);
            continue;
        }
        // Number
        if c.is_ascii_digit() {
            let start = i;
            while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            em.emit(&chars[start..i].iter().collect::<String>(), theme::SQL_NUMBER);
            continue;
        }
        // Word
        if is_ident_start(c) {
            let start = i;
            while i < n && is_ident(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let color = if is_keyword(&word) { theme::SQL_KEYWORD } else { theme::TEXT };
            em.emit(&word, color);
            continue;
        }
        // Whitespace / newline
        if c.is_whitespace() {
            let start = i;
            while i < n && chars[i].is_whitespace() {
                i += 1;
            }
            em.emit(&chars[start..i].iter().collect::<String>(), theme::TEXT);
            continue;
        }
        // Operator / punctuation
        em.emit(&c.to_string(), theme::SQL_OPERATOR);
        i += 1;
    }
    em.lines
}

pub fn highlight_json(text: &str) -> Vec<SpanLine> {
    let chars: Vec<char> = text.chars().collect();
    let mut em = Emitter::new();
    let mut i = 0;
    let n = chars.len();

    while i < n {
        let c = chars[i];
        if c == '"' {
            let start = i;
            i += 1;
            while i < n && chars[i] != '"' {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(n);
            // Property name if the next non-space char is ':'
            let mut j = i;
            while j < n && (chars[j] == ' ' || chars[j] == '\t') {
                j += 1;
            }
            let color = if j < n && chars[j] == ':' { theme::J_PROP } else { theme::J_STRING };
            em.emit(&chars[start..i].iter().collect::<String>(), color);
            continue;
        }
        if c.is_ascii_digit() || (c == '-' && i + 1 < n && chars[i + 1].is_ascii_digit()) {
            let start = i;
            i += 1;
            while i < n && (chars[i].is_ascii_digit() || "+-.eE".contains(chars[i])) {
                i += 1;
            }
            em.emit(&chars[start..i].iter().collect::<String>(), theme::J_NUMBER);
            continue;
        }
        if c.is_alphabetic() {
            let start = i;
            while i < n && chars[i].is_alphabetic() {
                i += 1;
            }
            em.emit(&chars[start..i].iter().collect::<String>(), theme::J_BOOL);
            continue;
        }
        if c.is_whitespace() {
            let start = i;
            while i < n && chars[i].is_whitespace() {
                i += 1;
            }
            em.emit(&chars[start..i].iter().collect::<String>(), theme::TEXT);
            continue;
        }
        em.emit(&c.to_string(), theme::J_PUNCT);
        i += 1;
    }
    em.lines
}

/// Parse a cell value as JSON the same way querybench does: only if it looks
/// like an object/array, and pretty-print on success.
pub fn try_pretty_json(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    serde_json::to_string_pretty(&parsed).ok()
}
