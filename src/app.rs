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
            Focus::Diff => self.hunk_up(),
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
            Focus::Diff => self.hunk_down(),
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

    /// `d`: discard hunk (diff panel) or discard file (unstaged list).
    pub fn discard_action(&mut self) {
        match self.focus {
            Focus::Unstaged => self.discard_file(),
            Focus::Diff if self.diff_source == DiffSource::Unstaged => self.discard_hunk(),
            _ => {}
        }
    }

    /// `D`: discard entire file (unstaged context only).
    pub fn discard_file_action(&mut self) {
        match self.focus {
            Focus::Unstaged => self.discard_file(),
            Focus::Diff if self.diff_source == DiffSource::Unstaged => self.discard_file(),
            _ => {}
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
