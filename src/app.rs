use std::path::PathBuf;

use crate::git::{self, ChangedFile, FileKind};

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

/// An irreversible action waiting on the user to confirm it.
#[derive(Debug, Clone, PartialEq)]
pub enum PendingAction {
    DiscardHunk,
    DiscardFile,
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
    pub diff_scroll: usize,
    pub focus: Focus,
    /// Which file list the diff panel is currently showing.
    pub diff_source: DiffSource,
    pub status: String,
    pub should_quit: bool,
    /// Set while a destructive action awaits confirmation. While this is
    /// `Some`, the main loop routes every key to confirm/cancel and suppresses
    /// the idle auto-reload, so the indices captured here stay valid.
    pub pending: Option<Pending>,
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
            diff_scroll: 0,
            focus: Focus::Unstaged,
            diff_source: DiffSource::Unstaged,
            status: String::from("Tab: cycle panels  q: quit"),
            should_quit: false,
            pending: None,
            repo_path,
        })
    }

    pub fn reload(&mut self) {
        let (staged, unstaged) = load_all(&self.repo_path);
        self.staged_files = staged;
        self.unstaged_files = unstaged;
        self.staged_sel = self.staged_sel.min(self.staged_files.len().saturating_sub(1));
        self.unstaged_sel = self.unstaged_sel.min(self.unstaged_files.len().saturating_sub(1));
        self.selected_hunk = 0;
        self.diff_scroll = 0;
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

    pub fn enter_diff(&mut self) {
        match self.focus {
            Focus::Staged => {
                self.diff_source = DiffSource::Staged;
                self.selected_hunk = 0;
                self.diff_scroll = 0;
                self.focus = Focus::Diff;
            }
            Focus::Unstaged => {
                self.diff_source = DiffSource::Unstaged;
                self.selected_hunk = 0;
                self.diff_scroll = 0;
                self.focus = Focus::Diff;
            }
            Focus::Diff => {}
        }
    }

    pub fn exit_diff(&mut self) {
        if self.focus == Focus::Diff {
            self.focus = match self.diff_source {
                DiffSource::Staged => Focus::Staged,
                DiffSource::Unstaged => Focus::Unstaged,
            };
        }
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
            self.scroll_to_selected_hunk();
        }
    }

    pub fn hunk_down(&mut self) {
        let max = self.current_file().map(|f| f.hunks.len()).unwrap_or(0);
        if max > 0 && self.selected_hunk + 1 < max {
            self.selected_hunk += 1;
            self.scroll_to_selected_hunk();
        }
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

fn load_all(repo_path: &std::path::Path) -> (Vec<ChangedFile>, Vec<ChangedFile>) {
    let staged = git::load_staged_diff(repo_path).unwrap_or_default();
    let mut unstaged = git::load_diff(repo_path).unwrap_or_default();
    unstaged.extend(git::load_untracked(repo_path).unwrap_or_default());
    (staged, unstaged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{Hunk, HunkLine, LineKind};

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
            diff_scroll: 0,
            focus: Focus::Unstaged,
            diff_source: DiffSource::Unstaged,
            status: String::new(),
            should_quit: false,
            pending: None,
            repo_path: PathBuf::from("/nonexistent-the-diff-test"),
        }
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
    fn staging_is_never_gated_behind_a_confirmation() {
        // Only irreversible actions confirm; s/S/u/U must stay immediate.
        let mut a = app(vec![changed("a.rs", FileKind::Modified, 1)]);
        a.focus = Focus::Diff;
        a.stage_action();
        assert!(a.pending.is_none());
    }
}
