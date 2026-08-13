mod app;
mod editor;
mod git;
mod ui;

use std::{io, path::PathBuf, time::{Duration, Instant}};

use anyhow::{Context, Result};
use app::{App, DiffMode, Focus};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

fn main() -> Result<()> {
    let repo_path = find_git_root().context(
        "Not inside a git repository. Run the-diff from within a git repo.",
    )?;

    let mut terminal = enter_tui()?;
    let result = run(&mut terminal, repo_path);
    leave_tui(&mut terminal)?;
    result
}

type Tui = Terminal<CrosstermBackend<io::Stdout>>;

/// Take over the terminal: raw mode, alternate screen, mouse reporting.
fn enter_tui() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

/// Hand the terminal back, in the state an editor or a shell expects it.
fn leave_tui(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Suspend the TUI, run the editor, and take the terminal back afterwards.
///
/// The editor draws over the whole screen and reads keys itself, so it cannot
/// share raw mode or the alternate screen with us. The reload afterwards picks
/// up whatever was saved.
fn edit_and_resume(terminal: &mut Tui, app: &mut App, path: &str, line: u32) {
    if let Err(e) = leave_tui(terminal) {
        app.status = format!("Could not release the terminal: {e}");
        return;
    }

    let outcome = editor::open(&app.repo_path.clone(), path, line);

    // Reclaim the terminal before reporting anything, so an error message has
    // somewhere to appear. A failure here leaves nothing to draw on, so quit.
    match enter_tui() {
        Ok(fresh) => *terminal = fresh,
        Err(e) => {
            app.status = format!("Could not restore the terminal: {e}");
            app.should_quit = true;
            return;
        }
    }
    let _ = terminal.clear();

    app.status = match outcome {
        Ok(()) => format!("Edited {path}"),
        Err(e) => format!("Editor failed: {e}"),
    };
    app.reload();
}

fn run(terminal: &mut Tui, repo_path: PathBuf) -> Result<()> {
    let mut app = App::new(repo_path)?;
    let mut last_reload = Instant::now();
    const AUTO_RELOAD: Duration = Duration::from_secs(10);

    loop {
        terminal.draw(|f| ui::render(f, &app))?;

        if !event::poll(Duration::from_millis(200))? {
            // Never reload while a confirmation is open: reload() resets
            // selected_hunk, which would silently repoint the pending action at
            // a different hunk than the one the dialog is asking about.
            if app.pending.is_none() && last_reload.elapsed() >= AUTO_RELOAD {
                app.reload();
                last_reload = Instant::now();
            }
            continue;
        }

        let ev = event::read()?;

        // Any input counts as activity, so the reload below only fires once the
        // user has actually stopped interacting.
        last_reload = Instant::now();

        let diff_height = terminal
            .size()
            .map(|s| s.height.saturating_sub(4) as usize)
            .unwrap_or(20);

        // A confirmation dialog is modal: y runs the action, anything else
        // cancels it, and scroll events are swallowed so the diff underneath
        // cannot move while the dialog describes a specific hunk.
        if app.pending.is_some() {
            match ev {
                // Press only: on platforms that also report key releases, the
                // release of the key that opened the dialog would cancel it.
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => app.confirm(),
                    _ => app.cancel(),
                },
                _ => {}
            }
            continue;
        }

        // Mouse scroll anywhere in the window scrolls the diff panel
        if let Event::Mouse(mouse) = ev {
            match mouse.kind {
                MouseEventKind::ScrollUp => app.scroll_up(),
                MouseEventKind::ScrollDown => app.scroll_down(diff_height),
                _ => {}
            }
            if app.should_quit { break; }
            continue;
        }

        if let Event::Key(key) = ev {
            match key.code {
                // Quit
                KeyCode::Char('q') | KeyCode::Char('Q') => app.should_quit = true,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.should_quit = true
                }

                // Panel cycling
                KeyCode::Tab => app.cycle_focus(),
                KeyCode::Enter => app.drill_in(),
                KeyCode::Esc => app.drill_out(),

                // Navigation — line scroll in diff, file nav in list panels
                KeyCode::Up | KeyCode::Char('k') => match app.focus {
                    Focus::Diff if app.diff_mode == DiffMode::Line => {
                        app.line_up();
                        app.ensure_line_visible(diff_height);
                    }
                    Focus::Diff => app.scroll_up(),
                    _ => app.file_up(),
                },
                KeyCode::Down | KeyCode::Char('j') => match app.focus {
                    Focus::Diff if app.diff_mode == DiffMode::Line => {
                        app.line_down();
                        app.ensure_line_visible(diff_height);
                    }
                    Focus::Diff => app.scroll_down(diff_height),
                    _ => app.file_down(),
                },

                // Hunk jumping within the diff panel
                KeyCode::Char('[') => app.hunk_up(),
                KeyCode::Char(']') => app.hunk_down(),

                // Page scroll
                KeyCode::PageUp => {
                    for _ in 0..diff_height / 2 { app.scroll_up(); }
                }
                KeyCode::PageDown => {
                    for _ in 0..diff_height / 2 { app.scroll_down(diff_height); }
                }

                // Reload
                // The timer was already reset above, when this key was read
                KeyCode::Char('r') => app.reload(),

                // s/S: stage (unstaged context only)
                KeyCode::Char('s') => app.stage_action(),
                KeyCode::Char('S') => app.stage_file_action(),

                // u/U: unstage (staged context only)
                KeyCode::Char('u') => app.unstage_action(),
                KeyCode::Char('U') => app.unstage_file_action(),

                // e: open the line under the cursor in $EDITOR (line mode)
                KeyCode::Char('e') => app.open_editor_action(),

                // d/D: discard (unstaged context only)
                KeyCode::Char('d') => app.discard_action(),
                KeyCode::Char('D') => app.discard_file_action(),

                _ => {}
            }
        }

        // Done outside the key match so the borrow of `app` ends first, and so
        // the screen is only torn down once per pass through the loop.
        if let Some(target) = app.editor_request.take() {
            edit_and_resume(terminal, &mut app, &target.path, target.line);
            last_reload = Instant::now();
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

fn find_git_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        let parent = dir.parent()?.to_path_buf();
        dir = parent;
    }
}
