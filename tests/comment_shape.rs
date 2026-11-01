//! Comments stay short: a run of comment lines is two at most, in every tracked code, script,
//! and config file.

use std::fs;
use std::path::Path;
use std::process::Command;

#[test]
fn no_comment_runs_past_two_lines() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let listed = Command::new("git").arg("-C").arg(root).args(["ls-files", "-z"]).output();
    let listed = listed.expect("git ls-files runs");
    // A git that failed lists nothing, which must fail here, never pass unchecked.
    assert!(listed.status.success() && !listed.stdout.is_empty(), "git ls-files listed nothing");
    let listed = listed.stdout;
    let mut long = Vec::new();
    for file in String::from_utf8_lossy(&listed).split('\0').filter(|f| !f.is_empty()) {
        if let Some(mark) = comment_mark(Path::new(file)) {
            check(&root.join(file), mark, &mut long);
        }
    }
    assert!(long.is_empty(), "comment runs past two lines, rewrite them:\n{}", long.join("\n"));
}

/// The line-comment marker of a file kind this rule covers, by extension or by name.
fn comment_mark(path: &Path) -> Option<&'static str> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    match ext {
        "rs" => Some("//"),
        "sh" | "ps1" | "yml" | "yaml" | "toml" | "py" | "tape" => Some("#"),
        _ if ["justfile", "vm", ".gitattributes", ".gitignore"].contains(&name) => Some("#"),
        _ => None,
    }
}

/// Every third consecutive line starting with `mark`; a `#!` shebang is no comment.
fn check(path: &Path, mark: &str, long: &mut Vec<String>) {
    let Ok(text) = fs::read_to_string(path) else { return };
    let mut run = 0;
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        run = if trimmed.starts_with(mark) && !trimmed.starts_with("#!") { run + 1 } else { 0 };
        if run == 3 {
            long.push(format!("{}:{}", path.display(), i - 1));
        }
    }
}
