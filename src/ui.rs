use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::app::{App, DiffSource, Focus, Pending};
use crate::git::{FileKind, LineKind};

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let outer = Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).split(area);

    let main = Layout::horizontal([Constraint::Percentage(28), Constraint::Percentage(72)])
        .split(outer[0]);

    // Left column: unstaged (top) + staged (bottom)
    let left = Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(main[0]);

    render_file_panel(
        frame,
        &app.unstaged_files,
        app.unstaged_sel,
        app.focus == Focus::Unstaged,
        " Unstaged ",
        true,
        left[0],
    );

    render_file_panel(
        frame,
        &app.staged_files,
        app.staged_sel,
        app.focus == Focus::Staged,
        " Staged ",
        false,
        left[1],
    );

    render_diff(frame, app, main[1]);
    render_footer(frame, app, outer[1]);

    // Drawn last so it sits above every panel
    if let Some(pending) = &app.pending {
        render_confirm(frame, pending, area);
    }
}

/// Centre a `width` x `height` box inside `area`, shrinking to fit if needed.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn render_confirm(frame: &mut Frame, pending: &Pending, area: Rect) {
    const DANGER: Color = Color::Rgb(210, 90, 90);

    let widest = pending.lines.iter().map(|l| l.len()).max().unwrap_or(0);
    // Body + blank line + key hints, plus borders and horizontal padding
    let width = (widest.max(34) + 6) as u16;
    let height = (pending.lines.len() + 4) as u16;

    let mut body: Vec<Line> = Vec::new();
    for line in &pending.lines {
        body.push(Line::from(Span::styled(
            format!("  {line}"),
            Style::default().fg(Color::Rgb(220, 220, 220)),
        )));
    }
    body.push(Line::from(""));
    body.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(" y ", Style::default().fg(Color::Rgb(255, 230, 230)).bg(DANGER).add_modifier(Modifier::BOLD)),
        Span::styled(" confirm    ", Style::default().fg(Color::Rgb(160, 160, 160))),
        Span::styled(" n ", Style::default().fg(Color::Rgb(220, 220, 220)).bg(Color::Rgb(60, 60, 60))),
        Span::styled(" cancel", Style::default().fg(Color::Rgb(160, 160, 160))),
    ]));

    let rect = centered_rect(width, height, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .title(Span::styled(
                    pending.title.clone(),
                    Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(DANGER))
                .style(Style::default().bg(Color::Rgb(20, 12, 12))),
        ),
        rect,
    );
}

fn render_file_panel(
    frame: &mut Frame,
    files: &[crate::git::ChangedFile],
    selected: usize,
    focused: bool,
    title: &str,
    show_untracked_sep: bool,
    area: Rect,
) {
    let border_style = focused_border(focused);

    let untracked_start = if show_untracked_sep {
        files.iter().position(|f| f.kind == FileKind::Untracked)
    } else {
        None
    };

    let items: Vec<ListItem> = files
        .iter()
        .enumerate()
        .flat_map(|(i, f)| {
            let mut rows: Vec<ListItem> = Vec::new();

            if Some(i) == untracked_start {
                rows.push(
                    ListItem::new(Line::from(Span::styled(
                        " ─── Untracked ───────────────",
                        Style::default().fg(Color::Rgb(60, 60, 60)),
                    )))
                    .style(Style::default().bg(Color::Rgb(15, 15, 15))),
                );
            }

            let hunk_count = f.hunks.len();
            let hunks_label = if hunk_count == 1 { "1 hunk".to_string() } else { format!("{hunk_count} hunks") };

            let (badge, badge_style, path_style) = match f.kind {
                FileKind::Modified => (
                    " M ",
                    Style::default().fg(Color::Rgb(80, 160, 255)).bg(Color::Rgb(20, 40, 70)),
                    Style::default().fg(Color::Rgb(200, 200, 200)),
                ),
                FileKind::Untracked => (
                    " ? ",
                    Style::default().fg(Color::Rgb(220, 160, 50)).bg(Color::Rgb(50, 35, 10)),
                    Style::default().fg(Color::Rgb(210, 180, 130)),
                ),
            };

            let line = Line::from(vec![
                Span::styled(badge, badge_style),
                Span::raw(" "),
                Span::styled(f.path.clone(), path_style),
                Span::styled(
                    format!("  {hunks_label}"),
                    Style::default().fg(Color::Rgb(60, 60, 60)),
                ),
            ]);

            rows.push(ListItem::new(line));
            rows
        })
        .collect();

    // The ListState index is offset by 1 if the selection is past the separator row
    let state_idx = if files.is_empty() {
        None
    } else {
        Some(match untracked_start {
            Some(sep) if selected >= sep => selected + 1,
            _ => selected,
        })
    };

    let added: usize = files.iter()
        .flat_map(|f| &f.hunks)
        .flat_map(|h| &h.lines)
        .filter(|l| l.kind == LineKind::Added)
        .count();
    let removed: usize = files.iter()
        .flat_map(|f| &f.hunks)
        .flat_map(|h| &h.lines)
        .filter(|l| l.kind == LineKind::Removed)
        .count();

    let title_line = Line::from(vec![
        Span::raw(format!("{title}({})  ", files.len())),
        Span::styled(format!("+{added}"), Style::default().fg(Color::Rgb(100, 210, 100))),
        Span::raw(" "),
        Span::styled(format!("-{removed}"), Style::default().fg(Color::Rgb(210, 90, 90))),
        Span::raw(" "),
    ]);

    let list = List::new(items)
        .block(
            Block::default()
                .title(title_line)
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(40, 40, 40))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = ListState::default();
    state.select(state_idx);
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_diff(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Diff;
    let border_style = focused_border(focused);

    let source_label = match app.diff_source {
        DiffSource::Staged => "staged",
        DiffSource::Unstaged => "unstaged",
    };

    let (title, lines) = match app.current_file() {
        None => (
            format!(" Diff ({source_label}) "),
            vec![Line::from(Span::styled(
                format!("  No {source_label} changes."),
                Style::default().fg(Color::Rgb(70, 70, 70)),
            ))],
        ),
        Some(file) => {
            let title = format!(
                " {} [{}]  hunk {}/{} ",
                file.path,
                source_label,
                if file.hunks.is_empty() { 0 } else { app.selected_hunk + 1 },
                file.hunks.len()
            );

            let mut lines: Vec<Line> = Vec::new();
            for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
                let is_selected = hunk_idx == app.selected_hunk;

                // Hunk header
                let (hdr_fg, hdr_bg) = if is_selected {
                    (Color::Rgb(180, 210, 255), Color::Rgb(25, 40, 70))
                } else {
                    (Color::Rgb(70, 90, 120), Color::Rgb(15, 20, 35))
                };
                let mut hdr_line = Line::from(vec![
                    Span::styled("         ", Style::default().bg(hdr_bg)),
                    Span::styled(hunk.header.clone(), Style::default().fg(hdr_fg).bg(hdr_bg)),
                ]);
                hdr_line.style = Style::default().bg(hdr_bg);
                lines.push(hdr_line);

                let mut old_line = hunk.old_start;
                let mut new_line = hunk.new_start;

                for dl in &hunk.lines {
                    let (old_num, new_num) = match dl.kind {
                        LineKind::Added => { let n = new_line; new_line += 1; (None, Some(n)) }
                        LineKind::Removed => { let n = old_line; old_line += 1; (Some(n), None) }
                        LineKind::Context => {
                            let (o, n) = (old_line, new_line);
                            old_line += 1; new_line += 1;
                            (Some(o), Some(n))
                        }
                        LineKind::NoNewline => (None, None),
                    };

                    let (bg, fg_code, fg_text) = match dl.kind {
                        LineKind::Added => (
                            if is_selected { Color::Rgb(0, 45, 0) } else { Color::Rgb(0, 25, 0) },
                            if is_selected { Color::Rgb(80, 200, 80) } else { Color::Rgb(40, 100, 40) },
                            if is_selected { Color::Rgb(160, 230, 160) } else { Color::Rgb(80, 130, 80) },
                        ),
                        LineKind::Removed => (
                            if is_selected { Color::Rgb(55, 0, 0) } else { Color::Rgb(30, 0, 0) },
                            if is_selected { Color::Rgb(210, 70, 70) } else { Color::Rgb(110, 40, 40) },
                            if is_selected { Color::Rgb(230, 160, 160) } else { Color::Rgb(120, 80, 80) },
                        ),
                        LineKind::Context => (
                            Color::Reset,
                            if is_selected { Color::Rgb(90, 90, 90) } else { Color::Rgb(55, 55, 55) },
                            if is_selected { Color::Rgb(200, 200, 200) } else { Color::Rgb(110, 110, 110) },
                        ),
                        LineKind::NoNewline => (Color::Reset, Color::Rgb(150, 120, 0), Color::Rgb(200, 170, 0)),
                    };

                    let num_style = Style::default()
                        .fg(if is_selected { Color::Rgb(90, 90, 90) } else { Color::Rgb(50, 50, 50) })
                        .bg(bg);

                    let old_str = old_num.map(|n| format!("{n:>4}")).unwrap_or_else(|| "    ".into());
                    let new_str = new_num.map(|n| format!("{n:>4}")).unwrap_or_else(|| "    ".into());
                    let sign = match dl.kind { LineKind::Added => "+", LineKind::Removed => "-", _ => " " };
                    let content = dl.content.get(1..).unwrap_or(&dl.content).to_string();

                    let mut line = Line::from(vec![
                        Span::styled(old_str, num_style),
                        Span::styled(" ", Style::default().bg(bg)),
                        Span::styled(new_str, num_style),
                        Span::styled(" │ ", Style::default().fg(Color::Rgb(45, 45, 45)).bg(bg)),
                        Span::styled(sign, Style::default().fg(fg_code).bg(bg)),
                        Span::styled(" ", Style::default().bg(bg)),
                        Span::styled(content, Style::default().fg(fg_text).bg(bg)),
                    ]);
                    line.style = Style::default().bg(bg);
                    lines.push(line);
                }
            }

            (title, lines)
        }
    };

    let visible_height = area.height.saturating_sub(2) as usize;
    let scroll = app.diff_scroll.min(lines.len().saturating_sub(1));
    let visible: Vec<Line> = lines.into_iter().skip(scroll).take(visible_height).collect();

    frame.render_widget(
        Paragraph::new(visible).block(
            Block::default().title(title).borders(Borders::ALL).border_style(border_style),
        ),
        area,
    );
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);

    let status_style = if app.status.contains("failed") || app.status.contains("Error") {
        Style::default().fg(Color::Red)
    } else if app.status.starts_with("Staged")
        || app.status.starts_with("Unstaged")
        || app.status.starts_with("Discarded")
        || app.status.starts_with("Deleted")
    {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::White)
    };
    frame.render_widget(
        Paragraph::new(format!(" {}", app.status)).style(status_style),
        rows[0],
    );

    // Context-sensitive help
    let help = match &app.focus {
        Focus::Unstaged =>
            " Tab:cycle  ↑↓/jk:nav  s:stage file  d:discard  Enter:diff  r:reload  q:quit",
        Focus::Staged =>
            " Tab:cycle  ↑↓/jk:nav  u:unstage file  Enter:diff  r:reload  q:quit",
        Focus::Diff if app.diff_source == DiffSource::Staged =>
            " Tab:panel  ↑↓/jk:scroll  []:hunk  u:unstage hunk  U:unstage file  r:reload  q:quit",
        Focus::Diff =>
            " Tab:panel  ↑↓/jk:scroll  []:hunk  s:stage hunk  S:stage file  d:discard hunk  D:discard file  r:reload  q:quit",
    };
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::Rgb(70, 70, 70))),
        rows[1],
    );
}

fn focused_border(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Rgb(50, 50, 50))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Pending, PendingAction};
    use ratatui::{backend::TestBackend, Terminal};
    use std::path::PathBuf;

    fn app_with_pending() -> App {
        App {
            staged_files: Vec::new(),
            unstaged_files: Vec::new(),
            staged_sel: 0,
            unstaged_sel: 0,
            selected_hunk: 0,
            diff_scroll: 0,
            focus: Focus::Unstaged,
            diff_source: DiffSource::Unstaged,
            status: String::from("ready"),
            should_quit: false,
            pending: Some(Pending {
                action: PendingAction::DiscardFile,
                title: String::from(" Confirm discard "),
                lines: vec![
                    String::from("Discard all changes in"),
                    String::from("src/git.rs"),
                    String::new(),
                    String::from("This cannot be undone."),
                ],
            }),
            repo_path: PathBuf::from("/nonexistent-the-diff-test"),
        }
    }

    /// Draw into an off-screen buffer and flatten it to text.
    fn rendered(app: &App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_confirm_dialog_is_drawn_over_the_panels() {
        let out = rendered(&app_with_pending(), 80, 24);
        assert!(out.contains("Confirm discard"), "missing title:\n{out}");
        assert!(out.contains("Discard all changes in"), "missing prompt:\n{out}");
        assert!(out.contains("src/git.rs"), "missing path:\n{out}");
        assert!(out.contains("This cannot be undone."), "missing warning:\n{out}");
        assert!(out.contains("confirm"), "missing y hint:\n{out}");
        assert!(out.contains("cancel"), "missing n hint:\n{out}");
    }

    #[test]
    fn no_dialog_is_drawn_when_nothing_is_pending() {
        let mut app = app_with_pending();
        app.pending = None;
        let out = rendered(&app, 80, 24);
        assert!(!out.contains("Confirm discard"));
    }

    #[test]
    fn rendering_survives_a_tiny_terminal() {
        // The dialog is larger than the screen here; it must clamp, not panic.
        let _ = rendered(&app_with_pending(), 20, 8);
    }

    #[test]
    fn centered_rect_clamps_to_the_available_area() {
        let area = Rect { x: 0, y: 0, width: 10, height: 4 };
        let r = centered_rect(40, 20, area);
        assert_eq!((r.width, r.height), (10, 4));
        assert!(r.x + r.width <= area.x + area.width);
        assert!(r.y + r.height <= area.y + area.height);
    }
}
