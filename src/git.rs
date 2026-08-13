use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum FileKind {
    /// Tracked file with unstaged modifications
    Modified,
    /// New file not yet in the index
    Untracked,
}

#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: String,
    /// The diff --git ... +++ b/path header lines, joined with \n
    pub header: String,
    pub hunks: Vec<Hunk>,
    pub kind: FileKind,
}

#[derive(Debug, Clone)]
pub struct Hunk {
    /// The @@ -x,y +a,b @@ ... line
    pub header: String,
    pub lines: Vec<HunkLine>,
    /// First line number in the old file for this hunk
    pub old_start: u32,
    /// First line number in the new file for this hunk
    pub new_start: u32,
}

impl Hunk {
    /// Old- and new-file line numbers for every line in the hunk.
    ///
    /// A deletion exists only in the old file and an addition only in the new
    /// one, so the two sides drift apart as the hunk goes on. The gutter and
    /// the editor jump both read from here so they cannot disagree.
    pub fn numbering(&self) -> Vec<(Option<u32>, Option<u32>)> {
        let mut old = self.old_start;
        let mut new = self.new_start;
        self.lines
            .iter()
            .map(|line| match line.kind {
                LineKind::Added => {
                    let n = new;
                    new += 1;
                    (None, Some(n))
                }
                LineKind::Removed => {
                    let o = old;
                    old += 1;
                    (Some(o), None)
                }
                LineKind::Context => {
                    let (o, n) = (old, new);
                    old += 1;
                    new += 1;
                    (Some(o), Some(n))
                }
                LineKind::NoNewline => (None, None),
            })
            .collect()
    }

    /// The line an editor should open at for `line_idx`.
    ///
    /// Deletions have no line on disk, so they resolve to the position the
    /// line used to occupy — the next new-side line.
    pub fn target_line(&self, line_idx: usize) -> u32 {
        let mut new = self.new_start;
        for (i, line) in self.lines.iter().enumerate() {
            if i == line_idx {
                break;
            }
            if matches!(line.kind, LineKind::Added | LineKind::Context) {
                new += 1;
            }
        }
        new.max(1)
    }
}

#[derive(Debug, Clone)]
pub struct HunkLine {
    /// Raw line content including the leading +/-/space character
    pub content: String,
    pub kind: LineKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LineKind {
    Added,
    Removed,
    Context,
    /// The "\ No newline at end of file" marker
    NoNewline,
}

/// Run `git diff --cached` and return staged changes (index vs HEAD).
pub fn load_staged_diff(repo_path: &Path) -> Result<Vec<ChangedFile>> {
    let output = Command::new("git")
        .args(["diff", "--cached"])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git diff --cached")?;

    let text = String::from_utf8(output.stdout).context("git diff --cached output is not valid UTF-8")?;
    Ok(parse_diff(&text))
}

/// Unstage a single hunk by reversing it in the index.
pub fn unstage_hunk(repo_path: &Path, file: &ChangedFile, hunk_idx: usize) -> Result<()> {
    let patch = build_patch(file, hunk_idx);
    apply_patch(repo_path, &patch, &["--cached", "--reverse"])
}

/// Unstage an entire file with `git reset HEAD`.
pub fn unstage_file(repo_path: &Path, path: &str) -> Result<()> {
    // Use output() to capture stdout/stderr — git reset prints "Unstaged changes
    // after reset:" which would corrupt the TUI if written to the terminal directly.
    let out = Command::new("git")
        .args(["reset", "HEAD", "--", path])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git reset")?;
    if !out.status.success() {
        anyhow::bail!("git reset failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Run `git diff` and parse the output into a list of changed files with hunks.
pub fn load_diff(repo_path: &Path) -> Result<Vec<ChangedFile>> {
    let output = Command::new("git")
        .args(["diff"])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git diff")?;

    if !output.status.success() {
        anyhow::bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let text = String::from_utf8(output.stdout).context("git diff output is not valid UTF-8")?;
    Ok(parse_diff(&text))
}

fn parse_diff(input: &str) -> Vec<ChangedFile> {
    let mut files: Vec<ChangedFile> = Vec::new();
    let mut header_buf: Vec<String> = Vec::new();
    let mut current_file: Option<ChangedFile> = None;
    let mut current_hunk: Option<Hunk> = None;

    for line in input.lines() {
        if line.starts_with("diff --git ") {
            flush_hunk(&mut current_file, &mut current_hunk);
            if let Some(f) = current_file.take() {
                files.push(f);
            }
            header_buf.clear();
            header_buf.push(line.to_string());
        } else if line.starts_with("index ")
            || line.starts_with("new file")
            || line.starts_with("deleted file")
            || line.starts_with("old mode")
            || line.starts_with("new mode")
            || line.starts_with("similarity")
            || line.starts_with("rename")
            || line.starts_with("--- ")
        {
            header_buf.push(line.to_string());
        } else if line.starts_with("+++ ") {
            header_buf.push(line.to_string());
            let path = extract_b_path(line);
            current_file = Some(ChangedFile {
                path,
                header: header_buf.join("\n"),
                hunks: Vec::new(),
                kind: FileKind::Modified,
            });
        } else if line.starts_with("@@ ") {
            flush_hunk(&mut current_file, &mut current_hunk);
            let (old_start, new_start) = parse_hunk_range(line);
            current_hunk = Some(Hunk {
                header: line.to_string(),
                lines: Vec::new(),
                old_start,
                new_start,
            });
        } else if let Some(ref mut hunk) = current_hunk {
            let kind = if line.starts_with('+') {
                LineKind::Added
            } else if line.starts_with('-') {
                LineKind::Removed
            } else if line.starts_with('\\') {
                LineKind::NoNewline
            } else {
                LineKind::Context
            };
            hunk.lines.push(HunkLine {
                content: line.to_string(),
                kind,
            });
        }
    }

    flush_hunk(&mut current_file, &mut current_hunk);
    if let Some(f) = current_file {
        files.push(f);
    }

    files
}

fn flush_hunk(file: &mut Option<ChangedFile>, hunk: &mut Option<Hunk>) {
    if let (Some(f), Some(h)) = (file.as_mut(), hunk.take()) {
        f.hunks.push(h);
    }
}

/// Parse `@@ -old_start[,count] +new_start[,count] @@` into (old_start, new_start).
fn parse_hunk_range(header: &str) -> (u32, u32) {
    let mut parts = header.split_whitespace().skip(1); // skip "@@"
    let parse = |s: &str| -> u32 {
        let s = s.trim_start_matches(['-', '+']);
        let end = s.find(',').unwrap_or(s.len());
        s[..end].parse().unwrap_or(1)
    };
    let old = parts.next().map(parse).unwrap_or(1);
    let new = parts.next().map(parse).unwrap_or(1);
    (old, new)
}

fn extract_b_path(line: &str) -> String {
    // "+++ b/src/foo.rs" -> "src/foo.rs"
    // "+++ /dev/null"    -> "/dev/null"
    if let Some(rest) = line.strip_prefix("+++ b/") {
        rest.to_string()
    } else if let Some(rest) = line.strip_prefix("+++ ") {
        rest.to_string()
    } else {
        line.to_string()
    }
}

/// Stage a single hunk by piping its patch to `git apply --cached`.
pub fn stage_hunk(repo_path: &Path, file: &ChangedFile, hunk_idx: usize) -> Result<()> {
    let patch = build_patch(file, hunk_idx);
    apply_patch(repo_path, &patch, &["--cached"])
}

/// Discard a single hunk by piping its reverse patch to `git apply --reverse`.
pub fn discard_hunk(repo_path: &Path, file: &ChangedFile, hunk_idx: usize) -> Result<()> {
    let patch = build_patch(file, hunk_idx);
    apply_patch(repo_path, &patch, &["--reverse"])
}

/// Stage an entire file with `git add`.
pub fn stage_file(repo_path: &Path, path: &str) -> Result<()> {
    let out = Command::new("git")
        .args(["add", "--", path])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git add")?;
    if !out.status.success() {
        anyhow::bail!("git add failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Discard all changes to a file, restoring it from HEAD.
pub fn discard_file(repo_path: &Path, path: &str) -> Result<()> {
    let out = Command::new("git")
        .args(["checkout", "HEAD", "--", path])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git checkout")?;
    if !out.status.success() {
        anyhow::bail!("git checkout failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Delete an untracked file from disk.
pub fn delete_file(repo_path: &Path, path: &str) -> Result<()> {
    std::fs::remove_file(repo_path.join(path))
        .with_context(|| format!("Failed to delete {path}"))
}

/// Return all untracked files (new files not yet in the index) as ChangedFiles
/// whose hunks show the full file content as additions.
pub fn load_untracked(repo_path: &Path) -> Result<Vec<ChangedFile>> {
    let ls = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git ls-files")?;

    let paths_text =
        String::from_utf8(ls.stdout).context("Invalid UTF-8 in ls-files output")?;

    let mut files = Vec::new();
    for path in paths_text.lines().filter(|p| !p.is_empty()) {
        // git diff --no-index exits with 1 when files differ — that is normal here
        let diff_out = Command::new("git")
            .args(["diff", "--no-index", "--", "/dev/null", path])
            .current_dir(repo_path)
            .output()
            .ok();

        let hunks = diff_out
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|text| {
                parse_diff(&text)
                    .into_iter()
                    .flat_map(|f| f.hunks)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // Synthetic header — only used if we ever try git apply, but for untracked
        // files we always use `git add` instead, so this is just informational.
        let header = format!(
            "diff --git a/dev/null b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}"
        );

        files.push(ChangedFile {
            path: path.to_string(),
            header,
            hunks,
            kind: FileKind::Untracked,
        });
    }

    Ok(files)
}

fn build_patch(file: &ChangedFile, hunk_idx: usize) -> String {
    let hunk = &file.hunks[hunk_idx];
    let mut out = String::new();
    out.push_str(&file.header);
    out.push('\n');
    out.push_str(&hunk.header);
    out.push('\n');
    for line in &hunk.lines {
        out.push_str(&line.content);
        out.push('\n');
    }
    out
}

/// Discard a single line from the working tree.
pub fn discard_line(
    repo_path: &Path,
    file: &ChangedFile,
    hunk_idx: usize,
    line_idx: usize,
) -> Result<()> {
    let patch = line_patch(file, hunk_idx, line_idx)?;
    apply_patch(repo_path, &patch, &["--reverse", "--recount"])
}

/// Unstage a single line, leaving the working tree untouched.
pub fn unstage_line(
    repo_path: &Path,
    file: &ChangedFile,
    hunk_idx: usize,
    line_idx: usize,
) -> Result<()> {
    let patch = line_patch(file, hunk_idx, line_idx)?;
    apply_patch(repo_path, &patch, &["--cached", "--reverse", "--recount"])
}

fn line_patch(file: &ChangedFile, hunk_idx: usize, line_idx: usize) -> Result<String> {
    build_line_patch(file, hunk_idx, line_idx)
        .context("That line is not an addition or a deletion")
}

/// Build a patch carrying exactly one changed line out of `hunk_idx`.
///
/// The result is faithful to the **new** side of the diff, so it is only valid
/// reverse-applied (`--reverse`, optionally with `--cached`). Every line the
/// caller did not pick is rewritten to describe the target as it already
/// stands: sibling additions are present, so they become context, and sibling
/// deletions are absent, so they are omitted entirely. Reverse-applying the
/// result therefore undoes the selected line and nothing else.
///
/// Returns `None` for a context line or a `\ No newline` marker, neither of
/// which is a change that can be undone on its own.
pub fn build_line_patch(file: &ChangedFile, hunk_idx: usize, line_idx: usize) -> Option<String> {
    let hunk = file.hunks.get(hunk_idx)?;
    let selected = hunk.lines.get(line_idx)?;
    if !matches!(selected.kind, LineKind::Added | LineKind::Removed) {
        return None;
    }

    let mut body: Vec<String> = Vec::new();
    let mut old_count = 0u32;
    let mut new_count = 0u32;
    // Deletions dropped ahead of the first emitted line move the old-side
    // start: the hunk now begins later in the old file than it used to.
    let mut old_skipped = 0u32;
    // Whether the most recent non-marker line survived, so a trailing
    // "\ No newline" marker can follow its owner in or out of the patch.
    let mut owner_kept = false;

    for (i, line) in hunk.lines.iter().enumerate() {
        let rest = line.content.get(1..).unwrap_or("");
        match line.kind {
            LineKind::Context => {
                body.push(line.content.clone());
                old_count += 1;
                new_count += 1;
                owner_kept = true;
            }
            LineKind::Removed if i == line_idx => {
                body.push(line.content.clone());
                old_count += 1;
                owner_kept = true;
            }
            LineKind::Removed => {
                // Not in the target at all, so it cannot appear in the patch.
                if body.is_empty() {
                    old_skipped += 1;
                }
                owner_kept = false;
            }
            LineKind::Added if i == line_idx => {
                body.push(line.content.clone());
                new_count += 1;
                owner_kept = true;
            }
            LineKind::Added => {
                // Already in the target, so it is unchanged context here.
                body.push(format!(" {rest}"));
                old_count += 1;
                new_count += 1;
                owner_kept = true;
            }
            LineKind::NoNewline => {
                if owner_kept {
                    body.push(line.content.clone());
                }
            }
        }
    }

    // An untracked file's stored header describes creating it from /dev/null.
    // Reversed, that reads as "delete the file", so reframe the patch as an
    // ordinary edit — and start the old side where the new side does, since
    // the parsed "-0,0" of a whole-file addition has no line to anchor to.
    let untracked = file.kind == FileKind::Untracked;
    let (header, old_base) = if untracked {
        (
            format!(
                "diff --git a/{p} b/{p}\n--- a/{p}\n+++ b/{p}",
                p = file.path
            ),
            hunk.new_start,
        )
    } else {
        (file.header.clone(), hunk.old_start)
    };

    let mut out = String::new();
    out.push_str(&header);
    out.push('\n');
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        old_base + old_skipped,
        old_count,
        hunk.new_start,
        new_count
    ));
    for line in &body {
        out.push_str(line);
        out.push('\n');
    }
    Some(out)
}

fn apply_patch(repo_path: &Path, patch: &str, extra_args: &[&str]) -> Result<()> {
    let mut args = vec!["-c", "core.autocrlf=false", "apply"];
    args.extend_from_slice(extra_args);

    let mut child = Command::new("git")
        .args(&args)
        .current_dir(repo_path)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to spawn git apply")?;

    {
        let stdin = child.stdin.as_mut().context("Failed to open stdin")?;
        stdin.write_all(patch.as_bytes()).context("Failed to write patch")?;
    }

    let output = child.wait_with_output().context("Failed to wait for git apply")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git apply failed: {}", stderr.trim());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── Real-repository fixtures ─────────────────────────────────────────────
    //
    // String assertions prove a patch is shaped as intended; only git can say
    // it is *valid*. The hunk start arithmetic in build_line_patch is exactly
    // where hand-rolled patch builders break, so these tests hand each patch
    // to the real `git apply` and check what lands on disk.

    /// A throwaway git repository, deleted when the test ends.
    struct TempRepo {
        path: PathBuf,
    }

    impl TempRepo {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "the-diff-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp repo");
            let repo = TempRepo { path };
            repo.git(&["init", "-q"]);
            repo
        }

        /// Run git with identity and signing pinned, so the ambient user
        /// configuration cannot make these tests pass or fail.
        fn git(&self, args: &[&str]) -> String {
            let out = Command::new("git")
                .args([
                    "-c",
                    "user.email=test@example.com",
                    "-c",
                    "user.name=Test",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.autocrlf=false",
                ])
                .args(args)
                .current_dir(&self.path)
                .output()
                .expect("run git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).into_owned()
        }

        fn write(&self, name: &str, contents: &str) {
            std::fs::write(self.path.join(name), contents).expect("write file");
        }

        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.path.join(name)).expect("read file")
        }

        /// Commit `initial` as f.txt, then leave `modified` in the working tree.
        fn with_change(initial: &str, modified: &str) -> Self {
            let repo = TempRepo::new();
            repo.write("f.txt", initial);
            repo.git(&["add", "f.txt"]);
            repo.git(&["commit", "-qm", "initial"]);
            repo.write("f.txt", modified);
            repo
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Index of the first line in hunk 0 whose content matches `needle`.
    fn line_at(file: &ChangedFile, needle: &str) -> usize {
        file.hunks[0]
            .lines
            .iter()
            .position(|l| l.content == needle)
            .unwrap_or_else(|| panic!("no line {needle:?} in {:?}", file.hunks[0].lines))
    }

    #[test]
    fn discarding_one_added_line_leaves_its_siblings_on_disk() {
        let repo = TempRepo::with_change("a\nb\n", "a\nX\nY\nb\n");
        let files = load_diff(&repo.path).expect("load diff");
        let idx = line_at(&files[0], "+X");
        discard_line(&repo.path, &files[0], 0, idx).expect("discard +X");
        assert_eq!(repo.read("f.txt"), "a\nY\nb\n");
    }

    #[test]
    fn discarding_one_deletion_restores_only_that_line() {
        let repo = TempRepo::with_change("a\nb\nc\n", "a\n");
        let files = load_diff(&repo.path).expect("load diff");
        let idx = line_at(&files[0], "-b");
        discard_line(&repo.path, &files[0], 0, idx).expect("discard -b");
        assert_eq!(repo.read("f.txt"), "a\nb\n");
    }

    #[test]
    fn discarding_an_addition_that_lacks_a_trailing_newline_restores_the_newline() {
        // The "\ No newline" marker describes the new side. Undoing the line it
        // belongs to has to put the file's final newline back.
        let repo = TempRepo::with_change("a\nb\n", "a\nb\nc");
        let files = load_diff(&repo.path).expect("load diff");
        let idx = line_at(&files[0], "+c");
        discard_line(&repo.path, &files[0], 0, idx).expect("discard +c");
        assert_eq!(repo.read("f.txt"), "a\nb\n");
    }

    #[test]
    fn discarding_the_first_line_of_a_hunk_does_not_shift_the_edit() {
        // A deletion at the very top of a hunk is the case where a stale
        // old-side start would land the edit on the wrong line.
        let repo = TempRepo::with_change("a\nb\nc\n", "b\nc\n");
        let files = load_diff(&repo.path).expect("load diff");
        let idx = line_at(&files[0], "-a");
        discard_line(&repo.path, &files[0], 0, idx).expect("discard -a");
        assert_eq!(repo.read("f.txt"), "a\nb\nc\n");
    }

    #[test]
    fn discarding_a_line_in_a_later_hunk_leaves_the_earlier_one_alone() {
        // Two hunks far enough apart that git does not merge them. Only the
        // second hunk's patch is built, so its start numbers have to be right
        // or the edit lands in the wrong part of the file.
        let mut initial = String::new();
        for i in 1..=40 {
            initial.push_str(&format!("line {i}\n"));
        }
        let mut modified = initial.clone();
        modified = modified.replace("line 2\n", "line 2\nEARLY\n");
        modified = modified.replace("line 38\n", "line 38\nLATE\n");

        let repo = TempRepo::with_change(&initial, &modified);
        let files = load_diff(&repo.path).expect("load diff");
        assert_eq!(files[0].hunks.len(), 2, "expected two separate hunks");

        let idx = files[0].hunks[1]
            .lines
            .iter()
            .position(|l| l.content == "+LATE")
            .expect("+LATE in hunk 1");
        discard_line(&repo.path, &files[0], 1, idx).expect("discard +LATE");

        let after = repo.read("f.txt");
        assert!(!after.contains("LATE"), "the late addition should be gone");
        assert!(after.contains("EARLY"), "the early addition must survive");
    }

    #[test]
    fn discarding_a_line_from_an_untracked_file_keeps_the_rest() {
        let repo = TempRepo::new();
        repo.write("f.txt", "one\ntwo\nthree\n");
        let files = load_untracked(&repo.path).expect("load untracked");
        let idx = line_at(&files[0], "+two");
        discard_line(&repo.path, &files[0], 0, idx).expect("discard +two");
        assert_eq!(repo.read("f.txt"), "one\nthree\n");
        // The file must still exist and still be untracked
        assert_eq!(repo.git(&["ls-files", "f.txt"]).trim(), "");
    }

    #[test]
    fn unstaging_one_line_leaves_the_rest_staged() {
        let repo = TempRepo::with_change("a\nb\n", "a\nX\nY\nb\n");
        repo.git(&["add", "f.txt"]);
        let files = load_staged_diff(&repo.path).expect("load staged diff");
        let idx = line_at(&files[0], "+X");
        unstage_line(&repo.path, &files[0], 0, idx).expect("unstage +X");
        // Y stays staged, X falls back to being an unstaged change
        assert!(repo.git(&["diff", "--cached"]).contains("+Y"));
        assert!(!repo.git(&["diff", "--cached"]).contains("+X"));
        assert!(repo.git(&["diff"]).contains("+X"));
        // Unstaging must never touch the working tree
        assert_eq!(repo.read("f.txt"), "a\nX\nY\nb\n");
    }

    const SAMPLE: &str = r#"diff --git a/src/main.rs b/src/main.rs
index 1234567..89abcde 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
 fn main() {
-    println!("old");
+    println!("new");
+    // added
 }
@@ -10,2 +11,2 @@ fn helper() {
-    let a = 1;
+    let a = 2;
"#;

    #[test]
    fn parses_one_file_with_two_hunks() {
        let files = parse_diff(SAMPLE);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[0].kind, FileKind::Modified);
    }

    #[test]
    fn classifies_line_kinds() {
        let files = parse_diff(SAMPLE);
        let first = &files[0].hunks[0];
        let count = |k: LineKind| first.lines.iter().filter(|l| l.kind == k).count();
        assert_eq!(
            (count(LineKind::Added), count(LineKind::Removed), count(LineKind::Context)),
            (2, 1, 2)
        );
    }

    #[test]
    fn header_is_captured_so_patches_can_be_rebuilt() {
        let files = parse_diff(SAMPLE);
        assert!(files[0].header.starts_with("diff --git a/src/main.rs b/src/main.rs"));
        assert!(files[0].header.contains("+++ b/src/main.rs"));
    }

    #[test]
    fn numbering_advances_each_side_independently() {
        let files = parse_diff(SAMPLE);
        assert_eq!(
            files[0].hunks[0].numbering(),
            vec![
                (Some(1), Some(1)), // context
                (Some(2), None),    // deletion: old side only
                (None, Some(2)),    // addition: new side only
                (None, Some(3)),
                (Some(3), Some(4)), // context, now out of step
            ]
        );
    }

    #[test]
    fn a_marker_belongs_to_neither_side() {
        let files = parse_diff(NO_EOL);
        assert_eq!(files[0].hunks[0].numbering().last(), Some(&(None, None)));
    }

    #[test]
    fn an_editor_opens_an_added_line_at_its_own_number() {
        let files = parse_diff(SAMPLE);
        assert_eq!(files[0].hunks[0].target_line(2), 2);
        assert_eq!(files[0].hunks[0].target_line(3), 3);
    }

    #[test]
    fn an_editor_opens_a_deleted_line_where_it_used_to_be() {
        // A deletion has no line in the file on disk, so the cursor goes to
        // the position it occupied — the next new-side line.
        let files = parse_diff(SAMPLE);
        assert_eq!(files[0].hunks[0].target_line(1), 2);
    }

    #[test]
    fn a_target_line_is_never_zero() {
        // A whole-file deletion parses as "+0,0", and no editor accepts line 0.
        let hunk = Hunk {
            header: String::from("@@ -1,1 +0,0 @@"),
            lines: vec![HunkLine { content: String::from("-gone"), kind: LineKind::Removed }],
            old_start: 1,
            new_start: 0,
        };
        assert_eq!(hunk.target_line(0), 1);
    }

    #[test]
    fn hunk_ranges_are_parsed() {
        assert_eq!(parse_hunk_range("@@ -1,3 +1,4 @@"), (1, 1));
        assert_eq!(parse_hunk_range("@@ -10,2 +11,2 @@ fn helper() {"), (10, 11));
        // Single-line hunks omit the count
        assert_eq!(parse_hunk_range("@@ -5 +7 @@"), (5, 7));
    }

    #[test]
    fn b_path_is_extracted() {
        assert_eq!(extract_b_path("+++ b/src/foo.rs"), "src/foo.rs");
        assert_eq!(extract_b_path("+++ /dev/null"), "/dev/null");
    }

    #[test]
    fn no_newline_marker_is_its_own_kind() {
        let diff = r"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
\ No newline at end of file
";
        let files = parse_diff(diff);
        let last = files[0].hunks[0].lines.last().map(|l| l.kind.clone());
        assert_eq!(last, Some(LineKind::NoNewline));
    }

    #[test]
    fn empty_diff_yields_no_files() {
        assert!(parse_diff("").is_empty());
    }

    /// Body of a patch: everything after the `@@` header line.
    fn body(patch: &str) -> Vec<&str> {
        patch.lines().skip_while(|l| !l.starts_with("@@")).skip(1).collect()
    }

    fn header_of(patch: &str) -> &str {
        patch.lines().find(|l| l.starts_with("@@")).expect("no @@ header")
    }

    #[test]
    fn selecting_an_added_line_demotes_the_other_additions_to_context() {
        // Reverse-applying this must remove only `println!("new")`, so every
        // other line has to describe the working tree exactly as it is: the
        // sibling addition is already on disk (context) and the deletion is
        // not on disk at all (omitted).
        let files = parse_diff(SAMPLE);
        let patch = build_line_patch(&files[0], 0, 2).expect("added line is discardable");
        assert_eq!(
            body(&patch),
            vec![
                " fn main() {",
                "+    println!(\"new\");",
                "     // added",
                " }",
            ]
        );
    }

    #[test]
    fn selecting_a_removed_line_keeps_it_and_demotes_the_additions() {
        let files = parse_diff(SAMPLE);
        let patch = build_line_patch(&files[0], 0, 1).expect("removed line is discardable");
        assert_eq!(
            body(&patch),
            vec![
                " fn main() {",
                "-    println!(\"old\");",
                "     println!(\"new\");",
                "     // added",
                " }",
            ]
        );
    }

    #[test]
    fn counts_describe_the_synthesized_hunk_not_the_original() {
        let files = parse_diff(SAMPLE);
        // 3 context + 1 addition
        assert_eq!(header_of(&build_line_patch(&files[0], 0, 2).unwrap()), "@@ -1,3 +1,4 @@");
        // 4 context + 1 deletion on the old side, 4 on the new
        assert_eq!(header_of(&build_line_patch(&files[0], 0, 1).unwrap()), "@@ -1,5 +1,4 @@");
    }

    #[test]
    fn omitting_a_leading_deletion_shifts_the_old_start() {
        // Hunk 1 is "-let a = 1; / +let a = 2;" at old line 10, new line 11.
        // Dropping the deletion means the patch now begins one line later in
        // the old file, and a stale start would place the edit on the wrong line.
        let files = parse_diff(SAMPLE);
        let patch = build_line_patch(&files[0], 1, 1).expect("added line is discardable");
        assert_eq!(header_of(&patch), "@@ -11,0 +11,1 @@");
        assert_eq!(body(&patch), vec!["+    let a = 2;"]);
    }

    #[test]
    fn a_context_line_has_nothing_to_discard() {
        let files = parse_diff(SAMPLE);
        assert!(build_line_patch(&files[0], 0, 0).is_none());
    }

    #[test]
    fn a_line_index_past_the_end_yields_no_patch() {
        let files = parse_diff(SAMPLE);
        assert!(build_line_patch(&files[0], 0, 99).is_none());
    }

    /// A hunk whose final addition has no trailing newline.
    const NO_EOL: &str = r"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
\ No newline at end of file
";

    #[test]
    fn the_no_newline_marker_travels_with_the_line_it_describes() {
        // Leaving the marker behind would silently append a newline to the file.
        let files = parse_diff(NO_EOL);
        let patch = build_line_patch(&files[0], 0, 1).expect("added line is discardable");
        assert_eq!(body(&patch), vec!["+new", r"\ No newline at end of file"]);
    }

    #[test]
    fn a_marker_is_dropped_along_with_its_omitted_owner() {
        // Here the marker belongs to the deletion, which is not in the working
        // tree — so neither the deletion nor its marker may appear.
        let diff = r"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
 keep
-old
\ No newline at end of file
+new
";
        let files = parse_diff(diff);
        let patch = build_line_patch(&files[0], 0, 3).expect("added line is discardable");
        assert_eq!(body(&patch), vec![" keep", "+new"]);
    }

    #[test]
    fn an_untracked_file_gets_a_modification_header() {
        // load_untracked synthesizes a "new file" header against /dev/null.
        // Reverse-applying that means "delete the whole file", so a line patch
        // has to reframe it as an ordinary edit to an existing file.
        let file = ChangedFile {
            path: String::from("notes.txt"),
            header: String::from(
                "diff --git a/dev/null b/notes.txt\nnew file mode 100644\n--- /dev/null\n+++ b/notes.txt",
            ),
            hunks: parse_diff(
                "diff --git a/dev/null b/notes.txt\n--- /dev/null\n+++ b/notes.txt\n@@ -0,0 +1,2 @@\n+one\n+two\n",
            )
            .remove(0)
            .hunks,
            kind: FileKind::Untracked,
        };
        let patch = build_line_patch(&file, 0, 0).expect("added line is discardable");
        assert!(patch.starts_with("diff --git a/notes.txt b/notes.txt\n"), "{patch}");
        assert!(patch.contains("--- a/notes.txt\n"), "{patch}");
        assert!(patch.contains("+++ b/notes.txt\n"), "{patch}");
        assert!(!patch.contains("/dev/null"), "{patch}");
        assert!(!patch.contains("new file mode"), "{patch}");
        // The old side has to start where the new side does; the parsed
        // "-0,0" of a whole-file addition would put the edit before line one.
        assert_eq!(header_of(&patch), "@@ -1,1 +1,2 @@");
    }

    #[test]
    fn the_file_header_is_carried_into_a_line_patch() {
        let files = parse_diff(SAMPLE);
        let patch = build_line_patch(&files[0], 0, 2).unwrap();
        assert!(patch.starts_with("diff --git a/src/main.rs b/src/main.rs"));
        assert!(patch.ends_with('\n'));
    }

    #[test]
    fn build_patch_emits_only_the_requested_hunk() {
        let files = parse_diff(SAMPLE);
        let patch = build_patch(&files[0], 1);
        assert!(patch.starts_with("diff --git"));
        assert!(patch.contains("@@ -10,2 +11,2 @@"));
        // The first hunk's contents must not leak in
        assert!(!patch.contains("println!"));
        assert!(patch.ends_with('\n'));
    }
}
