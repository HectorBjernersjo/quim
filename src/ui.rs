use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, FilterList, FormField, FormScope, Pane, SideRow, StatusKind, GENERAL_ROWS};
use crate::config;
use crate::db::Category;
use crate::editor::{CompKind, Mode};
use crate::highlight;
use crate::theme;

pub fn draw(f: &mut Frame, app: &mut App) {
    app.tick = app.tick.wrapping_add(1);
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(theme::BG)), area);

    let lay = app.cfg.quim.layout;
    let [main, status_a] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);

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
    let left = Rect {
        x: a.x,
        y: a.y,
        width: 1,
        height: a.height,
    };
    let right = Rect {
        x: a.x + a.width - 1,
        y: a.y,
        width: 1,
        height: a.height,
    };
    let top = Rect {
        x: a.x,
        y: a.y,
        width: a.width,
        height: 1,
    };
    let bottom = Rect {
        x: a.x,
        y: a.y + a.height - 1,
        width: a.width,
        height: 1,
    };
    match app.focus {
        Pane::Databases => vec![right, bottom],
        Pane::Tables => vec![right, top],
        Pane::Editor => vec![left, bottom],
        Pane::Results => vec![if app.detail.is_some() { right } else { left }, top],
        Pane::Detail => vec![left],
    }
}

fn pane_block(title: &str, focused: bool) -> Block<'static> {
    let border = if focused {
        theme::ACCENT_DIM
    } else {
        theme::BORDER
    };
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

fn scroll_window(
    sel: usize,
    len: usize,
    height: usize,
    scroll: &mut usize,
) -> std::ops::Range<usize> {
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

/// The sidebar tree: connections, each with its databases underneath. While a
/// filter is typed the tree flattens to just the matching databases.
fn draw_db_list(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Pane::Databases;
    let title = if app.pending_lists > 0 {
        format!("Connections {}", spinner(app.tick))
    } else {
        format!("Connections ({})", app.cfg.connections.len())
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
    let width = inner.width as usize;

    let rows = app.visible_rows();
    app.db_list.sel = app.db_list.sel.min(rows.len().saturating_sub(1));
    let range = scroll_window(app.db_list.sel, rows.len(), height, &mut app.db_list.scroll);
    let flat = !app.db_list.filter.is_empty();

    for pos in range {
        let is_sel = pos == app.db_list.sel;
        let cursor = is_sel && focused;
        let mut line = match rows[pos] {
            SideRow::Conn(ci) => {
                let c = &app.cfg.connections[ci];
                let engine = c.engine();
                let open = !app.cfg.quim.collapsed.contains(&c.id);
                let known = config::supported_engine(&engine);
                let meta = if known {
                    let location = config::location_of(&engine, &c.connection_string);
                    if location.is_empty() {
                        engine.clone()
                    } else {
                        format!("{engine} · {location}")
                    }
                } else {
                    "unknown engine".to_string()
                };
                let name_style = if cursor {
                    Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD)
                } else if known {
                    Style::new().fg(theme::TEXT).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(theme::ERROR)
                };
                // Name first, then as much of "engine · host" as still fits.
                let name = fit(&c.name, width.saturating_sub(4));
                let meta_w = width.saturating_sub(name.width() + 4);
                Line::from(vec![
                    Span::styled(
                        if open { " ▾ " } else { " ▸ " },
                        Style::new().fg(theme::FAINT),
                    ),
                    Span::styled(name, name_style),
                    Span::styled(
                        if meta_w >= 6 {
                            format!("  {}", fit(&meta, meta_w.saturating_sub(2)))
                        } else {
                            String::new()
                        },
                        Style::new().fg(theme::FAINT),
                    ),
                ])
            }
            SideRow::Db(i) => {
                let entry = &app.dbs[i];
                let is_active =
                    app.active.as_ref().map(|a| a.id.as_str()) == Some(entry.id.as_str());
                // The ● marks the database queries actually run against; the
                // cursor row is shown by colour, like the table list.
                let marker = if is_active { "●" } else { " " };
                let style = if cursor {
                    Style::new().fg(theme::ACCENT)
                } else if !entry.included {
                    Style::new().fg(theme::FAINT)
                } else if is_active {
                    Style::new().fg(theme::ACCENT_DIM)
                } else {
                    Style::new().fg(theme::TEXT)
                };
                // Children sit one step in; a flat filter hit or a connection
                // that is a single database stays at the left edge.
                let indent = if flat || entry.name == entry.conn_name {
                    format!(" {marker} ")
                } else {
                    format!(" {marker}   ")
                };
                let text = if flat { &entry.label } else { &entry.name };
                Line::from(vec![
                    Span::styled(indent.clone(), Style::new().fg(theme::ACCENT_DIM)),
                    Span::styled(fit(text, width.saturating_sub(indent.width())), style),
                ])
            }
        };
        if is_sel {
            line = line.style(Style::new().bg(theme::PANEL2));
        }
        lines.push(line);
    }
    if rows.is_empty() {
        let hint = if app.cfg.connections.is_empty() {
            "  (no connections — a adds one)"
        } else {
            "  (no matches)"
        };
        lines.push(Line::styled(hint, Style::new().fg(theme::FAINT)));
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
    let range = scroll_window(
        app.tbl_list.sel,
        filtered.len(),
        height,
        &mut app.tbl_list.scroll,
    );

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
                Style::new().fg(if is_sel && focused {
                    theme::ACCENT
                } else {
                    theme::TEXT
                }),
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
    if !app.cfg.quim.vim_mode {
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
    let db = app
        .active
        .as_ref()
        .map(|a| a.label.clone())
        .unwrap_or_else(|| "—".into());
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
                let e = if row == b.0 {
                    (b.1 + 1).min(len.max(1))
                } else {
                    len.max(1)
                };
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
            f.buffer_mut().set_style(rect, Style::new().bg(theme::SEL));
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
    line.chars()
        .take(char_idx)
        .map(|c| c.width().unwrap_or(0))
        .sum()
}

fn draw_completion(f: &mut Frame, app: &App, editor_inner: Rect, cur_x: usize) {
    let Some(comp) = &app.editor.completion else {
        return;
    };
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
    let y = if below {
        cursor_row + 1
    } else {
        cursor_row.saturating_sub(visible as u16)
    };
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
                if selected {
                    style
                } else {
                    Style::new().fg(tag_color).bg(theme::PANEL2)
                },
            ),
        ]));
    }
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).style(Style::new().bg(theme::PANEL2)),
        rect,
    );
}

pub fn display_text(cell: &Option<String>, category: Category) -> String {
    let Some(value) = cell else {
        return "NULL".into();
    };
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
        Some(v) if v.error.is_none() && v.rows.is_empty() && !v.cols.is_empty() => {
            format!("Results · 0 rows · col {}/{}", v.sel_col + 1, v.cols.len())
        }
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
        let hint = if app.dbs.is_empty() && app.pending_lists == 0 {
            vec![
                Line::raw(""),
                Line::styled("  No connections yet.", Style::new().fg(theme::MUTED)),
                Line::raw(""),
                Line::styled(
                    "  Press a in the sidebar to add one,",
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
    let body_h = (inner.height as usize).saturating_sub(3); // name + type + separator

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
    let mut types = vec![Span::raw(" ".repeat(rownum_w + 2))];
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
        // The header marks the cursor column, which is the only cue there is
        // when the result has no rows to put a cell cursor on.
        let sel = c == view.sel_col && focused;
        header.push(Span::styled(
            pad(&fit(&view.cols[c].name, w), w) + "  ",
            if sel {
                Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(theme::MUTED).add_modifier(Modifier::BOLD)
            },
        ));
        types.push(Span::styled(
            pad(&fit(&view.cols[c].ty, w), w) + "  ",
            Style::new()
                .fg(if sel { theme::ACCENT_DIM } else { theme::FAINT })
                .add_modifier(Modifier::ITALIC),
        ));
        sep.push(Span::styled(
            if sel {
                "━".repeat(w) + "  "
            } else {
                "─".repeat(w + 2)
            },
            Style::new().fg(if sel {
                theme::ACCENT
            } else {
                theme::BORDER_SOFT
            }),
        ));
        used += w + 2;
        if used >= width {
            break;
        }
    }

    let vsel = view.vsel_range();
    let needle = view.search.to_lowercase();
    let search_hl = Style::new()
        .fg(theme::SEARCH_FG)
        .bg(theme::SEARCH_BG)
        .add_modifier(Modifier::BOLD);
    let mut lines = vec![Line::from(header), Line::from(types), Line::from(sep)];
    if view.rows.is_empty() {
        lines.push(Line::styled(
            format!("{}(no rows)", " ".repeat(rownum_w + 2)),
            Style::new().fg(theme::MUTED),
        ));
    }
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
            if needle.is_empty() {
                spans.push(Span::styled(text, style));
            } else {
                spans.extend(highlight_spans(&text, &needle, style, search_hl));
            }
            spans.push(Span::styled(
                "  ",
                if is_sel_row {
                    Style::new().bg(row_bg)
                } else {
                    Style::new()
                },
            ));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Pane::Detail;
    let Some(detail) = &mut app.detail else {
        return;
    };
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
    if let Some(cmd) = app
        .editor
        .vim
        .cmdline
        .as_ref()
        .filter(|_| app.focus == Pane::Editor)
    {
        let text = format!(" :{cmd}");
        let cursor_x = area.x + text.width() as u16;
        f.render_widget(
            Paragraph::new(Line::styled(text, Style::new().fg(theme::TEXT)))
                .style(Style::new().bg(theme::BG)),
            area,
        );
        f.set_cursor_position(Position::new(
            cursor_x.min(area.x + area.width.saturating_sub(1)),
            area.y,
        ));
        return;
    }
    // The results search takes over the status bar while its term is typed.
    if let Some(view) = app
        .view
        .as_ref()
        .filter(|v| v.searching && app.focus == Pane::Results)
    {
        let head = format!(" /{}", view.search);
        // The cursor sits right after the typed term; the live count trails it.
        let cursor_x = area.x + head.width() as u16;
        let count = if view.search.is_empty() {
            String::new()
        } else if view.search_hits.is_empty() {
            "   no matches".into()
        } else {
            let pos = view.search_index().map(|i| i + 1).unwrap_or(0);
            format!("   [{pos}/{}]", view.search_hits.len())
        };
        let line = Line::from(vec![
            Span::styled(head, Style::new().fg(theme::TEXT)),
            Span::styled(count, Style::new().fg(theme::FAINT)),
        ]);
        f.render_widget(Paragraph::new(line).style(Style::new().bg(theme::BG)), area);
        f.set_cursor_position(Position::new(
            cursor_x.min(area.x + area.width.saturating_sub(1)),
            area.y,
        ));
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
        if app.settings.is_some() {
            (
                left,
                color,
                "j/k move · Space/Enter toggle · q close ".to_string(),
            )
        } else {
            let hints = match app.focus {
                Pane::Editor if app.cfg.quim.vim_mode && app.editor.mode != Mode::Insert => {
                    "i insert · v visual · u/U undo/redo · ^R/:w run"
                }
                Pane::Editor => "^R run · ^E/F2 $EDITOR · ^N complete",
                Pane::Results if app.view.as_ref().is_some_and(|v| v.vsel.is_some()) => {
                    "y JSON · Y markdown · j/k extend · Esc cancel"
                }
                Pane::Results if app.view.as_ref().is_some_and(|v| !v.search.is_empty()) => {
                    "/ search · n/p next/prev · Esc clear · Enter detail · y/Y yank"
                }
                Pane::Results => "Enter detail · / search · V rows · y/Y yank · r rerun · ? help",
                Pane::Detail => "j/k scroll · y copy · q close",
                Pane::Databases => "Enter select · a add · e edit · d hide/delete · t test · r all",
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
    let sel_row = {
        let st = app.settings.as_mut().unwrap();
        st.row = st.row.min(GENERAL_ROWS - 1);
        st.row
    };

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::ACCENT_DIM))
        .title(Span::styled(
            " Settings ",
            Style::new().fg(theme::ACCENT).bold(),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 36 || inner.height < 5 {
        return;
    }

    let width = inner.width as usize;
    let sel_style = Style::new()
        .fg(theme::TEXT)
        .bg(theme::PANEL2)
        .add_modifier(Modifier::BOLD);
    let vim_on = app.cfg.quim.vim_mode;
    let rows = [
        (
            format!(
                "Vim mode            [{}] {}",
                if vim_on { "x" } else { " " },
                if vim_on { "on" } else { "off" }
            ),
            "modal editing in the query pane",
        ),
        (
            "Reset pane layout".to_string(),
            "clear the sizes saved with Ctrl+W",
        ),
    ];
    let mut lines: Vec<Line> = vec![Line::raw("")];
    for (i, (label, hint)) in rows.iter().enumerate() {
        let style = if i == sel_row {
            sel_style
        } else {
            Style::new().fg(theme::TEXT)
        };
        lines.push(Line::from(vec![
            Span::styled(pad(&format!("  {label}"), 32.min(width)), style),
            Span::styled(format!("  {hint}"), Style::new().fg(theme::FAINT)),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  Connections live in the sidebar — a/e/d/t there.",
        Style::new().fg(theme::FAINT),
    ));
    lines.push(Line::styled(
        format!("  Saved to {}", crate::config::config_path().display()),
        Style::new().fg(theme::FAINT),
    ));
    f.render_widget(Paragraph::new(lines), inner);
}

// --- Connection form -----------------------------------------------------------

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn draw_confirm(f: &mut Frame, app: &App, area: Rect) {
    let Some((msg, action)) = &app.confirm else {
        return;
    };
    // Hiding is reversible, deleting is not — don't dress them the same.
    let (title, color) = if action.is_delete() {
        (" Delete ", theme::ERROR)
    } else {
        (" Hide ", theme::ACCENT)
    };
    let w = (msg.width() as u16 + 6).clamp(30, area.width.saturating_sub(4));
    let rect = centered(area, w, 3);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        .title(Span::styled(title, Style::new().fg(color).bold()));
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block.style(Style::new().bg(theme::PANEL2)), rect);
    f.render_widget(
        Paragraph::new(Line::styled(
            format!(" {msg}"),
            Style::new().fg(theme::TEXT),
        )),
        inner,
    );
}

/// Add/edit a connection: a name, a connection string, and a line showing what
/// quim read out of that string. The engine row is only a knob when the string
/// is too exotic to read.
fn draw_form(f: &mut Frame, app: &mut App, area: Rect) {
    let tick = app.tick;
    let Some(form) = &app.form else { return };
    let scoped = form.fields().contains(&FormField::Scope);
    let list_h = if scoped && form.scope == FormScope::Pick {
        form.db_list.len().min(10) as u16
    } else {
        0
    };
    let height: u16 = 2 // name + conn
        + 1 // engine
        + u16::from(scoped)
        + list_h
        + if form.error.is_some() { 2 } else { 1 } // spacing + error
        + 1; // buttons
    let w = 76u16.min(area.width.saturating_sub(4));
    let rect = centered(area, w, height + 2);
    let title = if form.editing_id.is_some() {
        " Edit connection "
    } else {
        " Add connection "
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
        let shown = if !focused && chars > field_w {
            fit(text, field_w)
        } else {
            shown
        };
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

    // What the connection string turned out to be, live as it is typed.
    let engine = form.engine();
    let (summary, summary_style) = if form.conn.trim().is_empty() {
        (
            "postgres:// URL · Server=…;Database=… · path to a .db file".to_string(),
            Style::new().fg(theme::FAINT),
        )
    } else if !config::supported_engine(&engine) {
        (
            "unrecognised — pick the engine here".to_string(),
            Style::new().fg(theme::ERROR),
        )
    } else {
        let location = config::location_of(&engine, &form.conn);
        let database = config::database_of_engine(&engine, &form.conn);
        let mut parts = vec![engine.clone()];
        if !location.is_empty() {
            parts.push(location);
        }
        parts.push(match &database {
            Some(db) => db.clone(),
            None => "all databases".into(),
        });
        (parts.join(" · "), Style::new().fg(theme::FAINT))
    };
    let focused = form.focus == FormField::Engine;
    let shown_engine = form
        .engine_override
        .clone()
        .unwrap_or_else(|| "auto".into());
    lines.push(Line::from(vec![
        Span::styled(pad(" Engine", label_w), label_style),
        Span::styled(
            format!(" ‹ {shown_engine} › "),
            if focused { sel_style } else { value_style },
        ),
        Span::styled(format!("  {summary}"), summary_style),
    ]));

    // Which of the host's databases to list. Absent for engines that have only
    // the one, where there is nothing to choose.
    if form.fields().contains(&FormField::Scope) {
        let focused = form.focus == FormField::Scope;
        let (word, note) = match form.scope {
            FormScope::Auto => (
                "auto",
                match config::database_of_engine(&engine, &form.conn) {
                    Some(db) => format!("just {db}, as the connection string says"),
                    None => "every database on the host".to_string(),
                },
            ),
            FormScope::All => ("all", "every database on the host".to_string()),
            FormScope::Pick if form.fetching => ("pick", format!("{} Listing…", spinner(tick))),
            FormScope::Pick => (
                "pick",
                format!(
                    "{} of {} ticked · Space toggles, a flips all",
                    form.db_list.iter().filter(|(_, on)| *on).count(),
                    form.db_list.len()
                ),
            ),
        };
        lines.push(Line::from(vec![
            Span::styled(pad(" Databases", label_w), label_style),
            Span::styled(
                format!(" ‹ {word} › "),
                if focused { sel_style } else { value_style },
            ),
            Span::styled(format!("  {note}"), Style::new().fg(theme::FAINT)),
        ]));

        if form.scope == FormScope::Pick && !form.db_list.is_empty() {
            let focused = form.focus == FormField::DbList;
            let visible = form.db_list.len().min(10);
            let start = form
                .list_sel
                .saturating_sub(visible.saturating_sub(1))
                .min(form.db_list.len() - visible);
            for (i, (name, on)) in form.db_list.iter().enumerate().skip(start).take(visible) {
                let is_sel = focused && i == form.list_sel;
                lines.push(Line::from(vec![
                    Span::styled(pad("", label_w + 1), label_style),
                    Span::styled(
                        format!(
                            "[{}] {}",
                            if *on { "x" } else { " " },
                            fit(name, field_w.saturating_sub(4))
                        ),
                        if is_sel { sel_style } else { value_style },
                    ),
                ]));
            }
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
        Span::styled(
            save_label,
            if save_focused {
                sel_style
            } else {
                Style::new().fg(theme::ACCENT)
            },
        ),
        Span::styled("]  [", Style::new().fg(theme::FAINT)),
        Span::styled(
            " Cancel ",
            if cancel_focused {
                sel_style
            } else {
                Style::new().fg(theme::MUTED)
            },
        ),
        Span::styled("]", Style::new().fg(theme::FAINT)),
    ]));

    f.render_widget(Paragraph::new(lines), inner);
    if let Some((x, y)) = cursor {
        f.set_cursor_position(Position::new(
            inner.x + x.min(inner.width.saturating_sub(1)),
            inner.y + y,
        ));
    }
}

fn draw_help(f: &mut Frame, area: Rect) {
    let entries: &[(&str, &str)] = &[
        (
            "Ctrl+H/J/K/L",
            "switch pane (also Ctrl+arrows, Alt+HJKL); at edge: tmux",
        ),
        (
            "Ctrl+R / F5 / Ctrl+Enter",
            "run query (also :w in vim normal mode)",
        ),
        ("Ctrl+W, then h/j/k/l", "resize pane (saved to config)"),
        ("", ""),
        ("j/k, gg/G, Ctrl+D/U", "navigate lists & results"),
        (
            "/",
            "filter databases/tables · search results (n/p next/prev)",
        ),
        (",", "settings: vim mode, pane layout (not in editor)"),
        ("", ""),
        ("a / e", "add / edit a connection (sidebar)"),
        ("d", "on a database: hide it · on a connection: delete it"),
        ("t", "test the connection under the cursor"),
        ("h / l, Space", "fold a connection open or shut"),
        ("Space (on a database)", "hide it / show it again"),
        ("r", "list every database on the host again"),
        ("Enter", "select database / preview table / open cell"),
        ("h/l, w/b, 0/$", "move between result columns"),
        ("V", "select rows; y copies JSON, Y a markdown table"),
        ("y / Y", "copy cell / row (TSV)"),
        ("r", "run the last query again"),
        ("", ""),
        (
            "vim mode",
            "i/a/o insert · v/V visual · d/c/y+motion · u/U undo/redo",
        ),
        (
            "",
            "w/b/e f/t gg/G 0/^/$ motions · p/P paste · :w runs the query",
        ),
        ("", "toggle it in , settings"),
        ("Ctrl+E / F2", "open the query in $EDITOR"),
        (
            "Ctrl+N/P, Tab",
            "autocomplete: cycle / accept (insert mode)",
        ),
        ("Ctrl+Backspace/Delete", "delete word (insert mode)"),
        ("Esc", "normal mode · close detail · clear filter"),
        ("", ""),
        ("q", "close detail / quit"),
        ("Ctrl+C", "quit"),
    ];
    let w = 68.min(area.width.saturating_sub(4));
    let h = (entries.len() as u16 + 2).min(area.height.saturating_sub(2));
    let rect = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::ACCENT_DIM))
        .title(Span::styled(
            " Keys ",
            Style::new().fg(theme::ACCENT).bold(),
        ));
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

/// Split `text` into spans, styling case-insensitive occurrences of `needle`
/// with `hl` and the rest with `base`. `needle` must already be lowercased.
fn highlight_spans(text: &str, needle: &str, base: Style, hl: Style) -> Vec<Span<'static>> {
    if needle.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let chars: Vec<char> = text.chars().collect();
    let nlen = needle.chars().count();
    let mut spans: Vec<Span<'static>> = vec![];
    let mut buf = String::new();
    let mut i = 0;
    while i < chars.len() {
        if i + nlen <= chars.len() {
            let cand: String = chars[i..i + nlen].iter().collect();
            if cand.to_lowercase() == needle {
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut buf), base));
                }
                spans.push(Span::styled(cand, hl));
                i += nlen;
                continue;
            }
        }
        buf.push(chars[i]);
        i += 1;
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, base));
    }
    spans
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::QueryView;
    use crate::config::Config;
    use crate::db::ColMeta;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn empty_result_view() -> QueryView {
        let cols = vec![
            ColMeta {
                name: "id".into(),
                ty: "int4".into(),
                category: Category::Num,
            },
            ColMeta {
                name: "name".into(),
                ty: "varchar".into(),
                category: Category::Str,
            },
        ];
        let widths = vec![4, 7];
        QueryView {
            cols,
            rows: vec![],
            widths,
            sel_row: 0,
            sel_col: 0,
            row_off: 0,
            col_off: 0,
            vsel: None,
            error: None,
            search: String::new(),
            searching: false,
            search_hits: vec![],
        }
    }

    /// The drawn screen as one string per row.
    fn screen(app: &mut App) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(100)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    fn render(view: QueryView) -> Vec<String> {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Config::default(), tx);
        app.view = Some(view);
        app.focus = Pane::Results;
        screen(&mut app)
    }

    #[test]
    fn the_sidebar_draws_connections_as_a_tree() {
        let _cfg = config::use_scratch_config_dir();
        let raw = r#"{"connections": [
            {"id": "conn_sql", "name": "Local server",
             "connectionString": "Server=sql01,1433;User Id=sa"},
            {"id": "conn_lite", "name": "notes",
             "connectionString": "/tmp/notes.db", "databases": ["main"]}
        ]}"#;
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(serde_json::from_str(raw).unwrap(), tx);
        app.cfg.quim.layout.sidebar_width = Some(46); // room for the meta column
        app.on_db_response(crate::db::DbResponse::Databases {
            conn_id: "conn_sql".into(),
            result: Ok(vec!["AppDb".into(), "master".into()]),
        });

        let text = screen(&mut app);
        let side: Vec<&String> = text.iter().take(8).collect();
        let line = |needle: &str| {
            side.iter()
                .find(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("no sidebar line with {needle:?} in {side:#?}"))
        };
        // The host says where it points and can be folded shut.
        assert!(line("Local server").contains('▾'));
        assert!(line("Local server").contains("mssql · sql01,1433"));
        // Its databases sit under it, indented past the connection name.
        // Counted in characters: the border and ▾ are multi-byte.
        let indent = |needle: &str| {
            let l = line(needle);
            l[..l.find(needle).unwrap()].chars().count()
        };
        assert!(indent("AppDb") > indent("Local server"));
        // A single-database connection is one row, no disclosure of its own.
        assert!(!line("notes").contains('▾'));
        assert_eq!(indent("notes"), indent("Local server"));
    }

    #[test]
    fn empty_result_still_renders_column_names_and_types() {
        let text = render(empty_result_view());

        let header = text
            .iter()
            .position(|line| line.contains("id") && line.contains("name"))
            .expect("header row with column names");
        // The type row sits directly under the names, aligned to the same columns.
        assert!(text[header + 1].contains("int4"));
        assert!(text[header + 1].contains("varchar"));
        assert_eq!(
            text[header].find("name"),
            text[header + 1].find("varchar"),
            "type must line up under its column name"
        );
        assert!(text.iter().any(|line| line.contains("(no rows)")));
        assert!(text.iter().any(|line| line.contains("0 rows")));
    }

    /// With no rows there is no cell cursor, so the heavy rule under the header
    /// is the only thing showing which column h/l moved to.
    #[test]
    fn cursor_column_is_marked_in_the_header_of_an_empty_result() {
        // Box-drawing characters are multi-byte, so positions are counted in
        // characters — a byte offset would not line up between rows.
        let column_of = |line: &str, needle: &str| {
            line.find(needle)
                .map(|byte| line[..byte].chars().count())
                .expect("needle on line")
        };
        let rule_at = |view: QueryView| {
            let text = render(view);
            let header = text
                .iter()
                .position(|line| line.contains("id") && line.contains("name"))
                .expect("header row with column names");
            let rule = column_of(&text[header + 2], "━");
            (
                column_of(&text[header], "id"),
                column_of(&text[header], "name"),
                rule,
            )
        };

        let (id_at, name_at, rule) = rule_at(empty_result_view());
        assert_eq!(rule, id_at, "column 0 selected");

        let mut view = empty_result_view();
        view.sel_col = 1;
        let (_, _, rule) = rule_at(view);
        assert_eq!(rule, name_at, "column 1 selected");
    }
}
