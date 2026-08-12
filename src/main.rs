mod app;
mod git;
mod ui;

use std::{io, path::PathBuf, time::{Duration, Instant}};

use anyhow::{Context, Result};
use app::{App, Focus};
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

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, repo_path);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    repo_path: PathBuf,
) -> Result<()> {
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
                KeyCode::Enter => app.enter_diff(),
                KeyCode::Esc => app.exit_diff(),

                // Navigation — line scroll in diff, file nav in list panels
                KeyCode::Up | KeyCode::Char('k') => {
                    if app.focus == Focus::Diff {
                        app.scroll_up();
                    } else {
                        app.file_up();
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if app.focus == Focus::Diff {
                        app.scroll_down(diff_height);
                    } else {
                        app.file_down();
                    }
                }

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
                KeyCode::Char('r') => { app.reload(); last_reload = Instant::now(); }

                // s/S: stage (unstaged context only)
                KeyCode::Char('s') => app.stage_action(),
                KeyCode::Char('S') => app.stage_file_action(),

                // u/U: unstage (staged context only)
                KeyCode::Char('u') => app.unstage_action(),
                KeyCode::Char('U') => app.unstage_file_action(),

                // d/D: discard (unstaged context only)
                KeyCode::Char('d') => app.discard_action(),
                KeyCode::Char('D') => app.discard_file_action(),

                _ => {}
            }
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
