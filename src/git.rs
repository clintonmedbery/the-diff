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

fn apply_patch(repo_path: &Path, patch: &str, extra_args: &[&str]) -> Result<()> {
    let mut args = vec!["apply"];
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
