mod app;
mod clipboard;
mod config;
mod db;
mod editor;
mod highlight;
mod theme;
mod ui;
mod vim;

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::{cursor, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::{Action, App};

static KEYBOARD_ENHANCED: AtomicBool = AtomicBool::new(false);

type Tui = Terminal<CrosstermBackend<io::Stdout>>;

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("quim — TUI for SQL against MSSQL, Postgres and SQLite");
        println!("\n  quim            start the TUI");
        println!("  quim --check    test config, connection, schema and SELECT 1 without the TUI");
        println!("\nConfig: {}", config::config_path().display());
        return Ok(());
    }
    if args.iter().any(|a| a == "--check") {
        return check();
    }

    // vim-tmux-navigator only passes C-hjkl through to processes whose comm
    // matches its vim pattern; the pattern allows any "<prefix>/" before the
    // name, so "quim/view" matches stock configs while staying recognizable in
    // process lists. quim hands the keys back to tmux at pane edges.
    #[cfg(target_os = "linux")]
    let _ = std::fs::write("/proc/self/comm", "quim/view");

    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let (req_tx, resp_rx) = db::spawn_worker();
    let mut app = App::new(cfg, req_tx);

    let mut terminal = init_terminal()?;
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        default_hook(info);
    }));

    let result = (|| -> io::Result<()> {
        // 0 = terminal default, 1 = bar (insert), 2 = block (normal/visual)
        let mut cursor_shape: u8 = 0;
        loop {
            while let Ok(resp) = resp_rx.try_recv() {
                app.on_db_response(resp);
            }
            terminal.draw(|f| ui::draw(f, &mut app))?;
            let want = if app.focus == app::Pane::Editor && app.cfg.quim.vim_mode {
                match app.editor.mode {
                    editor::Mode::Insert => 1,
                    _ => 2,
                }
            } else {
                0
            };
            if want != cursor_shape {
                cursor_shape = want;
                let style = match want {
                    1 => cursor::SetCursorStyle::SteadyBar,
                    2 => cursor::SetCursorStyle::SteadyBlock,
                    _ => cursor::SetCursorStyle::DefaultUserShape,
                };
                execute!(io::stdout(), style)?;
            }
            if !event::poll(Duration::from_millis(50))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => match app.on_key(key) {
                    Action::Quit => break,
                    Action::ExternalEdit => {
                        external_edit(&mut terminal, &mut app)?;
                        cursor_shape = 0; // restore_terminal reset the shape
                    }
                    Action::None => {}
                },
                Event::Paste(text) => app.on_paste(text),
                _ => {}
            }
        }
        Ok(())
    })();

    restore_terminal()?;
    result
}

fn init_terminal() -> io::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    if matches!(supports_keyboard_enhancement(), Ok(true)) {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        KEYBOARD_ENHANCED.store(true, Ordering::Relaxed);
    }
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal() -> io::Result<()> {
    let mut stdout = io::stdout();
    if KEYBOARD_ENHANCED.swap(false, Ordering::Relaxed) {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }
    execute!(
        stdout,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::SetCursorStyle::DefaultUserShape,
        cursor::Show
    )?;
    disable_raw_mode()
}

/// Suspend the TUI, open the query in $EDITOR (like lazygit), read it back.
fn external_edit(terminal: &mut Tui, app: &mut App) -> io::Result<()> {
    let path = std::env::temp_dir().join("quim_query.sql");
    std::fs::write(&path, app.editor.text())?;

    restore_terminal()?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} '{}'", path.display()))
        .status();

    // Re-enter the TUI regardless of how the editor exited.
    init_terminal()?;
    terminal.clear()?;

    match status {
        Ok(s) if s.success() => {
            if let Ok(text) = std::fs::read_to_string(&path) {
                app.editor.set_text(text.trim_end());
            }
        }
        Ok(_) => app.set_status("Editor aborted — query kept".into(), app::StatusKind::Info),
        Err(e) => app.set_status(
            format!("✕ Could not launch $EDITOR: {e}"),
            app::StatusKind::Err,
        ),
    }
    Ok(())
}

/// Headless sanity check: config → connect → list → schema → SELECT 1.
fn check() -> io::Result<()> {
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("✕ {e}");
            std::process::exit(1);
        }
    };
    println!("config: {}", config::config_path().display());
    println!("{} connection(s)", cfg.connections.len());
    if cfg.connections.is_empty() {
        println!("No connections — add one in quim (press a).");
        return Ok(());
    }

    let (req_tx, resp_rx) = db::spawn_worker();
    let recv = |label: &str| -> db::DbResponse {
        match resp_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(r) => r,
            Err(_) => {
                eprintln!("✕ timeout: {label}");
                std::process::exit(1);
            }
        }
    };

    let mut app = App::new(cfg, req_tx.clone());
    while app.pending_lists > 0 {
        let resp = recv("database listing");
        app.on_db_response(resp);
    }
    println!(
        "databases: {}",
        app.dbs
            .iter()
            .map(|d| d.label.clone())
            .collect::<Vec<_>>()
            .join(", ")
    );

    // App::new already picked a database and asked for its schema — check the
    // same one rather than guessing at the head of the list.
    let Some(first) = app.active.clone().or_else(|| app.dbs.first().cloned()) else {
        println!("No databases to test against.");
        return Ok(());
    };
    print!("schema for {} … ", first.label);
    io::stdout().flush()?;
    loop {
        match recv("schema") {
            db::DbResponse::Schema { result, .. } => {
                match result {
                    Ok(tables) => println!("✓ {} tables", tables.len()),
                    Err(e) => println!("✕ {e}"),
                }
                break;
            }
            other => app.on_db_response(other),
        }
    }

    print!("test query … ");
    io::stdout().flush()?;
    let _ = req_tx.send(db::DbRequest::Query {
        engine: first.engine.clone(),
        conn: first.conn.clone(),
        database: first.database.clone(),
        sql: "SELECT 1 AS one, 'text' AS text, NULL AS nothing".into(),
    });
    loop {
        match recv("query") {
            db::DbResponse::Query(out) => {
                match out.error {
                    None => {
                        println!(
                            "✓ {} columns, {} row(s), {} ms",
                            out.columns.len(),
                            out.rows.len(),
                            out.elapsed_ms
                        );
                        for (i, col) in out.columns.iter().enumerate() {
                            let val = out.rows.first().and_then(|r| r.get(i).cloned().flatten());
                            println!("  {} = {}", col.name, val.unwrap_or_else(|| "NULL".into()));
                        }
                    }
                    Some(e) => println!("✕ {e}"),
                }
                break;
            }
            other => app.on_db_response(other),
        }
    }
    Ok(())
}
