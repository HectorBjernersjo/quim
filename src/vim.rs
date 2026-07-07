// Modal (vim) editing for the query editor. Insert mode reuses the plain
// editor path in app.rs; this module handles normal and visual mode. Scope:
// motions (h l j k w b e 0 ^ $ gg G f F t T ; ,), operators (d c y + dd cc yy),
// x X s S D C Y r J, i a I A o O, p P, u/U undo/redo, visual + visual-line,
// counts. No macros, marks, registers beyond the unnamed one, or ex commands.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::editor::{Editor, Mode};

pub enum Outcome {
    Consumed,
    /// An ex command (`:w`) asked to run the current query.
    RunQuery,
}

#[derive(Default)]
pub struct VimState {
    count: String,
    operator: Option<char>, // 'd' | 'c' | 'y'
    pending_g: bool,
    /// Waiting for the target of f/F/t/T ('f'…) or r ('r').
    char_wait: Option<char>,
    /// Waiting for a text object after d/c/y + i/a ('i' or 'a').
    pending_object: Option<char>,
    pub last_find: Option<(char, char)>, // (kind, target)
    pub register: Register,
    /// Ex command line (`:`). `Some(buf)` while typing a command; the leading
    /// ':' is implied and not stored.
    pub cmdline: Option<String>,
}

#[derive(Default, Clone)]
pub struct Register {
    pub text: String,
    pub linewise: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum RangeKind {
    Charwise, // start..end, end exclusive
    Linewise, // whole lines start.row..=end.row
}

pub fn handle_key(ed: &mut Editor, key: KeyEvent) -> Outcome {
    if ed.vim.cmdline.is_some() {
        return cmdline_key(ed, key);
    }
    let KeyCode::Char(c) = key.code else {
        return special_key(ed, key);
    };
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match c {
            'd' => {
                scroll_half(ed, 1);
                Outcome::Consumed
            }
            'u' => {
                scroll_half(ed, -1);
                Outcome::Consumed
            }
            _ => Outcome::Consumed,
        };
    }

    // A pending f/F/t/T/r consumes the next char verbatim.
    if let Some(kind) = ed.vim.char_wait.take() {
        let count = take_count(ed).unwrap_or(1);
        if kind == 'r' {
            replace_char(ed, c, count);
        } else {
            ed.vim.last_find = Some((kind, c));
            do_find(ed, kind, c, count);
        }
        return Outcome::Consumed;
    }

    // Text objects: d/c/y + i/a + w (also quotes and brackets).
    if let Some(around) = ed.vim.pending_object.take() {
        text_object(ed, c, around == 'a');
        return Outcome::Consumed;
    }

    // gg / g<other>
    if ed.vim.pending_g {
        ed.vim.pending_g = false;
        if c == 'g' {
            let row = first_line_target(ed).0;
            linewise_or_move(ed, row);
        } else {
            ed.vim.count.clear();
            ed.vim.operator = None;
        }
        return Outcome::Consumed;
    }

    // Counts. '0' is a motion when no count has been started.
    if c.is_ascii_digit() && !(c == '0' && ed.vim.count.is_empty()) {
        ed.vim.count.push(c);
        return Outcome::Consumed;
    }

    match c {
        // --- motions ---------------------------------------------------------
        'h' => motion_n(ed, |e| e.norm_left()),
        'l' => motion_n(ed, |e| e.norm_right()),
        'j' => vertical(ed, 1),
        'k' => vertical(ed, -1),
        'w' => motion_n(ed, word_fwd),
        'b' => motion_n(ed, word_back),
        'e' => motion_e(ed),
        '0' => motion_to(ed, (ed.row, 0)),
        '^' => motion_to(ed, (ed.row, first_nonblank(ed, ed.row))),
        '$' => motion_dollar(ed),
        'G' => {
            let row = match take_count(ed) {
                Some(n) => (n - 1).min(ed.lines.len() - 1),
                None => ed.lines.len() - 1,
            };
            linewise_or_move(ed, row);
        }
        'g' => ed.vim.pending_g = true,
        'f' | 'F' | 't' | 'T' => ed.vim.char_wait = Some(c),
        ';' | ',' => {
            if let Some((kind, target)) = ed.vim.last_find {
                let kind = if c == ',' { invert_find(kind) } else { kind };
                let count = take_count(ed).unwrap_or(1);
                do_find(ed, kind, target, count);
            }
        }

        // --- operators -------------------------------------------------------
        'd' | 'c' | 'y' => {
            if ed.mode != Mode::Normal {
                visual_operate(ed, c);
            } else if ed.vim.operator == Some(c) {
                // dd / cc / yy
                let count = take_count(ed).unwrap_or(1);
                ed.vim.operator = None;
                lines_operate(ed, c, count);
            } else {
                ed.vim.operator = Some(c);
            }
        }

        // --- shorthands ------------------------------------------------------
        'x' => {
            if ed.mode != Mode::Normal {
                visual_operate(ed, 'd');
            } else {
                let count = take_count(ed).unwrap_or(1);
                let end = (ed.col + count).min(ed.line_len(ed.row));
                if end > ed.col {
                    apply_op(ed, 'd', (ed.row, ed.col), (ed.row, end), RangeKind::Charwise);
                }
            }
        }
        'X' => {
            let count = take_count(ed).unwrap_or(1);
            let start = ed.col.saturating_sub(count);
            if start < ed.col {
                apply_op(ed, 'd', (ed.row, start), (ed.row, ed.col), RangeKind::Charwise);
            }
        }
        'D' => op_to_eol(ed, 'd'),
        'C' => op_to_eol(ed, 'c'),
        'Y' => {
            let count = take_count(ed).unwrap_or(1);
            lines_operate(ed, 'y', count);
        }
        's' => {
            let end = (ed.col + take_count(ed).unwrap_or(1)).min(ed.line_len(ed.row));
            apply_op(ed, 'c', (ed.row, ed.col), (ed.row, end), RangeKind::Charwise);
        }
        'S' => {
            let count = take_count(ed).unwrap_or(1);
            lines_operate(ed, 'c', count);
        }
        'r' => ed.vim.char_wait = Some('r'),
        'J' => {
            let count = take_count(ed).unwrap_or(1);
            join_lines(ed, count);
        }

        // --- mode changes ----------------------------------------------------
        'i' => {
            if ed.vim.operator.is_some() {
                ed.vim.pending_object = Some('i');
            } else {
                enter_insert(ed);
            }
        }
        'a' => {
            if ed.vim.operator.is_some() {
                ed.vim.pending_object = Some('a');
            } else {
                ed.col = (ed.col + 1).min(ed.line_len(ed.row));
                enter_insert(ed);
            }
        }
        'I' => {
            ed.col = first_nonblank(ed, ed.row);
            enter_insert(ed);
        }
        'A' => {
            ed.col = ed.line_len(ed.row);
            enter_insert(ed);
        }
        'o' => {
            if ed.mode == Mode::Normal {
                open_line(ed, true);
            } else {
                visual_swap_ends(ed);
            }
        }
        'O' => open_line(ed, false),
        'v' => toggle_visual(ed, Mode::Visual),
        'V' => toggle_visual(ed, Mode::VisualLine),
        ':' => {
            clear_pending(ed);
            ed.vim.cmdline = Some(String::new());
        }

        // --- paste / undo ----------------------------------------------------
        'p' => {
            let count = take_count(ed).unwrap_or(1);
            paste(ed, true, count);
        }
        'P' => {
            let count = take_count(ed).unwrap_or(1);
            paste(ed, false, count);
        }
        'u' => {
            ed.undo();
            clear_pending(ed);
        }
        'U' => {
            ed.redo();
            clear_pending(ed);
        }
        _ => clear_pending(ed),
    }
    Outcome::Consumed
}

fn special_key(ed: &mut Editor, key: KeyEvent) -> Outcome {
    match key.code {
        KeyCode::Esc => {
            // Like vim: Esc cancels pending input or visual mode, otherwise
            // nothing (panes are switched with Ctrl+HJKL).
            clear_pending(ed);
            if ed.mode != Mode::Normal {
                ed.mode = Mode::Normal;
            }
            Outcome::Consumed
        }
        KeyCode::Left => handle_key(ed, KeyEvent::from(KeyCode::Char('h'))),
        KeyCode::Right => handle_key(ed, KeyEvent::from(KeyCode::Char('l'))),
        KeyCode::Up => handle_key(ed, KeyEvent::from(KeyCode::Char('k'))),
        KeyCode::Down => handle_key(ed, KeyEvent::from(KeyCode::Char('j'))),
        KeyCode::Home => handle_key(ed, KeyEvent::from(KeyCode::Char('0'))),
        KeyCode::End => handle_key(ed, KeyEvent::from(KeyCode::Char('$'))),
        KeyCode::Enter => handle_key(ed, KeyEvent::from(KeyCode::Char('j'))),
        KeyCode::Backspace => handle_key(ed, KeyEvent::from(KeyCode::Char('h'))),
        KeyCode::Delete => handle_key(ed, KeyEvent::from(KeyCode::Char('x'))),
        _ => Outcome::Consumed,
    }
}

// --- ex command line -----------------------------------------------------------

/// Keys while the `:` command line is open. Typing edits the buffer; Enter runs
/// it, Esc (or backspacing past the ':') cancels.
fn cmdline_key(ed: &mut Editor, key: KeyEvent) -> Outcome {
    match key.code {
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            ed.vim.cmdline.as_mut().unwrap().push(c);
        }
        KeyCode::Backspace => {
            let buf = ed.vim.cmdline.as_mut().unwrap();
            if buf.pop().is_none() {
                ed.vim.cmdline = None; // backspacing over the ':' leaves command mode
            }
        }
        KeyCode::Esc => ed.vim.cmdline = None,
        KeyCode::Enter => {
            let cmd = ed.vim.cmdline.take().unwrap();
            return run_command(cmd.trim());
        }
        _ => {}
    }
    Outcome::Consumed
}

/// Map an ex command to an outcome. Only the write family runs the query; other
/// commands are silently ignored (there is no file or buffer to act on).
fn run_command(cmd: &str) -> Outcome {
    match cmd {
        "w" | "wq" | "x" | "write" => Outcome::RunQuery,
        _ => Outcome::Consumed,
    }
}

// --- pending state -------------------------------------------------------------

fn take_count(ed: &mut Editor) -> Option<usize> {
    let s = std::mem::take(&mut ed.vim.count);
    s.parse().ok().filter(|n| *n > 0)
}

fn clear_pending(ed: &mut Editor) {
    ed.vim.count.clear();
    ed.vim.operator = None;
    ed.vim.pending_g = false;
    ed.vim.char_wait = None;
    ed.vim.pending_object = None;
}

fn enter_insert(ed: &mut Editor) {
    ed.push_undo();
    ed.mode = Mode::Insert;
    clear_pending(ed);
}

// --- motions -------------------------------------------------------------------

/// Run a simple motion `count` times; if an operator is pending, apply it over
/// the moved-over range (exclusive).
fn motion_n(ed: &mut Editor, step: fn(&mut Editor) -> (usize, usize)) {
    let count = take_count(ed).unwrap_or(1);
    let start = (ed.row, ed.col);
    let mut target = start;
    for _ in 0..count {
        let (r, c) = (ed.row, ed.col);
        target = step(ed);
        ed.row = target.0;
        ed.col = target.1;
        if (ed.row, ed.col) == (r, c) {
            break;
        }
    }
    finish_motion(ed, start, target, RangeKind::Charwise);
}

fn motion_to(ed: &mut Editor, target: (usize, usize)) {
    let start = (ed.row, ed.col);
    ed.vim.count.clear();
    ed.row = target.0;
    ed.col = target.1;
    finish_motion(ed, start, target, RangeKind::Charwise);
}

fn first_line_target(ed: &mut Editor) -> (usize, usize) {
    match take_count(ed) {
        Some(n) => ((n - 1).min(ed.lines.len() - 1), 0),
        None => (0, 0),
    }
}

/// After moving: if an operator was pending, apply it over start..target and
/// restore the cursor to the range start.
fn finish_motion(ed: &mut Editor, start: (usize, usize), target: (usize, usize), kind: RangeKind) {
    let Some(op) = ed.vim.operator.take() else {
        ed.clamp_normal_col();
        return;
    };
    let (a, b) = if (target.0, target.1) < (start.0, start.1) { (target, start) } else { (start, target) };
    if a == b && kind == RangeKind::Charwise {
        return;
    }
    apply_op(ed, op, a, b, kind);
}

/// gg/G with a pending operator act linewise; without one they just move.
fn linewise_or_move(ed: &mut Editor, row: usize) {
    if let Some(op) = ed.vim.operator.take() {
        let (a, b) = if row < ed.row { (row, ed.row) } else { (ed.row, row) };
        apply_op(ed, op, (a, 0), (b, 0), RangeKind::Linewise);
    } else {
        ed.row = row;
        ed.col = first_nonblank(ed, row);
    }
}

fn vertical(ed: &mut Editor, dir: isize) {
    let count = take_count(ed).unwrap_or(1) as isize;
    if let Some(op) = ed.vim.operator.take() {
        let target = (ed.row as isize + dir * count).clamp(0, ed.lines.len() as isize - 1) as usize;
        let (a, b) = if target < ed.row { (target, ed.row) } else { (ed.row, target) };
        apply_op(ed, op, (a, 0), (b, 0), RangeKind::Linewise);
        return;
    }
    let row = (ed.row as isize + dir * count).clamp(0, ed.lines.len() as isize - 1) as usize;
    ed.row = row;
    ed.clamp_normal_col();
}

fn motion_dollar(ed: &mut Editor) {
    ed.vim.count.clear();
    if let Some(op) = ed.vim.operator.take() {
        apply_op(ed, op, (ed.row, ed.col), (ed.row, ed.line_len(ed.row)), RangeKind::Charwise);
        return;
    }
    ed.col = ed.line_len(ed.row).saturating_sub(1);
    if ed.mode != Mode::Normal {
        ed.col = ed.line_len(ed.row).saturating_sub(1);
    }
}

/// e is inclusive: with an operator the range extends one past the word end.
fn motion_e(ed: &mut Editor) {
    let count = take_count(ed).unwrap_or(1);
    let op = ed.vim.operator.take();
    let start = (ed.row, ed.col);
    for _ in 0..count {
        let (r, c) = word_end(ed);
        if (r, c) == (ed.row, ed.col) {
            break;
        }
        ed.row = r;
        ed.col = c;
    }
    if let Some(op) = op {
        let end = (ed.row, (ed.col + 1).min(ed.line_len(ed.row)));
        ed.row = start.0;
        ed.col = start.1;
        apply_op(ed, op, start, end, RangeKind::Charwise);
    }
}

fn op_to_eol(ed: &mut Editor, op: char) {
    ed.vim.count.clear();
    apply_op(ed, op, (ed.row, ed.col), (ed.row, ed.line_len(ed.row)), RangeKind::Charwise);
}

fn first_nonblank(ed: &Editor, row: usize) -> usize {
    ed.lines[row].chars().take_while(|c| c.is_whitespace()).count()
}

fn scroll_half(ed: &mut Editor, dir: isize) {
    let jump = 6isize; // half of a typical editor pane
    let row = (ed.row as isize + dir * jump).clamp(0, ed.lines.len() as isize - 1) as usize;
    ed.row = row;
    ed.clamp_normal_col();
}

// vim word classes: word chars / punctuation / whitespace.
fn char_class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

fn char_at(ed: &Editor, row: usize, col: usize) -> Option<char> {
    ed.lines[row].chars().nth(col)
}

fn word_fwd(ed: &mut Editor) -> (usize, usize) {
    let (mut row, mut col) = (ed.row, ed.col);
    let start_class = char_at(ed, row, col).map(char_class).unwrap_or(0);
    // leave the current word/punct group
    if start_class != 0 {
        while let Some(c) = char_at(ed, row, col) {
            if char_class(c) != start_class {
                break;
            }
            col += 1;
        }
    }
    // skip whitespace, crossing line ends
    loop {
        match char_at(ed, row, col) {
            Some(c) if char_class(c) == 0 => col += 1,
            Some(_) => break,
            None => {
                if row + 1 < ed.lines.len() {
                    row += 1;
                    col = 0;
                    // vim's w stops on an empty line
                    if ed.line_len(row) == 0 {
                        break;
                    }
                } else {
                    col = ed.line_len(row);
                    break;
                }
            }
        }
    }
    (row, col)
}

fn word_back(ed: &mut Editor) -> (usize, usize) {
    let (mut row, mut col) = (ed.row, ed.col);
    loop {
        if col == 0 {
            if row == 0 {
                return (row, col);
            }
            row -= 1;
            col = ed.line_len(row);
            if col == 0 {
                return (row, 0); // empty line is a stop
            }
            continue;
        }
        col -= 1;
        if char_at(ed, row, col).map(char_class).unwrap_or(0) != 0 {
            break;
        }
    }
    let class = char_at(ed, row, col).map(char_class).unwrap();
    while col > 0 && char_at(ed, row, col - 1).map(char_class) == Some(class) {
        col -= 1;
    }
    (row, col)
}

fn word_end(ed: &Editor) -> (usize, usize) {
    let (mut row, mut col) = (ed.row, ed.col);
    loop {
        if col + 1 >= ed.line_len(row) {
            if row + 1 >= ed.lines.len() {
                return (row, ed.line_len(row).saturating_sub(1).max(col));
            }
            row += 1;
            col = 0;
            if ed.line_len(row) == 0 {
                continue;
            }
        } else {
            col += 1;
        }
        if char_at(ed, row, col).map(char_class).unwrap_or(0) != 0 {
            break;
        }
    }
    let class = char_at(ed, row, col).map(char_class).unwrap();
    while col + 1 < ed.line_len(row) && char_at(ed, row, col + 1).map(char_class) == Some(class) {
        col += 1;
    }
    (row, col)
}

fn invert_find(kind: char) -> char {
    match kind {
        'f' => 'F',
        'F' => 'f',
        't' => 'T',
        'T' => 't',
        k => k,
    }
}

fn do_find(ed: &mut Editor, kind: char, target: char, count: usize) {
    let chars: Vec<char> = ed.lines[ed.row].chars().collect();
    let mut col = ed.col;
    for _ in 0..count {
        let found = match kind {
            'f' | 't' => (col + 1..chars.len()).find(|&i| chars[i] == target),
            _ => (0..col).rev().find(|&i| chars[i] == target),
        };
        match found {
            Some(i) => col = i,
            None => {
                clear_pending(ed);
                return;
            }
        }
    }
    let col = match kind {
        't' => col.saturating_sub(1),
        'T' => col + 1,
        _ => col,
    };
    if let Some(op) = ed.vim.operator.take() {
        // f/t are inclusive forward; F/T exclusive backward.
        if matches!(kind, 'f' | 't') {
            apply_op(ed, op, (ed.row, ed.col), (ed.row, col + 1), RangeKind::Charwise);
        } else {
            apply_op(ed, op, (ed.row, col), (ed.row, ed.col), RangeKind::Charwise);
        }
    } else {
        ed.col = col;
        if ed.mode == Mode::Normal {
            ed.clamp_normal_col();
        }
    }
}

/// d/c/y + i/a + object. Objects work within the current line: w (word),
/// quotes and (){}[] brackets.
fn text_object(ed: &mut Editor, obj: char, around: bool) {
    let Some(op) = ed.vim.operator.take() else {
        clear_pending(ed);
        return;
    };
    ed.vim.count.clear();
    let range = match obj {
        'w' => word_object(ed, around),
        '"' | '\'' | '`' => quote_object(ed, obj, around),
        '(' | ')' | 'b' => bracket_object(ed, '(', ')', around),
        '[' | ']' => bracket_object(ed, '[', ']', around),
        '{' | '}' | 'B' => bracket_object(ed, '{', '}', around),
        _ => None,
    };
    if let Some((s, e)) = range {
        apply_op(ed, op, (ed.row, s), (ed.row, e), RangeKind::Charwise);
    }
}

/// iw: the word (or whitespace run) under the cursor. aw: plus trailing
/// whitespace, or leading if there is none trailing.
fn word_object(ed: &Editor, around: bool) -> Option<(usize, usize)> {
    let chars: Vec<char> = ed.lines[ed.row].chars().collect();
    if chars.is_empty() {
        return None;
    }
    let col = ed.col.min(chars.len() - 1);
    let class = char_class(chars[col]);
    let mut s = col;
    while s > 0 && char_class(chars[s - 1]) == class {
        s -= 1;
    }
    let mut e = col + 1;
    while e < chars.len() && char_class(chars[e]) == class {
        e += 1;
    }
    if around && class != 0 {
        let e2 = (e..chars.len()).take_while(|&i| chars[i].is_whitespace()).count() + e;
        if e2 > e {
            e = e2;
        } else {
            while s > 0 && chars[s - 1].is_whitespace() {
                s -= 1;
            }
        }
    }
    Some((s, e))
}

/// i"/a" etc: the quoted span the cursor is in (or the next one on the line).
fn quote_object(ed: &Editor, quote: char, around: bool) -> Option<(usize, usize)> {
    let chars: Vec<char> = ed.lines[ed.row].chars().collect();
    let positions: Vec<usize> = chars
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == quote)
        .map(|(i, _)| i)
        .collect();
    let col = ed.col;
    for pair in positions.chunks(2) {
        let [open, close] = *pair else { return None };
        if col <= close {
            return if around {
                Some((open, close + 1))
            } else {
                Some((open + 1, close))
            };
        }
    }
    None
}

/// ib/i( etc: the innermost bracket pair around the cursor on this line.
fn bracket_object(ed: &Editor, open: char, close: char, around: bool) -> Option<(usize, usize)> {
    let chars: Vec<char> = ed.lines[ed.row].chars().collect();
    if chars.is_empty() {
        return None;
    }
    let col = ed.col.min(chars.len() - 1);
    // scan backward for the unmatched opener
    let mut depth = 0i32;
    let mut start = None;
    for i in (0..=col).rev() {
        if chars[i] == close && i != col {
            depth += 1;
        } else if chars[i] == open {
            if depth == 0 {
                start = Some(i);
                break;
            }
            depth -= 1;
        }
    }
    let s = start?;
    // scan forward for its match
    let mut depth = 0i32;
    for (i, &c) in chars.iter().enumerate().skip(s + 1) {
        if c == open {
            depth += 1;
        } else if c == close {
            if depth == 0 {
                return if around { Some((s, i + 1)) } else { Some((s + 1, i)) };
            }
            depth -= 1;
        }
    }
    None
}

fn replace_char(ed: &mut Editor, c: char, count: usize) {
    let len = ed.line_len(ed.row);
    if ed.col + count > len {
        return;
    }
    ed.push_undo();
    let start = ed.byte_index(ed.row, ed.col);
    let end = ed.byte_index(ed.row, ed.col + count);
    let repl: String = std::iter::repeat(c).take(count).collect();
    ed.lines[ed.row].replace_range(start..end, &repl);
    ed.col += count - 1;
}

// --- operators -----------------------------------------------------------------

/// dd/cc/yy and D-family helpers acting on `count` whole lines from the cursor.
fn lines_operate(ed: &mut Editor, op: char, count: usize) {
    let last = (ed.row + count - 1).min(ed.lines.len() - 1);
    apply_op(ed, op, (ed.row, 0), (last, 0), RangeKind::Linewise);
}

/// Apply operator over a resolved range. Charwise: [start, end) on a single
/// line or spanning lines. Linewise: rows start.0..=end.0.
fn apply_op(ed: &mut Editor, op: char, start: (usize, usize), end: (usize, usize), kind: RangeKind) {
    ed.vim.count.clear();
    let text = range_text(ed, start, end, kind);
    ed.vim.register = Register { text: text.clone(), linewise: kind == RangeKind::Linewise };
    let _ = crate::clipboard::copy(&text); // yanks land on the system clipboard too

    if op == 'y' {
        ed.mode = Mode::Normal;
        ed.row = start.0;
        if kind == RangeKind::Charwise {
            ed.col = start.1;
        }
        ed.clamp_normal_col();
        return;
    }

    ed.push_undo();
    match kind {
        RangeKind::Charwise => {
            delete_charwise(ed, start, end);
        }
        RangeKind::Linewise => {
            if op == 'c' {
                // cc: clear the lines but keep one, preserving indentation
                let indent: String = ed.lines[start.0]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                ed.lines.drain(start.0..=end.0.min(ed.lines.len() - 1));
                ed.lines.insert(start.0, indent.clone());
                ed.row = start.0;
                ed.col = indent.chars().count();
            } else {
                ed.lines.drain(start.0..=end.0.min(ed.lines.len() - 1));
                if ed.lines.is_empty() {
                    ed.lines.push(String::new());
                }
                ed.row = start.0.min(ed.lines.len() - 1);
                ed.col = first_nonblank(ed, ed.row);
            }
        }
    }
    if op == 'c' {
        ed.mode = Mode::Insert;
    } else {
        ed.mode = Mode::Normal;
        ed.clamp_normal_col();
    }
}

fn range_text(ed: &Editor, start: (usize, usize), end: (usize, usize), kind: RangeKind) -> String {
    match kind {
        RangeKind::Linewise => ed.lines[start.0..=end.0.min(ed.lines.len() - 1)].join("\n"),
        RangeKind::Charwise => {
            if start.0 == end.0 {
                let s = ed.byte_index(start.0, start.1);
                let e = ed.byte_index(end.0, end.1);
                ed.lines[start.0][s..e].to_string()
            } else {
                let mut out = ed.lines[start.0][ed.byte_index(start.0, start.1)..].to_string();
                for row in start.0 + 1..end.0 {
                    out.push('\n');
                    out.push_str(&ed.lines[row]);
                }
                out.push('\n');
                out.push_str(&ed.lines[end.0][..ed.byte_index(end.0, end.1)]);
                out
            }
        }
    }
}

fn delete_charwise(ed: &mut Editor, start: (usize, usize), end: (usize, usize)) {
    if start.0 == end.0 {
        let s = ed.byte_index(start.0, start.1);
        let e = ed.byte_index(end.0, end.1);
        ed.lines[start.0].replace_range(s..e, "");
    } else {
        let tail = ed.lines[end.0][ed.byte_index(end.0, end.1)..].to_string();
        let s = ed.byte_index(start.0, start.1);
        ed.lines[start.0].truncate(s);
        ed.lines[start.0].push_str(&tail);
        ed.lines.drain(start.0 + 1..=end.0);
    }
    ed.row = start.0;
    ed.col = start.1;
}

fn join_lines(ed: &mut Editor, count: usize) {
    if ed.row + 1 >= ed.lines.len() {
        return;
    }
    ed.push_undo();
    let times = count.max(2) - 1; // J joins at least two lines
    for _ in 0..times {
        if ed.row + 1 >= ed.lines.len() {
            break;
        }
        let next = ed.lines.remove(ed.row + 1);
        let trimmed = next.trim_start();
        let line = &mut ed.lines[ed.row];
        while line.ends_with(' ') {
            line.pop();
        }
        ed.col = line.chars().count();
        if !trimmed.is_empty() {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(trimmed);
        }
    }
    ed.clamp_normal_col();
}

fn open_line(ed: &mut Editor, below: bool) {
    ed.push_undo();
    let indent: String = ed.lines[ed.row].chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let at = if below { ed.row + 1 } else { ed.row };
    ed.lines.insert(at, indent.clone());
    ed.row = at;
    ed.col = indent.chars().count();
    ed.mode = Mode::Insert;
    clear_pending(ed);
}

fn paste(ed: &mut Editor, after: bool, count: usize) {
    let reg = ed.vim.register.clone();
    if reg.text.is_empty() {
        return;
    }
    ed.push_undo();
    if reg.linewise {
        let at = if after { ed.row + 1 } else { ed.row };
        let mut insert_at = at;
        for _ in 0..count {
            for line in reg.text.split('\n') {
                ed.lines.insert(insert_at, line.to_string());
                insert_at += 1;
            }
        }
        ed.row = at;
        ed.col = first_nonblank(ed, ed.row);
    } else {
        let col = if after { (ed.col + 1).min(ed.line_len(ed.row)) } else { ed.col };
        ed.col = col;
        let text = reg.text.repeat(count);
        if text.contains('\n') {
            // multiline charwise paste: split the current line at the cursor
            let idx = ed.byte_index(ed.row, ed.col);
            let tail = ed.lines[ed.row].split_off(idx);
            let mut parts = text.split('\n');
            if let Some(first) = parts.next() {
                ed.lines[ed.row].push_str(first);
            }
            let mut row = ed.row;
            for part in parts {
                row += 1;
                ed.lines.insert(row, part.to_string());
            }
            ed.col = ed.line_len(row);
            ed.row = row;
            ed.lines[row].push_str(&tail);
        } else {
            let idx = ed.byte_index(ed.row, ed.col);
            ed.lines[ed.row].insert_str(idx, &text);
            ed.col += text.chars().count();
        }
        ed.col = ed.col.saturating_sub(1);
    }
    ed.clamp_normal_col();
}

// --- visual mode -----------------------------------------------------------------

fn toggle_visual(ed: &mut Editor, mode: Mode) {
    clear_pending(ed);
    if ed.mode == mode {
        ed.mode = Mode::Normal;
    } else {
        if ed.mode == Mode::Normal {
            ed.vanchor = (ed.row, ed.col);
        }
        ed.mode = mode;
    }
}

fn visual_operate(ed: &mut Editor, op: char) {
    let (mut a, mut b) = (ed.vanchor, (ed.row, ed.col));
    if b < a {
        std::mem::swap(&mut a, &mut b);
    }
    let kind = if ed.mode == Mode::VisualLine { RangeKind::Linewise } else { RangeKind::Charwise };
    ed.vim.count.clear();
    ed.vim.operator = None;
    if kind == RangeKind::Charwise {
        // visual charwise selections are inclusive of the cursor cell
        b.1 = (b.1 + 1).min(ed.line_len(b.0));
    }
    apply_op(ed, op, a, b, kind);
}

/// 'o' in visual mode: swap cursor and anchor. Called from handle_key's fall-through
/// wiring in app-level code isn't needed — kept here for clarity.
pub fn visual_swap_ends(ed: &mut Editor) {
    std::mem::swap(&mut ed.vanchor.0, &mut ed.row);
    std::mem::swap(&mut ed.vanchor.1, &mut ed.col);
}
