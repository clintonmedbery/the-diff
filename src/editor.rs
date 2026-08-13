//! Handing a file off to the user's editor.
//!
//! Editors disagree about how to be told which line to open, so the argument
//! list is built from the program's name. Anything unrecognised just gets the
//! path — opening at the top beats opening a junk buffer named "+42".

use std::path::Path;
use std::process::Command;

/// Which editor to hand off to, preferring `$VISUAL` over `$EDITOR`.
pub fn resolve() -> String {
    resolve_from(std::env::var("VISUAL").ok(), std::env::var("EDITOR").ok())
}

fn resolve_from(visual: Option<String>, editor: Option<String>) -> String {
    [visual, editor]
        .into_iter()
        .flatten()
        .find(|e| !e.trim().is_empty())
        .unwrap_or_else(|| String::from("vi"))
}

/// The full argument list for opening `path` at `line`.
///
/// `editor` may carry its own arguments, as in `EDITOR="emacsclient -nw"`.
pub fn argv(editor: &str, path: &str, line: u32) -> Vec<String> {
    let mut words = editor.split_whitespace().map(String::from);
    let program = words.next().unwrap_or_else(|| String::from("vi"));
    let mut out = vec![program.clone()];
    out.extend(words);

    // Match on the file name so an absolute path still resolves
    let name = Path::new(&program)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    match name {
        "vi" | "vim" | "nvim" | "view" | "nano" | "pico" | "emacs" | "emacsclient" | "joe"
        | "kak" | "micro" | "gedit" => {
            out.push(format!("+{line}"));
            out.push(path.to_string());
        }
        "code" | "code-insiders" | "codium" | "cursor" | "windsurf" => {
            out.push(String::from("-g"));
            out.push(format!("{path}:{line}"));
        }
        "hx" | "helix" | "subl" | "sublime_text" | "zed" => {
            out.push(format!("{path}:{line}"));
        }
        "idea" | "webstorm" | "pycharm" | "rubymine" | "goland" => {
            out.push(String::from("--line"));
            out.push(line.to_string());
            out.push(path.to_string());
        }
        // Unknown: a bare "+42" might be read as a filename, so skip the jump.
        _ => out.push(path.to_string()),
    }
    out
}

/// Run the editor on `path` (relative to `repo_path`) at `line`, blocking
/// until it exits. The caller is responsible for leaving and restoring the TUI.
pub fn open(repo_path: &Path, path: &str, line: u32) -> anyhow::Result<()> {
    let editor = resolve();
    let argv = argv(&editor, path, line);
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(repo_path)
        .status()
        .map_err(|e| anyhow::anyhow!("Could not run {}: {e}", argv[0]))?;
    if !status.success() {
        anyhow::bail!("{} exited with {status}", argv[0]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(editor: &str) -> Vec<String> {
        argv(editor, "src/main.rs", 12)
    }

    #[test]
    fn the_vi_family_takes_a_plus_line_argument() {
        assert_eq!(args("vim"), ["vim", "+12", "src/main.rs"]);
        assert_eq!(args("nvim"), ["nvim", "+12", "src/main.rs"]);
        assert_eq!(args("nano"), ["nano", "+12", "src/main.rs"]);
    }

    #[test]
    fn vs_code_needs_goto_and_a_suffixed_path() {
        assert_eq!(args("code"), ["code", "-g", "src/main.rs:12"]);
    }

    #[test]
    fn helix_and_sublime_take_a_suffixed_path() {
        assert_eq!(args("hx"), ["hx", "src/main.rs:12"]);
        assert_eq!(args("subl"), ["subl", "src/main.rs:12"]);
    }

    #[test]
    fn jetbrains_editors_take_a_line_flag() {
        assert_eq!(args("idea"), ["idea", "--line", "12", "src/main.rs"]);
    }

    #[test]
    fn an_unrecognised_editor_just_gets_the_path() {
        // A stray "+12" could be read as a second file to open.
        assert_eq!(args("frobnicate"), ["frobnicate", "src/main.rs"]);
    }

    #[test]
    fn an_editor_keeps_the_arguments_it_came_with() {
        assert_eq!(
            args("emacsclient -nw"),
            ["emacsclient", "-nw", "+12", "src/main.rs"]
        );
    }

    #[test]
    fn a_full_path_still_matches_on_the_program_name() {
        assert_eq!(
            args("/opt/homebrew/bin/nvim"),
            ["/opt/homebrew/bin/nvim", "+12", "src/main.rs"]
        );
    }

    #[test]
    fn visual_wins_over_editor() {
        assert_eq!(
            resolve_from(Some(String::from("nvim")), Some(String::from("vim"))),
            "nvim"
        );
    }

    #[test]
    fn an_unset_or_blank_editor_falls_back_to_vi() {
        assert_eq!(resolve_from(None, None), "vi");
        assert_eq!(resolve_from(Some(String::from("   ")), None), "vi");
        assert_eq!(
            resolve_from(Some(String::new()), Some(String::from("vim"))),
            "vim"
        );
    }
}
