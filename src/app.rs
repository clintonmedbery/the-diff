use std::path::PathBuf;

use crate::git::{self, ChangedFile, FileKind, LineKind};

/// Which UI panel has keyboard focus.
#[derive(Debug, Clone, PartialEq)]
pub enum Focus {
    Staged,
    Unstaged,
    Diff,
}

/// Which file list is feeding the diff panel on the right.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffSource {
    Staged,
    Unstaged,
}

/// How the diff pane interprets navigation and action keys.
///
/// This is deliberately separate from [`Focus`]: line mode is still the diff
/// pane holding the keyboard, so every `match self.focus` site keeps working
/// unchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffMode {
    /// Keys act on the selected hunk.
    Hunk,
    /// Keys act on the single line under the cursor.
    Line,
}

/// An irreversible action waiting on the user to confirm it.
///
/// Every variant shares the `Discard` prefix because discarding is the only
/// thing irreversible enough to confirm; the prefix earns its keep at the call
/// sites, where `PendingAction::Line` would not say what is about to happen.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq)]
pub enum PendingAction {
    DiscardHunk,
    DiscardLine,
    DiscardFile,
}

/// A file and line the main loop should hand to the user's editor.
#[derive(Debug, Clone, PartialEq)]
pub struct EditorTarget {
    /// Repository-relative path
    pub path: String,
    pub line: u32,
}

/// A queued destructive action, plus the text to show in the confirm dialog.
#[derive(Debug, Clone)]
pub struct Pending {
    pub action: PendingAction,
    /// Dialog border title, e.g. " Confirm discard "
    pub title: String,
    /// Message body, one entry per rendered line
    pub lines: Vec<String>,
}

pub struct App {
    pub staged_files: Vec<ChangedFile>,
    pub unstaged_files: Vec<ChangedFile>, // modified tracked + untracked
    pub staged_sel: usize,
    pub unstaged_sel: usize,
    pub selected_hunk: usize,
    /// Cursor position within `selected_hunk`, meaningful only in line mode.
    pub selected_line: usize,
    pub diff_scroll: usize,
    pub focus: Focus,
    /// Whether the diff pane is acting on hunks or on single lines.
    pub diff_mode: DiffMode,
    /// Which file list the diff panel is currently showing.
    pub diff_source: DiffSource,
    pub status: String,
    pub should_quit: bool,
    /// Set while a destructive action awaits confirmation. While this is
    /// `Some`, the main loop routes every key to confirm/cancel and suppresses
    /// the idle auto-reload, so the indices captured here stay valid.
    pub pending: Option<Pending>,
    /// Set when the reader asks for an editor. The main loop owns the terminal,
    /// so it performs the handoff and clears this — the same arrangement as
    /// `should_quit`.
    pub editor_request: Option<EditorTarget>,
    pub repo_path: PathBuf,
}

impl App {
    pub fn new(repo_path: PathBuf) -> anyhow::Result<Self> {
        let (staged_files, unstaged_files) = load_all(&repo_path);
        Ok(Self {
            staged_files,
            unstaged_files,
            staged_sel: 0,
            unstaged_sel: 0,
            selected_hunk: 0,
            selected_line: 0,
            diff_scroll: 0,
            focus: Focus::Unstaged,
            diff_mode: DiffMode::Hunk,
            diff_source: DiffSource::Unstaged,
            status: String::from("Tab: cycle panels  q: quit"),
            should_quit: false,
            pending: None,
            editor_request: None,
            repo_path,
        })
    }

    pub fn reload(&mut self) {
        let (staged, unstaged) = load_all(&self.repo_path);
        self.apply_reload(staged, unstaged);
    }

    /// Swap in freshly loaded file lists. Split out from `reload` so the view
    /// bookkeeping can be tested without a repository on disk.
    fn apply_reload(&mut self, staged: Vec<ChangedFile>, unstaged: Vec<ChangedFile>) {
        let prev_staged = self.staged_files.get(self.staged_sel).map(|f| f.path.clone());
        let prev_unstaged = self
            .unstaged_files
            .get(self.unstaged_sel)
            .map(|f| f.path.clone());
        let prev_shown = self.current_file().map(|f| f.path.clone());

        self.staged_files = staged;
        self.unstaged_files = unstaged;

        // Follow each selection by path rather than by index: staging or
        // discarding a file shifts everything below it up, and an index on its
        // own would silently select whichever file slid into that slot.
        self.staged_sel = reselect(&self.staged_files, prev_staged.as_deref(), self.staged_sel);
        self.unstaged_sel = reselect(
            &self.unstaged_files,
            prev_unstaged.as_deref(),
            self.unstaged_sel,
        );

        // Hold the reader's place only while the diff panel still shows the
        // same file. The idle reload fires while someone sits reading a
        // scrolled diff, so resetting then would lose their position — but an
        // offset carried into a different file is meaningless.
        let shown = self.current_file().map(|f| f.path.clone());
        if shown.is_some() && shown == prev_shown {
            let hunk_count = self.current_file().map(|f| f.hunks.len()).unwrap_or(0);
            self.selected_hunk = self.selected_hunk.min(hunk_count.saturating_sub(1));
            let total = self.total_diff_lines();
            self.diff_scroll = self.diff_scroll.min(total.saturating_sub(1));
            // The hunk may have shrunk under the cursor, and a line index past
            // its end would aim the next discard at the wrong line.
            let line_count = self
                .current_file()
                .and_then(|f| f.hunks.get(self.selected_hunk))
                .map(|h| h.lines.len())
                .unwrap_or(0);
            self.selected_line = self.selected_line.min(line_count.saturating_sub(1));
        } else {
            // A line index carried into another file is meaningless, so fall
            // back to hunk mode rather than pointing somewhere arbitrary.
            self.reset_diff_view();
        }
    }

    /// The file whose diff is currently shown in the right panel.
    pub fn current_file(&self) -> Option<&ChangedFile> {
        match self.diff_source {
            DiffSource::Staged => self.staged_files.get(self.staged_sel),
            DiffSource::Unstaged => self.unstaged_files.get(self.unstaged_sel),
        }
    }

    // ── Focus / panel cycling ────────────────────────────────────────────────

    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Staged => Focus::Unstaged,
            Focus::Unstaged => Focus::Diff,
            Focus::Diff => Focus::Staged,
        };
    }

    /// Descend one level: file list → hunks → lines.
    pub fn drill_in(&mut self) {
        match self.focus {
            Focus::Staged => {
                self.diff_source = DiffSource::Staged;
                self.reset_diff_view();
                self.focus = Focus::Diff;
            }
            Focus::Unstaged => {
                self.diff_source = DiffSource::Unstaged;
                self.reset_diff_view();
                self.focus = Focus::Diff;
            }
            Focus::Diff => self.enter_line_mode(),
        }
    }

    /// Rise one level: lines → hunks → file list.
    pub fn drill_out(&mut self) {
        if self.focus != Focus::Diff {
            return;
        }
        if self.diff_mode == DiffMode::Line {
            self.diff_mode = DiffMode::Hunk;
            return;
        }
        self.focus = match self.diff_source {
            DiffSource::Staged => Focus::Staged,
            DiffSource::Unstaged => Focus::Unstaged,
        };
    }

    fn enter_line_mode(&mut self) {
        if self.diff_mode == DiffMode::Line {
            return;
        }
        let Some(start) = self
            .current_file()
            .and_then(|f| f.hunks.get(self.selected_hunk))
            .filter(|h| !h.lines.is_empty())
            .map(first_change)
        else {
            return;
        };
        self.selected_line = start;
        self.diff_mode = DiffMode::Line;
    }

    fn reset_diff_view(&mut self) {
        self.selected_hunk = 0;
        self.selected_line = 0;
        self.diff_scroll = 0;
        self.diff_mode = DiffMode::Hunk;
    }

    // ── File-list navigation ─────────────────────────────────────────────────

    pub fn file_up(&mut self) {
        match self.focus {
            Focus::Staged => {
                if self.staged_sel > 0 {
                    self.staged_sel -= 1;
                    self.diff_source = DiffSource::Staged;
                    self.selected_hunk = 0;
                    self.diff_scroll = 0;
                }
            }
            Focus::Unstaged => {
                if self.unstaged_sel > 0 {
                    self.unstaged_sel -= 1;
                    self.diff_source = DiffSource::Unstaged;
                    self.selected_hunk = 0;
                    self.diff_scroll = 0;
                }
            }
            Focus::Diff => {}
        }
    }

    pub fn file_down(&mut self) {
        match self.focus {
            Focus::Staged => {
                if !self.staged_files.is_empty()
                    && self.staged_sel + 1 < self.staged_files.len()
                {
                    self.staged_sel += 1;
                    self.diff_source = DiffSource::Staged;
                    self.selected_hunk = 0;
                    self.diff_scroll = 0;
                }
            }
            Focus::Unstaged => {
                if !self.unstaged_files.is_empty()
                    && self.unstaged_sel + 1 < self.unstaged_files.len()
                {
                    self.unstaged_sel += 1;
                    self.diff_source = DiffSource::Unstaged;
                    self.selected_hunk = 0;
                    self.diff_scroll = 0;
                }
            }
            Focus::Diff => {}
        }
    }

    // ── Hunk / scroll navigation ─────────────────────────────────────────────

    pub fn hunk_up(&mut self) {
        if self.selected_hunk > 0 {
            self.selected_hunk -= 1;
            self.reset_line_cursor();
            self.scroll_to_selected_hunk();
        }
    }

    pub fn hunk_down(&mut self) {
        let max = self.current_file().map(|f| f.hunks.len()).unwrap_or(0);
        if max > 0 && self.selected_hunk + 1 < max {
            self.selected_hunk += 1;
            self.reset_line_cursor();
            self.scroll_to_selected_hunk();
        }
    }

    /// Put the line cursor on the newly selected hunk's first change, so an
    /// index from the hunk just left cannot point past the end of this one.
    fn reset_line_cursor(&mut self) {
        self.selected_line = self
            .current_file()
            .and_then(|f| f.hunks.get(self.selected_hunk))
            .map(first_change)
            .unwrap_or(0);
    }

    /// Move the line cursor down, rolling into the next hunk at the boundary
    /// so the whole file reads as one continuous list.
    pub fn line_down(&mut self) {
        let Some((lines, hunks)) = self.hunk_shape() else { return };
        if self.selected_line + 1 < lines {
            self.selected_line += 1;
        } else if self.selected_hunk + 1 < hunks {
            self.selected_hunk += 1;
            self.selected_line = 0;
        }
    }

    /// Move the line cursor up, rolling into the previous hunk's last line.
    pub fn line_up(&mut self) {
        if self.hunk_shape().is_none() {
            return;
        }
        if self.selected_line > 0 {
            self.selected_line -= 1;
            return;
        }
        if self.selected_hunk == 0 {
            return;
        }
        let previous = self
            .current_file()
            .and_then(|f| f.hunks.get(self.selected_hunk - 1))
            .map(|h| h.lines.len())
            .unwrap_or(0);
        self.selected_hunk -= 1;
        self.selected_line = previous.saturating_sub(1);
    }

    /// (lines in the selected hunk, hunks in the file), or `None` if there is
    /// no hunk under the cursor.
    fn hunk_shape(&self) -> Option<(usize, usize)> {
        let file = self.current_file()?;
        let lines = file.hunks.get(self.selected_hunk)?.lines.len();
        Some((lines, file.hunks.len()))
    }

    pub fn scroll_up(&mut self) {
        self.diff_scroll = self.diff_scroll.saturating_sub(1);
    }

    pub fn scroll_down(&mut self, visible_height: usize) {
        let total = self.total_diff_lines();
        if total > visible_height && self.diff_scroll + visible_height < total {
            self.diff_scroll += 1;
        }
    }

    pub fn total_diff_lines(&self) -> usize {
        self.current_file()
            .map(|f| f.hunks.iter().map(|h| 1 + h.lines.len()).sum())
            .unwrap_or(0)
    }

    /// Which rendered row a given line occupies, counting the one header row
    /// each hunk contributes above its lines.
    pub fn rendered_row_of(&self, hunk_idx: usize, line_idx: usize) -> usize {
        let before: usize = self
            .current_file()
            .map(|f| {
                f.hunks[..hunk_idx.min(f.hunks.len())]
                    .iter()
                    .map(|h| 1 + h.lines.len())
                    .sum()
            })
            .unwrap_or(0);
        before + 1 + line_idx
    }

    /// Scroll the minimum amount needed to bring the line cursor into view.
    pub fn ensure_line_visible(&mut self, visible_height: usize) {
        if visible_height == 0 {
            return;
        }
        let row = self.rendered_row_of(self.selected_hunk, self.selected_line);
        if row < self.diff_scroll {
            self.diff_scroll = row;
        } else if row >= self.diff_scroll + visible_height {
            self.diff_scroll = row + 1 - visible_height;
        }
    }

    fn scroll_to_selected_hunk(&mut self) {
        if let Some(file) = self.current_file() {
            let offset: usize = file.hunks[..self.selected_hunk]
                .iter()
                .map(|h| 1 + h.lines.len())
                .sum();
            self.diff_scroll = offset;
        }
    }

    // ── Actions (context-aware: staged vs unstaged) ──────────────────────────

    // ── Stage actions (s / S) ────────────────────────────────────────────────

    /// `s`: stage hunk (diff panel, unstaged source) or stage file (unstaged list).
    pub fn stage_action(&mut self) {
        match self.focus {
            Focus::Unstaged => self.stage_file(),
            // Staging one line needs a patch faithful to the *old* side, the
            // opposite of what discard and unstage build. Rather than quietly
            // widening `s` to the whole hunk, say so.
            Focus::Diff
                if self.diff_source == DiffSource::Unstaged
                    && self.diff_mode == DiffMode::Line =>
            {
                self.status =
                    String::from("Staging a single line isn't supported — Esc for hunk mode");
            }
            Focus::Diff if self.diff_source == DiffSource::Unstaged => self.stage_hunk(),
            _ => {}
        }
    }

    /// `S`: stage entire file (unstaged context only).
    pub fn stage_file_action(&mut self) {
        match self.focus {
            Focus::Unstaged => self.stage_file(),
            Focus::Diff if self.diff_source == DiffSource::Unstaged => self.stage_file(),
            _ => {}
        }
    }

    // ── Unstage actions (u / U) ──────────────────────────────────────────────

    /// `u`: unstage hunk (diff panel, staged source) or unstage file (staged list).
    pub fn unstage_action(&mut self) {
        match self.focus {
            Focus::Staged => self.unstage_file(),
            Focus::Diff
                if self.diff_source == DiffSource::Staged
                    && self.diff_mode == DiffMode::Line =>
            {
                self.unstage_line()
            }
            Focus::Diff if self.diff_source == DiffSource::Staged => self.unstage_hunk(),
            _ => {}
        }
    }

    /// `U`: unstage entire file (staged context only).
    pub fn unstage_file_action(&mut self) {
        match self.focus {
            Focus::Staged => self.unstage_file(),
            Focus::Diff if self.diff_source == DiffSource::Staged => self.unstage_file(),
            _ => {}
        }
    }

    // ── Discard actions (d / D) ──────────────────────────────────────────────

    /// `d`: ask before discarding a hunk (diff panel) or a file (unstaged list).
    pub fn discard_action(&mut self) {
        match self.focus {
            Focus::Unstaged => self.request_discard_file(),
            Focus::Diff
                if self.diff_source == DiffSource::Unstaged
                    && self.diff_mode == DiffMode::Line =>
            {
                self.request_discard_line()
            }
            Focus::Diff if self.diff_source == DiffSource::Unstaged => self.request_discard_hunk(),
            _ => {}
        }
    }

    /// `D`: ask before discarding an entire file (unstaged context only).
    pub fn discard_file_action(&mut self) {
        match self.focus {
            Focus::Unstaged => self.request_discard_file(),
            Focus::Diff if self.diff_source == DiffSource::Unstaged => self.request_discard_file(),
            _ => {}
        }
    }

    // ── Editor handoff (e) ───────────────────────────────────────────────────

    /// `e`: ask the main loop to open the cursor's line in an editor.
    pub fn open_editor_action(&mut self) {
        if self.focus != Focus::Diff || self.diff_mode != DiffMode::Line {
            return;
        }
        let Some(target) = self.current_file().and_then(|file| {
            let hunk = file.hunks.get(self.selected_hunk)?;
            Some(EditorTarget {
                path: file.path.clone(),
                line: hunk.target_line(self.selected_line),
            })
        }) else {
            return;
        };
        self.editor_request = Some(target);
    }

    // ── Confirmation of destructive actions ──────────────────────────────────

    fn request_discard_hunk(&mut self) {
        let Some(file) = self.unstaged_files.get(self.unstaged_sel) else { return };
        if self.selected_hunk >= file.hunks.len() { return }
        self.pending = Some(Pending {
            action: PendingAction::DiscardHunk,
            title: String::from(" Confirm discard "),
            lines: vec![
                format!("Discard hunk {} of {} in", self.selected_hunk + 1, file.hunks.len()),
                file.path.clone(),
                String::new(),
                String::from("This cannot be undone."),
            ],
        });
    }

    fn request_discard_line(&mut self) {
        let Some(file) = self.unstaged_files.get(self.unstaged_sel) else { return };
        let Some(hunk) = file.hunks.get(self.selected_hunk) else { return };
        let Some(line) = hunk.lines.get(self.selected_line) else { return };
        if !matches!(line.kind, LineKind::Added | LineKind::Removed) {
            self.status = String::from("Nothing to discard on this line");
            return;
        }
        self.pending = Some(Pending {
            action: PendingAction::DiscardLine,
            title: String::from(" Confirm discard "),
            lines: vec![
                format!("Discard line {} in", hunk.target_line(self.selected_line)),
                file.path.clone(),
                String::new(),
                truncate(&line.content, DIALOG_WIDTH),
                String::new(),
                String::from("This cannot be undone."),
            ],
        });
    }

    fn request_discard_file(&mut self) {
        let Some(file) = self.unstaged_files.get(self.unstaged_sel) else { return };
        let untracked = file.kind == FileKind::Untracked;
        self.pending = Some(Pending {
            action: PendingAction::DiscardFile,
            title: String::from(if untracked { " Confirm delete " } else { " Confirm discard " }),
            lines: vec![
                String::from(if untracked {
                    "Delete untracked file"
                } else {
                    "Discard all changes in"
                }),
                file.path.clone(),
                String::new(),
                String::from("This cannot be undone."),
            ],
        });
    }

    /// `y`: run the pending action.
    pub fn confirm(&mut self) {
        let Some(pending) = self.pending.take() else { return };
        match pending.action {
            PendingAction::DiscardHunk => self.discard_hunk(),
            PendingAction::DiscardLine => self.discard_line(),
            PendingAction::DiscardFile => self.discard_file(),
        }
    }

    /// Any other key: dismiss the pending action without running it.
    pub fn cancel(&mut self) {
        if self.pending.take().is_some() {
            self.status = String::from("Cancelled");
        }
    }

    // ── Private action implementations ───────────────────────────────────────

    fn stage_hunk(&mut self) {
        let Some(file) = self.unstaged_files.get(self.unstaged_sel).cloned() else { return };
        let idx = self.selected_hunk;
        if idx >= file.hunks.len() { return; }
        match git::stage_hunk(&self.repo_path, &file, idx) {
            Ok(()) => {
                self.status = format!("Staged hunk {}/{} in {}", idx + 1, file.hunks.len(), file.path);
                self.reload();
            }
            Err(e) => self.status = format!("Stage failed: {e}"),
        }
    }

    fn unstage_hunk(&mut self) {
        let Some(file) = self.staged_files.get(self.staged_sel).cloned() else { return };
        let idx = self.selected_hunk;
        if idx >= file.hunks.len() { return; }
        match git::unstage_hunk(&self.repo_path, &file, idx) {
            Ok(()) => {
                self.status = format!("Unstaged hunk {}/{} in {}", idx + 1, file.hunks.len(), file.path);
                self.reload();
            }
            Err(e) => self.status = format!("Unstage failed: {e}"),
        }
    }

    fn stage_file(&mut self) {
        let Some(file) = self.unstaged_files.get(self.unstaged_sel) else { return };
        let path = file.path.clone();
        match git::stage_file(&self.repo_path, &path) {
            Ok(()) => { self.status = format!("Staged {path}"); self.reload(); }
            Err(e) => self.status = format!("Stage failed: {e}"),
        }
    }

    fn unstage_file(&mut self) {
        let Some(file) = self.staged_files.get(self.staged_sel) else { return };
        let path = file.path.clone();
        match git::unstage_file(&self.repo_path, &path) {
            Ok(()) => { self.status = format!("Unstaged {path}"); self.reload(); }
            Err(e) => self.status = format!("Unstage failed: {e}"),
        }
    }

    fn discard_hunk(&mut self) {
        let Some(file) = self.unstaged_files.get(self.unstaged_sel).cloned() else { return };
        let idx = self.selected_hunk;
        if idx >= file.hunks.len() { return; }
        match git::discard_hunk(&self.repo_path, &file, idx) {
            Ok(()) => {
                self.status = format!("Discarded hunk {}/{} in {}", idx + 1, file.hunks.len(), file.path);
                self.reload();
            }
            Err(e) => self.status = format!("Discard failed: {e}"),
        }
    }

    fn discard_line(&mut self) {
        let Some(file) = self.unstaged_files.get(self.unstaged_sel).cloned() else { return };
        let (hunk_idx, line_idx) = (self.selected_hunk, self.selected_line);
        match git::discard_line(&self.repo_path, &file, hunk_idx, line_idx) {
            Ok(()) => {
                self.status = format!("Discarded 1 line in {}", file.path);
                self.reload();
            }
            Err(e) => self.status = format!("Discard failed: {e}"),
        }
    }

    fn unstage_line(&mut self) {
        let Some(file) = self.staged_files.get(self.staged_sel).cloned() else { return };
        let (hunk_idx, line_idx) = (self.selected_hunk, self.selected_line);
        match git::unstage_line(&self.repo_path, &file, hunk_idx, line_idx) {
            Ok(()) => {
                self.status = format!("Unstaged 1 line in {}", file.path);
                self.reload();
            }
            Err(e) => self.status = format!("Unstage failed: {e}"),
        }
    }

    fn discard_file(&mut self) {
        let Some(file) = self.unstaged_files.get(self.unstaged_sel) else { return };
        let path = file.path.clone();
        let kind = file.kind.clone();
        let result = if kind == FileKind::Untracked {
            git::delete_file(&self.repo_path, &path)
        } else {
            git::discard_file(&self.repo_path, &path)
        };
        match result {
            Ok(()) => {
                self.status = if kind == FileKind::Untracked {
                    format!("Deleted {path}")
                } else {
                    format!("Discarded all changes in {path}")
                };
                self.reload();
            }
            Err(e) => self.status = format!("Discard failed: {e}"),
        }
    }
}

/// The first line of `hunk` worth putting a cursor on: landing on a context
/// line would mean the reader's first keypress does nothing.
fn first_change(hunk: &crate::git::Hunk) -> usize {
    hunk.lines
        .iter()
        .position(|l| matches!(l.kind, LineKind::Added | LineKind::Removed))
        .unwrap_or(0)
}

/// Widest line the confirm dialog will show before eliding.
const DIALOG_WIDTH: usize = 60;

/// Shorten `s` to `width` characters, marking the cut with an ellipsis.
fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    s.chars().take(width.saturating_sub(1)).chain(['…']).collect()
}

/// Locate `path` in `files` so a selection can follow its file across a
/// reload. Falls back to the previous index, clamped, once that file is gone.
fn reselect(files: &[ChangedFile], path: Option<&str>, previous: usize) -> usize {
    if let Some(path) = path {
        if let Some(i) = files.iter().position(|f| f.path == path) {
            return i;
        }
    }
    previous.min(files.len().saturating_sub(1))
}

fn load_all(repo_path: &std::path::Path) -> (Vec<ChangedFile>, Vec<ChangedFile>) {
    let staged = git::load_staged_diff(repo_path).unwrap_or_default();
    let mut unstaged = git::load_diff(repo_path).unwrap_or_default();
    unstaged.extend(git::load_untracked(repo_path).unwrap_or_default());
    (staged, unstaged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{Hunk, HunkLine};

    fn hunk(n: usize) -> Hunk {
        Hunk {
            header: format!("@@ -{n},1 +{n},1 @@"),
            lines: vec![HunkLine {
                content: String::from("+x"),
                kind: LineKind::Added,
            }],
            old_start: n as u32,
            new_start: n as u32,
        }
    }

    fn changed(path: &str, kind: FileKind, hunks: usize) -> ChangedFile {
        ChangedFile {
            path: String::from(path),
            header: format!("diff --git a/{path} b/{path}"),
            hunks: (0..hunks).map(hunk).collect(),
            kind,
        }
    }

    /// An App pointed at a path that does not exist, so any git invocation
    /// fails at spawn instead of mutating a real repository. These tests cover
    /// the confirmation state machine, not the git calls behind it.
    fn app(unstaged: Vec<ChangedFile>) -> App {
        App {
            staged_files: Vec::new(),
            unstaged_files: unstaged,
            staged_sel: 0,
            unstaged_sel: 0,
            selected_hunk: 0,
            selected_line: 0,
            diff_scroll: 0,
            focus: Focus::Unstaged,
            diff_mode: DiffMode::Hunk,
            diff_source: DiffSource::Unstaged,
            status: String::new(),
            should_quit: false,
            pending: None,
            editor_request: None,
            repo_path: PathBuf::from("/nonexistent-the-diff-test"),
        }
    }

    /// A hunk whose lines follow `kinds`, so navigation tests can place
    /// context and changes deliberately.
    fn mixed(kinds: &[LineKind]) -> Hunk {
        Hunk {
            header: String::from("@@ -1,3 +1,3 @@"),
            lines: kinds
                .iter()
                .map(|k| HunkLine {
                    content: match k {
                        LineKind::Added => String::from("+new"),
                        LineKind::Removed => String::from("-old"),
                        LineKind::Context => String::from(" same"),
                        LineKind::NoNewline => String::from(r"\ No newline at end of file"),
                    },
                    kind: k.clone(),
                })
                .collect(),
            old_start: 1,
            new_start: 1,
        }
    }

    fn with_hunks(hunks: Vec<Hunk>) -> App {
        let mut a = app(vec![ChangedFile {
            path: String::from("a.rs"),
            header: String::from("diff --git a/a.rs b/a.rs"),
            hunks,
            kind: FileKind::Modified,
        }]);
        a.focus = Focus::Diff;
        a
    }

    // ── Entering and leaving line mode ───────────────────────────────────────

    #[test]
    fn drilling_in_from_a_file_list_lands_on_hunks_not_lines() {
        let mut a = app(vec![changed("a.rs", FileKind::Modified, 2)]);
        a.focus = Focus::Unstaged;
        a.drill_in();
        assert_eq!(a.focus, Focus::Diff);
        assert_eq!(a.diff_mode, DiffMode::Hunk);
    }

    #[test]
    fn drilling_in_again_enters_line_mode() {
        let mut a = with_hunks(vec![mixed(&[LineKind::Context, LineKind::Added])]);
        a.drill_in();
        assert_eq!(a.diff_mode, DiffMode::Line);
    }

    #[test]
    fn line_mode_starts_on_the_first_actual_change() {
        // Landing on a context line would mean the first `d` does nothing.
        let mut a = with_hunks(vec![mixed(&[
            LineKind::Context,
            LineKind::Context,
            LineKind::Removed,
        ])]);
        a.drill_in();
        assert_eq!(a.selected_line, 2);
    }

    #[test]
    fn a_hunk_of_pure_context_starts_at_the_top() {
        let mut a = with_hunks(vec![mixed(&[LineKind::Context, LineKind::Context])]);
        a.drill_in();
        assert_eq!(a.selected_line, 0);
    }

    #[test]
    fn drilling_out_of_line_mode_returns_to_hunks() {
        let mut a = with_hunks(vec![mixed(&[LineKind::Added])]);
        a.drill_in();
        a.drill_out();
        assert_eq!(a.diff_mode, DiffMode::Hunk);
        assert_eq!(a.focus, Focus::Diff, "should still be in the diff pane");
    }

    #[test]
    fn drilling_out_of_hunk_mode_returns_to_the_file_list() {
        let mut a = with_hunks(vec![mixed(&[LineKind::Added])]);
        a.drill_out();
        assert_eq!(a.focus, Focus::Unstaged);
    }

    #[test]
    fn an_empty_hunk_cannot_be_entered_line_by_line() {
        let mut a = with_hunks(vec![]);
        a.drill_in();
        assert_eq!(a.diff_mode, DiffMode::Hunk);
    }

    // ── Navigating line by line ──────────────────────────────────────────────

    #[test]
    fn moving_down_walks_the_lines_of_the_hunk() {
        let mut a = with_hunks(vec![mixed(&[
            LineKind::Added,
            LineKind::Context,
            LineKind::Added,
        ])]);
        a.drill_in();
        a.line_down();
        assert_eq!(a.selected_line, 1, "context lines are still walked past");
        a.line_down();
        assert_eq!(a.selected_line, 2);
    }

    #[test]
    fn moving_down_off_the_end_of_a_hunk_enters_the_next_one() {
        let mut a = with_hunks(vec![
            mixed(&[LineKind::Added]),
            mixed(&[LineKind::Removed, LineKind::Added]),
        ]);
        a.drill_in();
        a.line_down();
        assert_eq!((a.selected_hunk, a.selected_line), (1, 0));
    }

    #[test]
    fn moving_up_off_the_top_of_a_hunk_enters_the_previous_one() {
        let mut a = with_hunks(vec![
            mixed(&[LineKind::Added, LineKind::Context]),
            mixed(&[LineKind::Added]),
        ]);
        a.drill_in();
        a.selected_hunk = 1;
        a.selected_line = 0;
        a.line_up();
        assert_eq!(
            (a.selected_hunk, a.selected_line),
            (0, 1),
            "should land on the last line of the previous hunk"
        );
    }

    #[test]
    fn the_cursor_stops_at_the_end_of_the_last_hunk() {
        let mut a = with_hunks(vec![mixed(&[LineKind::Added, LineKind::Context])]);
        a.drill_in();
        a.selected_line = 1;
        a.line_down();
        assert_eq!((a.selected_hunk, a.selected_line), (0, 1));
    }

    #[test]
    fn the_cursor_stops_at_the_top_of_the_first_hunk() {
        let mut a = with_hunks(vec![mixed(&[LineKind::Added, LineKind::Context])]);
        a.drill_in();
        a.selected_line = 0;
        a.line_up();
        assert_eq!((a.selected_hunk, a.selected_line), (0, 0));
    }

    #[test]
    fn jumping_to_another_hunk_moves_the_line_cursor_with_it() {
        // Hunk 1 has fewer lines than the cursor's index in hunk 0, so a
        // carried-over index would point past its end.
        let mut a = with_hunks(vec![
            mixed(&[LineKind::Added, LineKind::Added, LineKind::Added]),
            mixed(&[LineKind::Context, LineKind::Removed]),
        ]);
        a.drill_in();
        a.selected_line = 2;
        a.hunk_down();
        assert_eq!(a.selected_hunk, 1);
        assert_eq!(a.selected_line, 1, "should land on the new hunk's first change");
    }

    #[test]
    fn jumping_back_a_hunk_also_moves_the_line_cursor() {
        let mut a = with_hunks(vec![
            mixed(&[LineKind::Context, LineKind::Added]),
            mixed(&[LineKind::Added, LineKind::Added]),
        ]);
        a.drill_in();
        a.selected_hunk = 1;
        a.selected_line = 1;
        a.hunk_up();
        assert_eq!((a.selected_hunk, a.selected_line), (0, 1));
    }

    // ── Acting on a single line ──────────────────────────────────────────────

    #[test]
    fn discarding_in_line_mode_targets_the_line_not_the_hunk() {
        let mut a = with_hunks(vec![mixed(&[LineKind::Context, LineKind::Removed])]);
        a.drill_in();
        a.discard_action();
        let p = a.pending.as_ref().expect("expected a confirmation");
        assert_eq!(p.action, PendingAction::DiscardLine);
    }

    #[test]
    fn the_confirmation_quotes_the_line_being_discarded() {
        let mut a = with_hunks(vec![mixed(&[LineKind::Context, LineKind::Removed])]);
        a.drill_in();
        a.discard_action();
        let p = a.pending.as_ref().expect("expected a confirmation");
        assert!(p.lines.iter().any(|l| l == "-old"), "{:?}", p.lines);
        assert!(p.lines.iter().any(|l| l.contains("line 2")), "{:?}", p.lines);
        assert!(p.lines.iter().any(|l| l == "a.rs"), "{:?}", p.lines);
    }

    #[test]
    fn a_very_long_line_is_truncated_in_the_dialog() {
        // render_confirm sizes the box to its widest line, so an untruncated
        // 400-character line would draw a dialog wider than the terminal.
        let mut a = with_hunks(vec![mixed(&[LineKind::Added])]);
        a.unstaged_files[0].hunks[0].lines[0].content = format!("+{}", "x".repeat(400));
        a.drill_in();
        a.discard_action();
        let p = a.pending.as_ref().expect("expected a confirmation");
        assert!(p.lines.iter().all(|l| l.chars().count() <= 64), "{:?}", p.lines);
        assert!(p.lines.iter().any(|l| l.ends_with('…')), "{:?}", p.lines);
    }

    #[test]
    fn a_context_line_reports_that_there_is_nothing_to_discard() {
        let mut a = with_hunks(vec![mixed(&[LineKind::Context, LineKind::Context])]);
        a.drill_in();
        a.discard_action();
        assert!(a.pending.is_none());
        assert!(a.status.contains("Nothing to discard"), "{}", a.status);
    }

    #[test]
    fn unstaging_a_line_is_never_gated_behind_a_confirmation() {
        let mut a = app(Vec::new());
        a.staged_files = vec![ChangedFile {
            path: String::from("a.rs"),
            header: String::from("diff --git a/a.rs b/a.rs"),
            hunks: vec![mixed(&[LineKind::Added])],
            kind: FileKind::Modified,
        }];
        a.focus = Focus::Diff;
        a.diff_source = DiffSource::Staged;
        a.drill_in();
        a.unstage_action();
        assert!(a.pending.is_none(), "unstaging is reversible");
    }

    #[test]
    fn staging_by_line_says_it_is_unsupported_rather_than_staging_the_hunk() {
        // Silently widening `s` to the whole hunk would stage changes the
        // reader did not ask for.
        let mut a = with_hunks(vec![mixed(&[LineKind::Added, LineKind::Added])]);
        a.drill_in();
        a.stage_action();
        assert!(a.status.contains("line"), "{}", a.status);
        assert!(a.pending.is_none());
    }

    // ── Handing off to an editor ─────────────────────────────────────────────

    #[test]
    fn opening_an_editor_targets_the_line_under_the_cursor() {
        let mut a = with_hunks(vec![mixed(&[LineKind::Context, LineKind::Added])]);
        a.drill_in();
        a.open_editor_action();
        let t = a.editor_request.as_ref().expect("expected an editor request");
        assert_eq!(t.path, "a.rs");
        // One context line precedes the addition, so it is line 2 on disk
        assert_eq!(t.line, 2);
    }

    #[test]
    fn opening_an_editor_works_from_the_staged_diff_too() {
        let mut a = app(Vec::new());
        a.staged_files = vec![ChangedFile {
            path: String::from("staged.rs"),
            header: String::from("diff --git a/staged.rs b/staged.rs"),
            hunks: vec![mixed(&[LineKind::Added])],
            kind: FileKind::Modified,
        }];
        a.focus = Focus::Diff;
        a.diff_source = DiffSource::Staged;
        a.drill_in();
        a.open_editor_action();
        assert_eq!(
            a.editor_request.as_ref().map(|t| t.path.as_str()),
            Some("staged.rs")
        );
    }

    #[test]
    fn hunk_mode_has_no_line_to_open_an_editor_at() {
        let mut a = with_hunks(vec![mixed(&[LineKind::Added])]);
        a.open_editor_action();
        assert!(a.editor_request.is_none());
    }

    #[test]
    fn opening_an_editor_with_no_file_shown_does_nothing() {
        let mut a = app(Vec::new());
        a.focus = Focus::Diff;
        a.open_editor_action();
        assert!(a.editor_request.is_none());
    }

    // ── Keeping the cursor on screen ─────────────────────────────────────────

    #[test]
    fn a_rendered_row_accounts_for_each_hunk_header() {
        let a = with_hunks(vec![
            mixed(&[LineKind::Added, LineKind::Context]),
            mixed(&[LineKind::Added]),
        ]);
        // Row 0 is hunk 0's header, so its first line is row 1
        assert_eq!(a.rendered_row_of(0, 0), 1);
        assert_eq!(a.rendered_row_of(0, 1), 2);
        // Hunk 1's header is row 3, its first line row 4
        assert_eq!(a.rendered_row_of(1, 0), 4);
    }

    #[test]
    fn scrolling_follows_a_cursor_that_walks_below_the_viewport() {
        let mut a = with_hunks(vec![mixed(&[
            LineKind::Added,
            LineKind::Added,
            LineKind::Added,
            LineKind::Added,
        ])]);
        a.drill_in();
        a.selected_line = 3; // rendered row 4
        a.ensure_line_visible(3);
        // Row 4 must be the last of three visible rows: 2, 3, 4
        assert_eq!(a.diff_scroll, 2);
    }

    #[test]
    fn scrolling_follows_a_cursor_that_walks_above_the_viewport() {
        let mut a = with_hunks(vec![mixed(&[LineKind::Added, LineKind::Added])]);
        a.drill_in();
        a.diff_scroll = 5;
        a.selected_line = 0; // rendered row 1
        a.ensure_line_visible(3);
        assert_eq!(a.diff_scroll, 1);
    }

    #[test]
    fn an_already_visible_cursor_does_not_move_the_view() {
        let mut a = with_hunks(vec![mixed(&[LineKind::Added, LineKind::Added])]);
        a.drill_in();
        a.diff_scroll = 1;
        a.selected_line = 1; // rendered row 2, visible in rows 1..4
        a.ensure_line_visible(3);
        assert_eq!(a.diff_scroll, 1);
    }

    #[test]
    fn discarding_a_file_asks_first() {
        let mut a = app(vec![changed("src/git.rs", FileKind::Modified, 2)]);
        a.discard_action();
        let p = a.pending.as_ref().expect("expected a confirmation");
        assert_eq!(p.action, PendingAction::DiscardFile);
        assert!(p.lines.iter().any(|l| l == "src/git.rs"));
        assert!(p.lines.iter().any(|l| l.contains("Discard all changes in")));
    }

    #[test]
    fn discarding_a_hunk_names_which_hunk() {
        let mut a = app(vec![changed("src/ui.rs", FileKind::Modified, 3)]);
        a.focus = Focus::Diff;
        a.selected_hunk = 1;
        a.discard_action();
        let p = a.pending.as_ref().expect("expected a confirmation");
        assert_eq!(p.action, PendingAction::DiscardHunk);
        assert!(p.lines.iter().any(|l| l == "Discard hunk 2 of 3 in"));
    }

    #[test]
    fn an_untracked_file_is_described_as_a_delete() {
        let mut a = app(vec![changed("notes.txt", FileKind::Untracked, 1)]);
        a.discard_action();
        let p = a.pending.as_ref().expect("expected a confirmation");
        assert_eq!(p.title.trim(), "Confirm delete");
        assert!(p.lines.iter().any(|l| l.contains("Delete untracked file")));
    }

    #[test]
    fn cancelling_clears_the_pending_action() {
        let mut a = app(vec![changed("a.rs", FileKind::Modified, 1)]);
        a.discard_action();
        assert!(a.pending.is_some());
        a.cancel();
        assert!(a.pending.is_none());
        assert_eq!(a.status, "Cancelled");
    }

    #[test]
    fn confirming_with_nothing_pending_does_nothing() {
        let mut a = app(Vec::new());
        a.confirm();
        assert!(a.pending.is_none());
    }

    #[test]
    fn discard_is_unavailable_in_the_staged_panel() {
        let mut a = app(vec![changed("a.rs", FileKind::Modified, 1)]);
        a.focus = Focus::Staged;
        a.discard_action();
        assert!(a.pending.is_none());
    }

    #[test]
    fn a_hunk_index_past_the_end_asks_nothing() {
        let mut a = app(vec![changed("a.rs", FileKind::Modified, 1)]);
        a.focus = Focus::Diff;
        a.selected_hunk = 5;
        a.discard_action();
        assert!(a.pending.is_none());
    }

    #[test]
    fn discard_with_no_files_asks_nothing() {
        let mut a = app(Vec::new());
        a.discard_action();
        assert!(a.pending.is_none());
    }

    #[test]
    fn reloading_keeps_the_reader_in_place() {
        let mut a = app(vec![changed("a.rs", FileKind::Modified, 3)]);
        a.focus = Focus::Diff;
        a.selected_hunk = 2;
        a.diff_scroll = 4;
        // Nothing about the file changed between loads
        a.apply_reload(Vec::new(), vec![changed("a.rs", FileKind::Modified, 3)]);
        assert_eq!(a.selected_hunk, 2, "hunk selection should survive a reload");
        assert_eq!(a.diff_scroll, 4, "scroll position should survive a reload");
    }

    #[test]
    fn reloading_clamps_the_view_when_the_diff_shrinks() {
        let mut a = app(vec![changed("a.rs", FileKind::Modified, 3)]);
        a.focus = Focus::Diff;
        a.selected_hunk = 2;
        a.diff_scroll = 5;
        // Down to one hunk: 1 header + 1 line = 2 rendered lines
        a.apply_reload(Vec::new(), vec![changed("a.rs", FileKind::Modified, 1)]);
        assert_eq!(a.selected_hunk, 0, "hunk 2 no longer exists");
        assert!(a.diff_scroll <= 1, "scroll {} is past the new end", a.diff_scroll);
    }

    #[test]
    fn selection_follows_its_file_when_the_list_shifts() {
        let mut a = app(vec![
            changed("a.rs", FileKind::Modified, 1),
            changed("b.rs", FileKind::Modified, 1),
        ]);
        a.unstaged_sel = 1; // b.rs
        // a.rs gets staged away, so b.rs slides up to index 0
        a.apply_reload(Vec::new(), vec![changed("b.rs", FileKind::Modified, 1)]);
        assert_eq!(a.unstaged_sel, 0);
        assert_eq!(a.current_file().map(|f| f.path.as_str()), Some("b.rs"));
    }

    #[test]
    fn losing_the_selected_file_starts_the_new_one_at_the_top() {
        let mut a = app(vec![
            changed("a.rs", FileKind::Modified, 3),
            changed("b.rs", FileKind::Modified, 3),
        ]);
        a.focus = Focus::Diff;
        a.unstaged_sel = 0; // a.rs
        a.selected_hunk = 2;
        a.diff_scroll = 4;
        // a.rs is gone; the selection lands on b.rs, so the old offset into
        // a.rs must not carry over
        a.apply_reload(Vec::new(), vec![changed("b.rs", FileKind::Modified, 3)]);
        assert_eq!(a.current_file().map(|f| f.path.as_str()), Some("b.rs"));
        assert_eq!(a.selected_hunk, 0);
        assert_eq!(a.diff_scroll, 0);
    }

    #[test]
    fn reloading_keeps_the_line_cursor_in_place() {
        // The idle reload fires while someone sits in line mode deciding
        // whether to discard; moving the cursor under them would be dangerous.
        let mut a = with_hunks(vec![mixed(&[
            LineKind::Context,
            LineKind::Added,
            LineKind::Added,
        ])]);
        a.drill_in();
        a.selected_line = 2;
        let same = ChangedFile {
            path: String::from("a.rs"),
            header: String::from("diff --git a/a.rs b/a.rs"),
            hunks: vec![mixed(&[LineKind::Context, LineKind::Added, LineKind::Added])],
            kind: FileKind::Modified,
        };
        a.apply_reload(Vec::new(), vec![same]);
        assert_eq!(a.diff_mode, DiffMode::Line);
        assert_eq!(a.selected_line, 2);
    }

    #[test]
    fn reloading_clamps_a_line_cursor_past_the_end_of_a_shrunken_hunk() {
        let mut a = with_hunks(vec![mixed(&[
            LineKind::Added,
            LineKind::Added,
            LineKind::Added,
        ])]);
        a.drill_in();
        a.selected_line = 2;
        let smaller = ChangedFile {
            path: String::from("a.rs"),
            header: String::from("diff --git a/a.rs b/a.rs"),
            hunks: vec![mixed(&[LineKind::Added])],
            kind: FileKind::Modified,
        };
        a.apply_reload(Vec::new(), vec![smaller]);
        assert_eq!(a.selected_line, 0, "cursor must not point past the hunk");
    }

    #[test]
    fn reloading_into_a_different_file_leaves_line_mode() {
        // A line index means nothing in a file the reader never opened.
        let mut a = with_hunks(vec![mixed(&[LineKind::Added, LineKind::Added])]);
        a.drill_in();
        a.selected_line = 1;
        let other = ChangedFile {
            path: String::from("b.rs"),
            header: String::from("diff --git a/b.rs b/b.rs"),
            hunks: vec![mixed(&[LineKind::Added])],
            kind: FileKind::Modified,
        };
        a.apply_reload(Vec::new(), vec![other]);
        assert_eq!(a.diff_mode, DiffMode::Hunk);
        assert_eq!(a.selected_line, 0);
    }

    #[test]
    fn reloading_an_empty_tree_leaves_line_mode() {
        let mut a = with_hunks(vec![mixed(&[LineKind::Added])]);
        a.drill_in();
        a.apply_reload(Vec::new(), Vec::new());
        assert_eq!(a.diff_mode, DiffMode::Hunk);
    }

    #[test]
    fn reselect_falls_back_to_a_clamped_index() {
        let files = vec![changed("x.rs", FileKind::Modified, 1)];
        // Known path wins
        assert_eq!(reselect(&files, Some("x.rs"), 9), 0);
        // Unknown path falls back to the clamped previous index
        assert_eq!(reselect(&files, Some("gone.rs"), 9), 0);
        // Empty list is always index 0
        assert_eq!(reselect(&[], Some("x.rs"), 9), 0);
    }

    #[test]
    fn reloading_an_empty_tree_resets_the_view() {
        let mut a = app(vec![changed("a.rs", FileKind::Modified, 3)]);
        a.selected_hunk = 2;
        a.diff_scroll = 4;
        a.apply_reload(Vec::new(), Vec::new());
        assert_eq!(a.selected_hunk, 0);
        assert_eq!(a.diff_scroll, 0);
    }

    #[test]
    fn staging_is_never_gated_behind_a_confirmation() {
        // Only irreversible actions confirm; s/S/u/U must stay immediate.
        let mut a = app(vec![changed("a.rs", FileKind::Modified, 1)]);
        a.focus = Focus::Diff;
        a.stage_action();
        assert!(a.pending.is_none());
    }
}
