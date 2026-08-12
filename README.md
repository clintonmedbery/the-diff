# the-diff

A terminal UI for reviewing and staging git changes hunk by hunk.

`the-diff` shows your unstaged and staged changes side by side with the diff, and
lets you stage, unstage, or discard individual hunks without leaving the
keyboard. It is a small, focused alternative to `git add -p`.

<!-- TODO: add a screenshot or terminal recording here -->

## Install

From source, with a Rust toolchain installed:

```sh
git clone https://github.com/clintonmedbery/the-diff
cd the-diff
cargo install --path .
```

Or just build and run it in place:

```sh
cargo run --release
```

## Usage

Run `the-diff` from anywhere inside a git repository:

```sh
the-diff
```

It walks up from the current directory to find the repository root, so it works
from subdirectories. If you are not inside a git repo it exits with an error.

The screen is split into three panels:

- **Unstaged** (top left) — modified tracked files, plus untracked files below a
  separator
- **Staged** (bottom left) — what is currently in the index
- **Diff** (right) — the hunks of the selected file, with line numbers

## Keybindings

### Navigation

| Key | Action |
| --- | --- |
| `Tab` | Cycle focus: Unstaged → Staged → Diff |
| `Enter` | Focus the diff panel for the selected file |
| `Esc` | Leave the diff panel, back to its file list |
| `↑` / `k` | Move up in a file list, or scroll up in the diff |
| `↓` / `j` | Move down in a file list, or scroll down in the diff |
| `[` / `]` | Jump to the previous / next hunk |
| `PageUp` / `PageDown` | Scroll the diff by half a screen |
| Mouse wheel | Scroll the diff from anywhere in the window |
| `r` | Reload the diff |
| `q` / `Q` / `Ctrl-C` | Quit |

### Staging

Lowercase acts on the selected hunk when the diff panel is focused, and on the
selected file when a file list is focused. Uppercase always acts on the whole
file.

| Key | Action | Available in |
| --- | --- | --- |
| `s` | Stage hunk or file | Unstaged |
| `S` | Stage whole file | Unstaged |
| `u` | Unstage hunk or file | Staged |
| `U` | Unstage whole file | Staged |
| `d` | Discard hunk or file | Unstaged |
| `D` | Discard whole file | Unstaged |

Discarding is irreversible, so `d` and `D` open a confirmation dialog. Press `y`
to go through with it; any other key cancels. For an untracked file, "discard"
deletes the file from disk.

The diff auto-reloads after 10 seconds of inactivity, so changes you make in
your editor show up without pressing `r`.

## How it works

`the-diff` shells out to `git` rather than linking a git library. It reads state
with `git diff`, `git diff --cached`, and `git ls-files --others`, and it makes
changes by piping single-hunk patches to `git apply` (with `--cached` to stage
and `--reverse` to unstage or discard). Whole-file operations use `git add`,
`git reset HEAD`, and `git checkout HEAD`.

This means it has no opinion about your git config, hooks, or version, and
anything it does is a normal git operation you could have typed yourself.

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you shall be dual-licensed as above, without any
additional terms or conditions.
