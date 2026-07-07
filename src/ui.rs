use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{
    App, FilterList, FormField, Pane, SettingsSection, SourceKind, StatusKind, GENERAL_ROWS,
};
use crate::db::Category;
use crate::editor::{CompKind, Mode};
use crate::highlight;
use crate::theme;

pub fn draw(f: &mut Frame, app: &mut App) {
    app.tick = app.tick.wrapping_add(1);
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(theme::BG)), area);

    let lay = app.cfg.qb.layout;
    let [main, status_a] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);

    // The settings screen replaces the whole workspace; popups still stack on top.
    if app.settings.is_some() {
        draw_settings(f, app, main);
        draw_status(f, app, status_a);
        if app.form.is_some() {
            draw_form(f, app, area);
        }
        if app.confirm.is_some() {
            draw_confirm(f, app, area);
        }
        if app.show_help {
            draw_help(f, area);
        }
        return;
    }

    let sidebar_w = lay
        .sidebar_width
        .unwrap_or((area.width / 4).clamp(24, 40))
        .clamp(16, main.width.saturating_sub(32).max(16));
    let [side, right] =
        Layout::horizontal([Constraint::Length(sidebar_w), Constraint::Min(30)]).areas(main);
    let db_h = lay
        .db_list_height
        .unwrap_or(side.height * 35 / 100)
        .clamp(3, side.height.saturating_sub(5).max(3));
    let [db_a, tbl_a] =
        Layout::vertical([Constraint::Length(db_h), Constraint::Min(5)]).areas(side);

    let (work, detail_a) = if app.detail.is_some() {
        let dw = lay
            .detail_width
            .unwrap_or((right.width * 42 / 100).clamp(30, 70))
            .clamp(20, right.width.saturating_sub(20).max(20));
        let [w, d] = Layout::horizontal([Constraint::Min(20), Constraint::Length(dw)]).areas(right);
        (w, Some(d))
    } else {
        (right, None)
    };
    let editor_h = lay
        .editor_height
        .unwrap_or_else(|| (app.editor.lines.len() as u16 + 2).clamp(6, work.height / 2))
        .clamp(3, work.height.saturating_sub(3).max(3));
    let [ed_a, res_a] =
        Layout::vertical([Constraint::Length(editor_h), Constraint::Min(3)]).areas(work);

    app.areas.db_list = db_a;
    app.areas.tbl_list = tbl_a;
    app.areas.editor = ed_a;
    app.areas.results = res_a;
    app.areas.detail = detail_a.unwrap_or_default();

    draw_db_list(f, app, db_a);
    draw_table_list(f, app, tbl_a);
    draw_editor(f, app, ed_a);
    draw_results(f, app, res_a);
    if let Some(d) = detail_a {
        draw_detail(f, app, d);
    }
    draw_status(f, app, status_a);
    if app.resize_mode {
        for wall in resize_walls(app) {
            f.buffer_mut().set_style(
                wall,
                Style::new()
                    .fg(theme::ON_ACCENT)
                    .bg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            );
        }
    }
    if app.form.is_some() {
        draw_form(f, app, area);
    }
    if app.confirm.is_some() {
        draw_confirm(f, app, area);
    }
    if app.show_help {
        draw_help(f, area);
    }
}

/// The border segments the resize keys will move for the focused pane, so they
/// can be highlighted as the "grabbed" walls. Screen edges aren't included —
/// only walls shared with a neighbouring pane can move.
fn resize_walls(app: &App) -> Vec<Rect> {
    let a = match app.focus {
        Pane::Databases => app.areas.db_list,
        Pane::Tables => app.areas.tbl_list,
        Pane::Editor => app.areas.editor,
        Pane::Results => app.areas.results,
        Pane::Detail => app.areas.detail,
    };
    if a.width == 0 || a.height == 0 {
        return vec![];
    }
    let left = Rect { x: a.x, y: a.y, width: 1, height: a.height };
    let right = Rect { x: a.x + a.width - 1, y: a.y, width: 1, height: a.height };
    let top = Rect { x: a.x, y: a.y, width: a.width, height: 1 };
    let bottom = Rect { x: a.x, y: a.y + a.height - 1, width: a.width, height: 1 };
    match app.focus {
        Pane::Databases => vec![right, bottom],
        Pane::Tables => vec![right, top],
        Pane::Editor => vec![left, bottom],
        Pane::Results => vec![if app.detail.is_some() { right } else { left }, top],
        Pane::Detail => vec![left],
    }
}

fn pane_block(title: &str, focused: bool) -> Block<'static> {
    let border = if focused { theme::ACCENT_DIM } else { theme::BORDER };
    let title_style = if focused {
        Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(theme::MUTED)
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .title(Span::styled(format!(" {title} "), title_style))
}

fn filter_line(list: &FilterList, focused: bool) -> Option<Line<'static>> {
    if list.filter.is_empty() && !list.typing {
        return None;
    }
    let style = if list.typing && focused {
        Style::new().fg(theme::ACCENT)
    } else {
        Style::new().fg(theme::MUTED)
    };
    let cursor = if list.typing && focused { "▏" } else { "" };
    Some(Line::from(vec![
        Span::styled(" /", Style::new().fg(theme::FAINT)),
        Span::styled(format!("{}{cursor}", list.filter), style),
    ]))
}

fn scroll_window(sel: usize, len: usize, height: usize, scroll: &mut usize) -> std::ops::Range<usize> {
    if height == 0 || len == 0 {
        return 0..0;
    }
    if sel < *scroll {
        *scroll = sel;
    }
    if sel >= *scroll + height {
        *scroll = sel + 1 - height;
    }
    if *scroll + height > len {
        *scroll = len.saturating_sub(height);
    }
    *scroll..(*scroll + height).min(len)
}

fn draw_db_list(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Pane::Databases;
    let title = if app.pending_servers > 0 {
        format!("Databases {}", spinner(app.tick))
    } else {
        format!("Databases ({})", app.dbs.len())
    };
    let block = pane_block(&title, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = vec![];
    if let Some(fl) = filter_line(&app.db_list, focused) {
        lines.push(fl);
    }
    let header = lines.len();
    let height = (inner.height as usize).saturating_sub(header);

    let filtered = app.filtered_dbs();
    app.db_list.sel = app.db_list.sel.min(filtered.len().saturating_sub(1));
    let range = scroll_window(app.db_list.sel, filtered.len(), height, &mut app.db_list.scroll);

    for pos in range {
        let entry = &app.dbs[filtered[pos]];
        let is_sel = pos == app.db_list.sel;
        let is_active = app.active.as_ref().map(|a| a.id.as_str()) == Some(entry.id.as_str());
        let marker = if is_active { "● " } else { "  " };
        let name_style = match (is_active, is_sel && focused) {
            (_, true) => Style::new().fg(theme::TEXT).add_modifier(Modifier::BOLD),
            (true, _) => Style::new().fg(theme::ACCENT_DIM),
            _ => Style::new().fg(theme::TEXT),
        };
        let mut line = Line::from(vec![
            Span::styled(marker, Style::new().fg(theme::ACCENT_DIM)),
            Span::styled(fit(&entry.label, inner.width.saturating_sub(3) as usize), name_style),
        ]);
        if is_sel {
            line = line.style(Style::new().bg(theme::PANEL2));
        }
        lines.push(line);
    }
    if app.dbs.is_empty() && app.pending_servers == 0 {
        lines.push(Line::styled("  (no databases)", Style::new().fg(theme::FAINT)));
        lines.push(Line::styled("  , opens settings", Style::new().fg(theme::FAINT)));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_table_list(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Pane::Tables;
    let title = if app.tables_loading {
        format!("Tables {}", spinner(app.tick))
    } else {
        format!("Tables ({})", app.tables.len())
    };
    let block = pane_block(&title, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = vec![];
    if let Some(fl) = filter_line(&app.tbl_list, focused) {
        lines.push(fl);
    }
    let header = lines.len();
    let height = (inner.height as usize).saturating_sub(header);

    let filtered = app.filtered_tables();
    app.tbl_list.sel = app.tbl_list.sel.min(filtered.len().saturating_sub(1));
    let range = scroll_window(app.tbl_list.sel, filtered.len(), height, &mut app.tbl_list.scroll);

    for pos in range {
        let t = &app.tables[filtered[pos]];
        let is_sel = pos == app.tbl_list.sel;
        let name = format!("{}.{}", t.schema, t.name);
        let count = t.rows.map(crate::app::fmt_count).unwrap_or_default();
        let w = inner.width as usize;
        let name_w = w.saturating_sub(count.width() + 3);
        let mut spans = vec![
            Span::raw(" "),
            Span::styled(
                pad(&fit(&name, name_w), name_w),
                Style::new().fg(if is_sel && focused { theme::ACCENT } else { theme::TEXT }),
            ),
            Span::raw(" "),
            Span::styled(count, Style::new().fg(theme::FAINT)),
        ];
        if is_sel {
            spans = spans
                .into_iter()
                .map(|s| {
                    let st = s.style.bg(theme::PANEL2);
                    Span::styled(s.content, st)
                })
                .collect();
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn mode_badge(app: &App) -> Option<(&'static str, Color)> {
    if !app.cfg.qb.vim_mode {
        return None;
    }
    if app.editor.vim.cmdline.is_some() {
        return Some(("COMMAND", theme::ACCENT));
    }
    Some(match app.editor.mode {
        Mode::Insert => ("INSERT", theme::ACCENT),
        Mode::Normal => ("NORMAL", theme::MUTED),
        Mode::Visual => ("VISUAL", theme::V_DATE),
        Mode::VisualLine => ("V-LINE", theme::V_DATE),
    })
}

fn draw_editor(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Pane::Editor;
    let db = app.active.as_ref().map(|a| a.label.clone()).unwrap_or_else(|| "—".into());
    let title = match mode_badge(app) {
        Some((badge, _)) if focused => format!("Query · {db} · {badge}"),
        _ => format!("Query · {db}"),
    };
    let block = pane_block(&title, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let ed = &mut app.editor;
    let height = inner.height as usize;
    let width = inner.width as usize;

    // Vertical scroll follows the cursor.
    if ed.row < ed.scroll_row {
        ed.scroll_row = ed.row;
    }
    if ed.row >= ed.scroll_row + height {
        ed.scroll_row = ed.row + 1 - height;
    }
    // Horizontal: display width of the chars left of the cursor.
    let cur_x: usize = ed.lines[ed.row]
        .chars()
        .take(ed.col)
        .map(|c| c.width().unwrap_or(0))
        .sum();
    if cur_x < ed.scroll_col {
        ed.scroll_col = cur_x;
    }
    if cur_x >= ed.scroll_col + width {
        ed.scroll_col = cur_x + 1 - width;
    }

    let highlighted = highlight::highlight_sql(&ed.text());
    let mut lines: Vec<Line> = vec![];
    for i in ed.scroll_row..(ed.scroll_row + height).min(ed.lines.len()) {
        let spans = highlighted.get(i).cloned().unwrap_or_default();
        lines.push(crop_spans(&spans, ed.scroll_col, width));
    }
    f.render_widget(Paragraph::new(lines), inner);

    // Visual-mode selection: repaint the background of the selected cells.
    if matches!(ed.mode, Mode::Visual | Mode::VisualLine) {
        let (mut a, mut b) = (ed.vanchor, (ed.row, ed.col));
        if b < a {
            std::mem::swap(&mut a, &mut b);
        }
        let linewise = ed.mode == Mode::VisualLine;
        for row in ed.scroll_row..(ed.scroll_row + height).min(ed.lines.len()) {
            if row < a.0 || row > b.0 {
                continue;
            }
            let len = ed.line_len(row);
            let (c0, c1) = if linewise {
                (0, len.max(1))
            } else {
                let s = if row == a.0 { a.1 } else { 0 };
                let e = if row == b.0 { (b.1 + 1).min(len.max(1)) } else { len.max(1) };
                (s.min(len), e)
            };
            let x0 = disp_col(&ed.lines[row], c0);
            let x1 = disp_col(&ed.lines[row], c1).max(x0 + 1);
            // clip to the horizontal scroll window
            let (lo, hi) = (ed.scroll_col, ed.scroll_col + width);
            if x1 <= lo || x0 >= hi {
                continue;
            }
            let x0 = x0.max(lo) - lo;
            let x1 = x1.min(hi) - lo;
            let rect = Rect::new(
                inner.x + x0 as u16,
                inner.y + (row - ed.scroll_row) as u16,
                (x1 - x0) as u16,
                1,
            );
            f.buffer_mut().set_style(rect, Style::new().bg(theme::PANEL2));
        }
    }

    // While the `:` command line is open the cursor lives on the status bar.
    if focused && ed.vim.cmdline.is_none() {
        f.set_cursor_position(Position::new(
            inner.x + (cur_x - ed.scroll_col) as u16,
            inner.y + (ed.row - ed.scroll_row) as u16,
        ));
        draw_completion(f, app, inner, cur_x);
    }
}

/// Display column of `char_idx` within `line`.
fn disp_col(line: &str, char_idx: usize) -> usize {
    line.chars().take(char_idx).map(|c| c.width().unwrap_or(0)).sum()
}

fn draw_completion(f: &mut Frame, app: &App, editor_inner: Rect, cur_x: usize) {
    let Some(comp) = &app.editor.completion else { return };
    let ed = &app.editor;
    let visible = 8usize.min(comp.items.len());
    let width = comp
        .items
        .iter()
        .map(|i| i.label.width() + 6)
        .max()
        .unwrap_or(10)
        .clamp(14, 44) as u16;

    let cursor_row = editor_inner.y + (ed.row - ed.scroll_row) as u16;
    let cursor_col = editor_inner.x + (cur_x - ed.scroll_col) as u16;
    let frame = f.area();
    let below = cursor_row + 1 + visible as u16 <= frame.height;
    let y = if below { cursor_row + 1 } else { cursor_row.saturating_sub(visible as u16) };
    let x = cursor_col.min(frame.width.saturating_sub(width));
    let rect = Rect::new(x, y, width.min(frame.width), visible as u16);

    // Keep the selection inside the visible window.
    let first = comp.sel.saturating_sub(visible.saturating_sub(1));
    let start = first.min(comp.items.len() - visible);

    let mut lines = vec![];
    for (i, item) in comp.items.iter().enumerate().skip(start).take(visible) {
        let selected = i == comp.sel;
        let (tag, tag_color) = match item.kind {
            CompKind::Table => ("tbl", theme::V_UUID),
            CompKind::Column => ("col", theme::V_NUMBER),
            CompKind::Schema => ("sch", theme::V_DATE),
            CompKind::Keyword => (" kw", theme::SQL_KEYWORD),
        };
        let name_w = width as usize - 5;
        let style = if selected {
            Style::new()
                .fg(theme::ON_ACCENT)
                .bg(theme::ACCENT_DIM)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme::TEXT).bg(theme::PANEL2)
        };
        lines.push(Line::from(vec![
            Span::styled(pad(&fit(&item.label, name_w), name_w), style),
            Span::styled(
                format!(" {tag} "),
                if selected { style } else { Style::new().fg(tag_color).bg(theme::PANEL2) },
            ),
        ]));
    }
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).style(Style::new().bg(theme::PANEL2)), rect);
}

pub fn display_text(cell: &Option<String>, category: Category) -> String {
    let Some(value) = cell else { return "NULL".into() };
    let flat: String = value
        .chars()
        .map(|c| match c {
            '\n' => '⏎',
            '\r' => ' ',
            '\t' => ' ',
            c => c,
        })
        .collect();
    match category {
        Category::Str => format!("\"{flat}\""),
        _ => flat,
    }
}

fn category_style(cell: &Option<String>, category: Category) -> Style {
    if cell.is_none() {
        return Style::new().fg(theme::V_NULL);
    }
    let color = match category {
        Category::Str => theme::V_STRING,
        Category::Uuid => theme::V_UUID,
        Category::Date => theme::V_DATE,
        Category::Num => theme::V_NUMBER,
        Category::Bool => theme::V_BOOL,
        Category::Bin => theme::V_BINARY,
        Category::Other => theme::TEXT,
    };
    Style::new().fg(color)
}

fn draw_results(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Pane::Results;
    let title = match &app.view {
        Some(v) if v.error.is_none() && !v.rows.is_empty() => {
            let visual = match v.vsel_range() {
                Some((lo, hi)) => format!(" · V {} row(s)", hi - lo + 1),
                None => String::new(),
            };
            format!(
                "Results · row {}/{} col {}/{}{visual}",
                v.sel_row + 1,
                v.rows.len(),
                v.sel_col + 1,
                v.cols.len()
            )
        }
        _ => "Results".into(),
    };
    let block = pane_block(&title, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 {
        return;
    }

    if app.running {
        let msg = format!("{} Running…", spinner(app.tick));
        f.render_widget(
            Paragraph::new(Line::styled(msg, Style::new().fg(theme::MUTED))),
            inner,
        );
        return;
    }
    let Some(view) = &mut app.view else {
        let hint = if app.dbs.is_empty() && app.pending_servers == 0 {
            vec![
                Line::raw(""),
                Line::styled("  No sources configured.", Style::new().fg(theme::MUTED)),
                Line::raw(""),
                Line::styled(
                    "  Press , to open Settings and add a server or database,",
                    Style::new().fg(theme::FAINT),
                ),
                Line::styled(
                    format!("  or edit {}", crate::config::config_path().display()),
                    Style::new().fg(theme::FAINT),
                ),
            ]
        } else {
            vec![
                Line::raw(""),
                Line::styled(
                    "  Run a query with Ctrl+R (or F5). Enter on a table previews it.",
                    Style::new().fg(theme::FAINT),
                ),
            ]
        };
        f.render_widget(Paragraph::new(hint), inner);
        return;
    };

    if let Some(err) = &view.error {
        f.render_widget(
            Paragraph::new(err.as_str())
                .style(Style::new().fg(theme::ERROR))
                .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }
    if view.cols.is_empty() {
        return;
    }

    let width = inner.width as usize;
    let rownum_w = view.rows.len().to_string().len().max(2);
    let body_h = (inner.height as usize).saturating_sub(2); // header + separator

    // Horizontal window: make sure the selected column fits.
    if view.sel_col < view.col_off {
        view.col_off = view.sel_col;
    }
    loop {
        let mut used = rownum_w + 2;
        let mut fits = false;
        for c in view.col_off..=view.sel_col {
            used += view.widths[c] as usize + 2;
            if c == view.sel_col && used <= width {
                fits = true;
            }
        }
        if fits || view.col_off >= view.sel_col {
            break;
        }
        view.col_off += 1;
    }

    let range = scroll_window(view.sel_row, view.rows.len(), body_h, &mut view.row_off);

    let mut header = vec![Span::styled(
        pad("#", rownum_w) + "  ",
        Style::new().fg(theme::FAINT),
    )];
    let mut sep = vec![Span::styled(
        "─".repeat(rownum_w + 2),
        Style::new().fg(theme::BORDER_SOFT),
    )];
    // Columns are rendered until the width runs out; the last one may be cut
    // mid-column (the paragraph clips it) so it's clear there is more to scroll.
    let mut used = rownum_w + 2;
    let mut last_col = view.col_off;
    for c in view.col_off..view.cols.len() {
        let w = view.widths[c] as usize;
        last_col = c;
        header.push(Span::styled(
            pad(&fit(&view.cols[c].name, w), w) + "  ",
            Style::new().fg(theme::MUTED).add_modifier(Modifier::BOLD),
        ));
        sep.push(Span::styled("─".repeat(w + 2), Style::new().fg(theme::BORDER_SOFT)));
        used += w + 2;
        if used >= width {
            break;
        }
    }

    let vsel = view.vsel_range();
    let mut lines = vec![Line::from(header), Line::from(sep)];
    for r in range {
        let row = &view.rows[r];
        let in_vsel = vsel.is_some_and(|(lo, hi)| r >= lo && r <= hi);
        let is_sel_row = r == view.sel_row || in_vsel;
        // Rows in a visual selection get a distinct accent-tinted band so the
        // whole row reads as selected; the plain cursor row keeps the subtler
        // panel tint.
        let row_bg = if in_vsel { theme::SEL } else { theme::PANEL2 };
        let mut spans = vec![Span::styled(
            pad(&(r + 1).to_string(), rownum_w) + "  ",
            if is_sel_row {
                Style::new().fg(theme::ACCENT_DIM).bg(row_bg)
            } else {
                Style::new().fg(theme::FAINT)
            },
        )];
        for c in view.col_off..=last_col {
            let w = view.widths[c] as usize;
            let cell = row.get(c).cloned().flatten();
            let text = pad(&fit(&display_text(&cell, view.cols[c].category), w), w);
            let mut style = category_style(&cell, view.cols[c].category);
            if is_sel_row {
                style = style.bg(row_bg);
            }
            // The green cursor-cell highlight is suppressed while selecting rows
            // so the selection reads as a whole row, not one bright cell.
            if is_sel_row && c == view.sel_col && focused && vsel.is_none() {
                style = Style::new()
                    .fg(theme::ON_ACCENT)
                    .bg(theme::ACCENT_DIM)
                    .add_modifier(Modifier::BOLD);
            }
            spans.push(Span::styled(text, style));
            spans.push(Span::styled(
                "  ",
                if is_sel_row { Style::new().bg(row_bg) } else { Style::new() },
            ));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Pane::Detail;
    let Some(detail) = &mut app.detail else { return };
    let block = pane_block(&detail.title, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 4 || inner.height < 2 {
        return;
    }
    let width = inner.width as usize - 1;

    let mut lines: Vec<Line> = vec![];
    match (&detail.value, &detail.pretty_json) {
        (None, _) => lines.push(Line::styled("NULL", Style::new().fg(theme::V_NULL))),
        (Some(_), Some(json)) => {
            for span_line in highlight::highlight_json(json) {
                wrap_spans(&span_line, width, &mut lines);
            }
        }
        (Some(text), None) => {
            for raw in text.split('\n') {
                wrap_spans(&[(raw.to_string(), theme::TEXT)], width, &mut lines);
            }
        }
    }
    let meta = match &detail.value {
        Some(v) => format!("{} chars · y copies", v.chars().count()),
        None => "y copies".into(),
    };
    lines.insert(0, Line::styled(meta, Style::new().fg(theme::FAINT)));
    lines.insert(1, Line::raw(""));

    let height = inner.height as usize;
    detail.scroll = detail.scroll.min(lines.len().saturating_sub(height));
    let visible: Vec<Line> = lines.into_iter().skip(detail.scroll).take(height).collect();
    f.render_widget(Paragraph::new(visible), inner);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let (text, kind) = &app.status;
    let color = match kind {
        StatusKind::Info => theme::MUTED,
        StatusKind::Ok => theme::ACCENT_DIM,
        StatusKind::Err => theme::ERROR,
    };
    // The `:` command line takes over the status bar while it is open.
    if let Some(cmd) = app.editor.vim.cmdline.as_ref().filter(|_| app.focus == Pane::Editor) {
        let text = format!(" :{cmd}");
        let cursor_x = area.x + text.width() as u16;
        f.render_widget(
            Paragraph::new(Line::styled(text, Style::new().fg(theme::TEXT)))
                .style(Style::new().bg(theme::BG)),
            area,
        );
        f.set_cursor_position(Position::new(cursor_x.min(area.x + area.width.saturating_sub(1)), area.y));
        return;
    }
    let (left, left_color, right) = if app.resize_mode {
        (
            " RESIZE".to_string(),
            theme::ACCENT,
            "h/l/j/k move wall · Esc/Enter done ".to_string(),
        )
    } else {
        let left = if app.running {
            format!(" {} Running…", spinner(app.tick))
        } else {
            format!(" {text}")
        };
        if let Some(st) = &app.settings {
            let hints = if !st.in_content {
                "j/k section · l/Enter open · q close"
            } else {
                match st.section {
                    SettingsSection::General => "j/k move · Space/Enter toggle · h back · q close",
                    _ => "j/k move · a add · e edit · d delete · t test · h back · q close",
                }
            };
            (left, color, format!("{hints} "))
        } else {
            let hints = match app.focus {
                Pane::Editor if app.cfg.qb.vim_mode && app.editor.mode != Mode::Insert => {
                    "i insert · v visual · u/U undo/redo · ^R/:w run"
                }
                Pane::Editor => "^R run · ^E/F2 $EDITOR · ^N complete",
                Pane::Results
                    if app.view.as_ref().is_some_and(|v| v.vsel.is_some()) =>
                {
                    "y JSON · Y markdown · j/k extend · Esc cancel"
                }
                Pane::Results => "Enter detail · V rows · y/Y yank · r rerun · ? help",
                Pane::Detail => "j/k scroll · y copy · q close",
                Pane::Databases => "Enter select · / filter · a/e/d sources · , settings",
                _ => "Enter select · / filter · ^R run · ? help",
            };
            (left, color, format!("{hints} · ^HJKL panes · ^W resize "))
        }
    };
    let lw = left.width();
    let rw = right.width();
    let pad_w = (area.width as usize).saturating_sub(lw + rw);
    let line = Line::from(vec![
        Span::styled(left, Style::new().fg(left_color)),
        Span::raw(" ".repeat(pad_w)),
        Span::styled(right, Style::new().fg(theme::FAINT)),
    ]);
    f.render_widget(Paragraph::new(line).style(Style::new().bg(theme::BG)), area);
}

// --- Settings screen ---------------------------------------------------------------

fn draw_settings(f: &mut Frame, app: &mut App, area: Rect) {
    // Clamp the selection first: sources may have been deleted since last draw.
    let (section, in_content) = {
        let st = app.settings.as_ref().unwrap();
        (st.section, st.in_content)
    };
    let len = match section {
        SettingsSection::General => GENERAL_ROWS,
        SettingsSection::Servers => app.cfg.servers.len(),
        SettingsSection::Databases => app.cfg.databases.len(),
    };
    if let Some(st) = app.settings.as_mut() {
        st.row = st.row.min(len.saturating_sub(1));
    }
    let sel_row = app.settings.as_ref().unwrap().row;

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::ACCENT_DIM))
        .title(Span::styled(" Settings ", Style::new().fg(theme::ACCENT).bold()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 36 || inner.height < 5 {
        return;
    }
    let [nav_a, sep_a, content_a] = Layout::horizontal([
        Constraint::Length(17),
        Constraint::Length(2),
        Constraint::Min(20),
    ])
    .areas(inner);

    // Section list on the left.
    let sections = [
        (SettingsSection::General, "General".to_string()),
        (SettingsSection::Servers, format!("Servers ({})", app.cfg.servers.len())),
        (SettingsSection::Databases, format!("Databases ({})", app.cfg.databases.len())),
    ];
    let mut nav: Vec<Line> = vec![Line::raw("")];
    for (s, label) in &sections {
        let current = *s == section;
        let marker = if current { "▸ " } else { "  " };
        let style = match (current, in_content) {
            (true, false) => Style::new()
                .fg(theme::ACCENT)
                .bg(theme::PANEL2)
                .add_modifier(Modifier::BOLD),
            (true, true) => Style::new().fg(theme::ACCENT),
            _ => Style::new().fg(theme::TEXT),
        };
        nav.push(Line::styled(pad(&format!(" {marker}{label}"), nav_a.width as usize), style));
    }
    f.render_widget(Paragraph::new(nav), nav_a);

    let sep: Vec<Line> = (0..sep_a.height)
        .map(|_| Line::styled("│", Style::new().fg(theme::BORDER_SOFT)))
        .collect();
    f.render_widget(Paragraph::new(sep), sep_a);

    // Content on the right.
    let width = content_a.width as usize;
    let sel_style = Style::new().fg(theme::TEXT).bg(theme::PANEL2).add_modifier(Modifier::BOLD);
    let row_style = |i: usize| {
        if in_content && i == sel_row {
            sel_style
        } else {
            Style::new().fg(theme::TEXT)
        }
    };
    let mut lines: Vec<Line> = vec![Line::raw("")];
    match section {
        SettingsSection::General => {
            let vim_on = app.cfg.qb.vim_mode;
            let rows = [
                (
                    format!("Vim mode            [{}] {}", if vim_on { "x" } else { " " }, if vim_on { "on" } else { "off" }),
                    "modal editing in the query pane",
                ),
                ("Reset pane layout".to_string(), "clear the sizes saved with Ctrl+W"),
            ];
            for (i, (label, hint)) in rows.iter().enumerate() {
                lines.push(Line::from(vec![
                    Span::styled(pad(&format!("  {label}"), 32.min(width)), row_style(i)),
                    Span::styled(format!("  {hint}"), Style::new().fg(theme::FAINT)),
                ]));
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!("  Saved to {}", crate::config::config_path().display()),
                Style::new().fg(theme::FAINT),
            ));
        }
        SettingsSection::Servers => {
            let name_w = 20usize;
            let host_w = width.saturating_sub(name_w + 6).clamp(10, 30);
            lines.push(Line::styled(
                format!("  {}{}DATABASES", pad("NAME", name_w + 2), pad("HOST", host_w + 2)),
                Style::new().fg(theme::MUTED).add_modifier(Modifier::BOLD),
            ));
            for (i, s) in app.cfg.servers.iter().enumerate() {
                let host = crate::config::server_of(&s.connection_string).unwrap_or_default();
                let summary = match &s.databases {
                    crate::config::DbSelection::All(_) => "all".to_string(),
                    crate::config::DbSelection::Named(v) => format!("{} selected", v.len()),
                };
                lines.push(Line::styled(
                    format!(
                        "  {}{}{summary}",
                        pad(&fit(&s.name, name_w), name_w + 2),
                        pad(&fit(&host, host_w), host_w + 2),
                    ),
                    row_style(i),
                ));
            }
            if app.cfg.servers.is_empty() {
                lines.push(Line::styled("  (none — a adds one)", Style::new().fg(theme::FAINT)));
            }
        }
        SettingsSection::Databases => {
            let name_w = 20usize;
            let host_w = width.saturating_sub(name_w + 6).clamp(10, 30);
            lines.push(Line::styled(
                format!("  {}{}DATABASE", pad("NAME", name_w + 2), pad("HOST", host_w + 2)),
                Style::new().fg(theme::MUTED).add_modifier(Modifier::BOLD),
            ));
            for (i, d) in app.cfg.databases.iter().enumerate() {
                let host = crate::config::server_of(&d.connection_string).unwrap_or_default();
                let db = crate::config::database_of(&d.connection_string).unwrap_or_default();
                lines.push(Line::styled(
                    format!(
                        "  {}{}{db}",
                        pad(&fit(&d.name, name_w), name_w + 2),
                        pad(&fit(&host, host_w), host_w + 2),
                    ),
                    row_style(i),
                ));
            }
            if app.cfg.databases.is_empty() {
                lines.push(Line::styled("  (none — a adds one)", Style::new().fg(theme::FAINT)));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), content_a);
}

// --- Source form -----------------------------------------------------------------

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn draw_confirm(f: &mut Frame, app: &App, area: Rect) {
    let Some((msg, _)) = &app.confirm else { return };
    let w = (msg.width() as u16 + 6).clamp(30, area.width.saturating_sub(4));
    let rect = centered(area, w, 3);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::ERROR))
        .title(Span::styled(" Delete ", Style::new().fg(theme::ERROR).bold()));
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block.style(Style::new().bg(theme::PANEL2)), rect);
    f.render_widget(
        Paragraph::new(Line::styled(format!(" {msg}"), Style::new().fg(theme::TEXT))),
        inner,
    );
}

fn draw_form(f: &mut Frame, app: &mut App, area: Rect) {
    let tick = app.tick;
    let Some(form) = &app.form else { return };
    let list_h = if form.kind == SourceKind::Server && !form.all_dbs && !form.db_list.is_empty() {
        form.db_list.len().min(8) as u16
    } else {
        0
    };
    let base: u16 = 2 // name + conn
        + if form.editing_id.is_none() { 1 } else { 0 } // type
        + if form.kind == SourceKind::Server { 2 } else { 0 } // fetch + all
        + list_h
        + if form.error.is_some() { 2 } else { 1 } // spacing + error
        + 1; // buttons
    let w = 76u16.min(area.width.saturating_sub(4));
    let rect = centered(area, w, base + 2);
    let title = match (form.editing_id.is_some(), form.kind) {
        (false, SourceKind::Server) => " Add server ",
        (false, SourceKind::Database) => " Add database ",
        (true, SourceKind::Server) => " Edit server ",
        (true, SourceKind::Database) => " Edit database ",
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::ACCENT_DIM))
        .title(Span::styled(title, Style::new().fg(theme::ACCENT).bold()));
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block.style(Style::new().bg(theme::PANEL2)), rect);

    let label_w = 14usize;
    let field_w = (inner.width as usize).saturating_sub(label_w + 3);
    let mut lines: Vec<Line> = vec![];
    let mut cursor: Option<(u16, u16)> = None;

    let sel_style = Style::new()
        .fg(theme::ON_ACCENT)
        .bg(theme::ACCENT_DIM)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::new().fg(theme::MUTED);
    let value_style = Style::new().fg(theme::TEXT);

    if form.editing_id.is_none() {
        let focused = form.focus == FormField::Kind;
        let val = match form.kind {
            SourceKind::Server => "‹ Server ›",
            SourceKind::Database => "‹ Database ›",
        };
        lines.push(Line::from(vec![
            Span::styled(pad(" Type", label_w), label_style),
            Span::styled(
                format!(" {val} "),
                if focused { sel_style } else { value_style },
            ),
            Span::styled("  (server = one connection, many databases)", Style::new().fg(theme::FAINT)),
        ]));
    }

    for (field, label, text) in [
        (FormField::Name, " Name", &form.name),
        (FormField::Conn, " Connection", &form.conn),
    ] {
        let focused = form.focus == field;
        let chars = text.chars().count();
        let (start, cur_off) = if focused {
            let start = form.cursor.saturating_sub(field_w.saturating_sub(1));
            (start, form.cursor - start)
        } else {
            (0, 0)
        };
        let shown: String = text.chars().skip(start).take(field_w).collect();
        let shown = if !focused && chars > field_w { fit(text, field_w) } else { shown };
        let style = if focused {
            Style::new().fg(theme::TEXT).bg(theme::BG)
        } else {
            value_style
        };
        if focused {
            cursor = Some(((label_w + 1 + cur_off) as u16, lines.len() as u16));
        }
        lines.push(Line::from(vec![
            Span::styled(pad(label, label_w), label_style),
            Span::styled(" ", Style::new()),
            Span::styled(pad(&shown, field_w), style),
        ]));
    }

    if form.kind == SourceKind::Server {
        let focused = form.focus == FormField::Fetch;
        let fetch_label = if form.fetching {
            format!(" {} Fetching… ", spinner(tick))
        } else {
            " Fetch databases ".to_string()
        };
        lines.push(Line::from(vec![
            Span::styled(pad("", label_w + 1), label_style),
            Span::styled("[", Style::new().fg(theme::FAINT)),
            Span::styled(fetch_label, if focused { sel_style } else { Style::new().fg(theme::ACCENT_DIM) }),
            Span::styled("]", Style::new().fg(theme::FAINT)),
        ]));

        let focused = form.focus == FormField::AllToggle;
        let mark = if form.all_dbs { "x" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(pad(" Databases", label_w), label_style),
            Span::styled(
                format!(" [{mark}] all "),
                if focused { sel_style } else { value_style },
            ),
            Span::styled(
                if form.all_dbs { "(every database on the server)" } else { "(pick from the list below)" },
                Style::new().fg(theme::FAINT),
            ),
        ]));

        if !form.all_dbs && !form.db_list.is_empty() {
            let focused = form.focus == FormField::DbList;
            let visible = form.db_list.len().min(8);
            let start = form
                .list_sel
                .saturating_sub(visible.saturating_sub(1))
                .min(form.db_list.len() - visible);
            for (i, (name, on)) in form.db_list.iter().enumerate().skip(start).take(visible) {
                let is_sel = focused && i == form.list_sel;
                let mark = if *on { "x" } else { " " };
                lines.push(Line::from(vec![
                    Span::styled(pad("", label_w + 1), label_style),
                    Span::styled(
                        format!("[{mark}] {}", fit(name, field_w.saturating_sub(4))),
                        if is_sel { sel_style } else { value_style },
                    ),
                ]));
            }
        } else if !form.all_dbs && form.db_list.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(pad("", label_w + 1), label_style),
                Span::styled("(fetch databases first)", Style::new().fg(theme::FAINT)),
            ]));
        }
    }

    if let Some(err) = &form.error {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!(" ✕ {}", fit(err, inner.width as usize - 4)),
            Style::new().fg(theme::ERROR),
        ));
    } else {
        lines.push(Line::raw(""));
    }

    let save_label = if form.saving {
        format!(" {} Testing… ", spinner(tick))
    } else {
        " Save ".to_string()
    };
    let save_focused = form.focus == FormField::Save;
    let cancel_focused = form.focus == FormField::Cancel;
    lines.push(Line::from(vec![
        Span::styled(pad("", label_w + 1), label_style),
        Span::styled("[", Style::new().fg(theme::FAINT)),
        Span::styled(save_label, if save_focused { sel_style } else { Style::new().fg(theme::ACCENT) }),
        Span::styled("]   [", Style::new().fg(theme::FAINT)),
        Span::styled(" Cancel ", if cancel_focused { sel_style } else { value_style }),
        Span::styled("]", Style::new().fg(theme::FAINT)),
    ]));

    f.render_widget(Paragraph::new(lines), inner);
    if let Some((cx, cy)) = cursor {
        f.set_cursor_position(Position::new(inner.x + cx, inner.y + cy));
    }
}

fn draw_help(f: &mut Frame, area: Rect) {
    let entries: &[(&str, &str)] = &[
        ("Ctrl+H/J/K/L", "switch pane (also Ctrl+arrows, Alt+HJKL); at edge: tmux"),
        ("Ctrl+R / F5 / Ctrl+Enter", "run query (also :w in vim normal mode)"),
        ("Ctrl+W, then h/j/k/l", "resize pane (saved to config)"),
        ("", ""),
        ("j/k, gg/G, Ctrl+D/U", "navigate lists & results"),
        ("/", "filter databases/tables"),
        (",", "settings: vim mode, servers & databases (not in editor)"),
        ("a / e / d", "add / edit / delete source (database list)"),
        ("Enter", "select database / preview table / open cell"),
        ("h/l, w/b, 0/$", "move between result columns"),
        ("V", "select rows; y copies JSON, Y a markdown table"),
        ("y / Y", "copy cell / row (TSV)"),
        ("r", "run the last query again"),
        ("", ""),
        ("vim mode", "i/a/o insert · v/V visual · d/c/y+motion · u/U undo/redo"),
        ("", "w/b/e f/t gg/G 0/^/$ motions · p/P paste · :w runs the query"),
        ("", "toggle in , settings"),
        ("Ctrl+E / F2", "open the query in $EDITOR"),
        ("Ctrl+N/P, Tab", "autocomplete: cycle / accept (insert mode)"),
        ("Ctrl+Backspace/Delete", "delete word (insert mode)"),
        ("Esc", "normal mode · close detail · clear filter"),
        ("", ""),
        ("q", "close detail / quit"),
        ("Ctrl+C", "quit"),
    ];
    let w = 68.min(area.width.saturating_sub(4));
    let h = (entries.len() as u16 + 2).min(area.height.saturating_sub(2));
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::ACCENT_DIM))
        .title(Span::styled(" Keys ", Style::new().fg(theme::ACCENT).bold()));
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block.style(Style::new().bg(theme::PANEL2)), rect);
    let lines: Vec<Line> = entries
        .iter()
        .map(|(key, desc)| {
            Line::from(vec![
                Span::styled(format!(" {key:<26}"), Style::new().fg(theme::ACCENT_DIM)),
                Span::styled(desc.to_string(), Style::new().fg(theme::TEXT)),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

// --- Text helpers -------------------------------------------------------------

fn spinner(tick: usize) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[(tick / 2) % FRAMES.len()]
}

/// Truncate to a display width, appending … when cut.
pub fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in text.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > width.saturating_sub(1) {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

fn pad(text: &str, width: usize) -> String {
    let w = text.width();
    if w >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - w))
    }
}

/// Take a display-column window out of a styled line (for horizontal scroll).
fn crop_spans(spans: &[(String, Color)], skip: usize, take: usize) -> Line<'static> {
    let mut out: Vec<Span> = vec![];
    let mut cur = String::new();
    let mut cur_color: Option<Color> = None;
    let mut pos = 0;
    let end = skip + take;
    'outer: for (text, color) in spans {
        for c in text.chars() {
            let w = c.width().unwrap_or(0);
            if pos + w > end {
                break 'outer;
            }
            if pos >= skip {
                if cur_color != Some(*color) {
                    if !cur.is_empty() {
                        out.push(Span::styled(
                            std::mem::take(&mut cur),
                            Style::new().fg(cur_color.unwrap()),
                        ));
                    }
                    cur_color = Some(*color);
                }
                cur.push(c);
            }
            pos += w;
        }
    }
    if !cur.is_empty() {
        out.push(Span::styled(cur, Style::new().fg(cur_color.unwrap())));
    }
    Line::from(out)
}

/// Wrap a styled line at a display width, appending the pieces to `out`.
fn wrap_spans(spans: &[(String, Color)], width: usize, out: &mut Vec<Line<'static>>) {
    if width == 0 {
        return;
    }
    let mut line: Vec<Span> = vec![];
    let mut cur = String::new();
    let mut cur_color: Option<Color> = None;
    let mut w = 0;
    let flush_span = |line: &mut Vec<Span<'static>>, cur: &mut String, color: Option<Color>| {
        if !cur.is_empty() {
            line.push(Span::styled(
                std::mem::take(cur),
                Style::new().fg(color.unwrap_or(theme::TEXT)),
            ));
        }
    };
    for (text, color) in spans {
        if cur_color != Some(*color) {
            flush_span(&mut line, &mut cur, cur_color);
            cur_color = Some(*color);
        }
        for c in text.chars() {
            let cw = c.width().unwrap_or(0);
            if w + cw > width {
                flush_span(&mut line, &mut cur, cur_color);
                out.push(Line::from(std::mem::take(&mut line)));
                w = 0;
            }
            cur.push(c);
            w += cw;
        }
    }
    flush_span(&mut line, &mut cur, cur_color);
    out.push(Line::from(line));
}
