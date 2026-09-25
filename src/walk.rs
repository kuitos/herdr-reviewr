//! The `All files` listing for a directory outside any git repository.
//!
//! Inside a repo the listing is `git::all_files` (`ls-files`). Outside one there is no index to
//! ask, so this walks the filesystem with the same rules a repo would apply: `.gitignore` files
//! (and the global excludes file) are honored, dotfiles are listed, `.git` never is. The result
//! has `git::all_files`'s shape — files only, an ignored directory collapsed to one `is_dir`
//! placeholder, sorted — so the tree, the lazy expansion (`git::list_ignored_dir`), and the
//! viewer cannot tell the two apart.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::git::WorktreeEntry;

/// The most entries one listing collects. A plain directory can be anything — `$HOME`
/// included — so the walk stops here rather than hold the pane on a tree it cannot show.
pub const LISTING_CAP: usize = 20_000;

/// One plain-directory listing: the entries and whether the walk stopped at the cap.
#[derive(Debug, Default)]
pub struct Listing {
    pub entries: Vec<WorktreeEntry>,
    pub capped: bool,
}

/// Every entry under `root` for the `All files` tab, walking at most `cap` entries.
///
/// Files the ignore rules keep come back as themselves. A child of a walked directory that the
/// walk neither yielded nor entered is ignored, and comes back as a placeholder the way
/// `ls-files --others --ignored --directory` reports one: a directory as `is_dir` (skipped when
/// empty, as `--no-empty-directory` does), a file as itself. A walk the cap stopped emits no
/// placeholders, since a directory it stopped inside would read its unwalked children as
/// ignored; placeholders themselves spend the same budget.
pub fn plain_files(root: &Path, cap: usize) -> Result<Listing> {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        // `ls-files` lists dotfiles; only the ignore rules hide a path.
        .hidden(false)
        // Outside a repo `.gitignore` is still the author's intent. Without this the walker
        // skips every git rule for want of a `.git` directory.
        .require_git(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(false)
        .ignore(true)
        // The reviewed directory is the root of what is shown, so rules above it do not apply.
        .parents(false)
        .follow_links(false)
        .filter_entry(|e| e.file_name() != ".git");
    let mut walked: HashSet<PathBuf> = HashSet::new();
    let mut dirs: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut entries = Vec::new();
    // Walked paths and placeholders both spend the budget, so neither a deep tree of empty
    // directories nor one huge ignored directory can run past it.
    let mut spent = 0;
    let mut capped = false;
    for result in builder.build() {
        // An unreadable entry drops out rather than failing the listing, the same best-effort
        // stance as `git::list_ignored_dir`.
        let Ok(entry) = result else { continue };
        if entry.depth() == 0 {
            continue;
        }
        if spent >= cap {
            capped = true;
            break;
        }
        spent += 1;
        let path = entry.path().to_path_buf();
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
        if is_dir {
            dirs.push(path.clone());
        } else if let Some(rel) = relative(root, &path) {
            // A symlink is not a directory here, so it lists as a file, as `ls-files` does.
            entries.push(WorktreeEntry { path: rel, ignored: false, is_dir: false });
        }
        walked.insert(path);
    }
    if !capped {
        'dirs: for dir in &dirs {
            let Ok(children) = std::fs::read_dir(dir) else { continue };
            for child in children.flatten() {
                let path = child.path();
                if child.file_name() == ".git" || walked.contains(&path) {
                    continue;
                }
                let is_dir = child.file_type().is_ok_and(|t| t.is_dir());
                if is_dir && std::fs::read_dir(&path).map_or(true, |mut d| d.next().is_none()) {
                    continue;
                }
                let Some(rel) = relative(root, &path) else { continue };
                if spent >= cap {
                    capped = true;
                    break 'dirs;
                }
                spent += 1;
                entries.push(WorktreeEntry { path: rel, ignored: true, is_dir });
            }
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Listing { entries, capped })
}

/// `path` relative to `root`, `/`-separated like `ls-files` output, or `None` for a name that
/// is not UTF-8 (the tree keys on `String` paths).
fn relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    Some(parts?.join("/"))
}

#[cfg(test)]
mod tests {
    use super::{Listing, plain_files};
    use crate::git::WorktreeEntry;
    use std::fs;
    use std::path::Path;

    fn write(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn listed(listing: &Listing) -> Vec<(&str, bool, bool)> {
        listing.entries.iter().map(|e| (e.path.as_str(), e.ignored, e.is_dir)).collect()
    }

    #[test]
    fn a_plain_directory_lists_like_ls_files_would() {
        // No `.git` anywhere: the `.gitignore` still decides, dotfiles list, an ignored
        // directory collapses to one placeholder, an ignored file lists as itself, and an
        // empty ignored directory is skipped as `--no-empty-directory` skips it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, ".gitignore", "target/\n*.log\nempty/\n");
        write(root, ".editorconfig", "root = true\n");
        write(root, "src/main.rs", "fn main() {}\n");
        write(root, "src/nested/.gitignore", "local.txt\n");
        write(root, "src/nested/local.txt", "x\n");
        write(root, "src/nested/kept.rs", "x\n");
        write(root, "target/debug/out", "bin\n");
        write(root, "run.log", "log\n");
        fs::create_dir(root.join("empty")).unwrap();

        let listing = plain_files(root, 1000).unwrap();

        assert!(!listing.capped);
        assert_eq!(
            listed(&listing),
            vec![
                (".editorconfig", false, false),
                (".gitignore", false, false),
                ("run.log", true, false),
                ("src/main.rs", false, false),
                ("src/nested/.gitignore", false, false),
                ("src/nested/kept.rs", false, false),
                ("src/nested/local.txt", true, false),
                ("target", true, true),
            ]
        );
    }

    #[test]
    fn the_walk_matches_ls_files_on_the_same_tree() {
        // Parity with the repo listing: the same untracked tree, listed by the walk and then
        // by `git::all_files` once it is a repo, comes back identical.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, ".gitignore", "build/\n*.tmp\n");
        write(root, ".config/tool.toml", "x\n");
        write(root, "docs/readme.md", "x\n");
        write(root, "docs/draft.tmp", "x\n");
        write(root, "build/a/b.o", "x\n");
        write(root, "lib/sub/.gitignore", "gen/\n");
        write(root, "lib/sub/gen/x.rs", "x\n");
        write(root, "lib/sub/mod.rs", "x\n");
        let walked = plain_files(root, 1000).unwrap().entries;
        // Tracked, as the files of a real checkout are: `ls-files --directory` stops at an
        // untracked directory, so an all-untracked repo would hide the ignored files inside.
        for args in [
            &["init", "-q"][..],
            &["add", "-A"],
            &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "c"],
        ] {
            let ok =
                std::process::Command::new("git").arg("-C").arg(root).args(args).status().unwrap();
            assert!(ok.success(), "git {args:?}");
        }
        assert_eq!(walked, crate::git::all_files(root).unwrap());
    }

    #[test]
    fn a_git_directory_is_never_listed() {
        // A `.git` that is not a repository (git would reject it) is still never shown,
        // neither walked nor as an ignored placeholder.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".git/HEAD", "junk\n");
        write(dir.path(), "a.txt", "a\n");
        let listing = plain_files(dir.path(), 1000).unwrap();
        assert_eq!(listed(&listing), vec![("a.txt", false, false)]);
    }

    #[test]
    fn the_walk_stops_at_the_cap_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..30 {
            write(dir.path(), &format!("f{i:02}.txt"), "x\n");
        }
        let listing = plain_files(dir.path(), 10).unwrap();
        assert!(listing.capped, "the cap is reported");
        assert_eq!(listing.entries.len(), 10);
        assert!(listing.entries.iter().all(|e| !e.ignored), "a capped walk invents no ignores");

        let whole = plain_files(dir.path(), 30).unwrap();
        assert!(!whole.capped, "a listing that fits exactly is whole");
        assert_eq!(whole.entries.len(), 30);
    }

    #[test]
    fn placeholders_spend_the_cap_too() {
        // One ignored directory's siblings are all ignored files: the placeholder pass stops
        // at the budget rather than listing an unbounded directory.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".gitignore", "*.o\n");
        for i in 0..20 {
            write(dir.path(), &format!("f{i:02}.o"), "x\n");
        }
        let listing = plain_files(dir.path(), 5).unwrap();
        assert!(listing.capped);
        assert!(listing.entries.len() <= 5);
        assert_eq!(
            listing.entries[0],
            WorktreeEntry { path: ".gitignore".into(), ignored: false, is_dir: false }
        );
    }
}
