//! Formatting comments and exporting them to the agent or clipboard.
//!
//! A comment becomes a block of `location` (with its quoted span, when it
//! has one), the diff snippet, then the text. Export is consume-on-success: the caller removes
//! a comment only after `export` returns `Ok`.

use std::io::Write;
use std::process::Stdio;

use anyhow::{Context, Result, bail};

use crate::herdr;
use crate::model::Comment;

/// One comment as its export block: location (and quote), snippet, then text. The quote is
/// flattened to one line, so it can never introduce the blank-line block separator.
pub fn format_comment(comment: &Comment) -> String {
    format!("{}\n{}\n{}", comment.heading(), comment.lines, normalize_text(&comment.text))
}

/// One comment as the quote an immediate delivery drops into the agent's input box, the shape
/// a reply quote takes in a chat composer: the quoted span on one `> ` line after its location
/// (`> path:3「text」`), or, for a comment on whole lines, the location and then each snippet
/// line behind its own `> `, markers kept. The comment text follows on the next line, its
/// line breaks kept.
pub fn format_quote(comment: &Comment) -> String {
    let quote = match comment.quote_label() {
        Some(label) => format!("> {}「{label}」", comment.location()),
        None => std::iter::once(comment.location())
            .chain(comment.lines.lines().map(str::to_string))
            .map(|line| format!("> {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    };
    format!("{quote}\n{}", normalize_text(&comment.text))
}

/// Comment text for export: drop `\r`, trim trailing space per line, and drop blank
/// lines so a multi-line comment can never introduce the blank-line block separator.
fn normalize_text(text: &str) -> String {
    text.replace('\r', "")
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Many comments, sorted by file then start line, one blank line between blocks.
pub fn format_all(comments: &[&Comment]) -> String {
    let mut sorted = comments.to_vec();
    sorted.sort_by(|a, b| a.file.cmp(&b.file).then(a.start.cmp(&b.start)));
    sorted.iter().map(|c| format_comment(c)).collect::<Vec<_>>().join("\n\n")
}

/// A destination comments can be exported to. Export succeeds or errors as a whole.
pub trait ExportTarget {
    fn export(&self, text: &str) -> Result<()>;
    fn label(&self) -> &'static str;
    /// Destination-specific confirmation shown after a successful export.
    fn success_message(&self, count: usize) -> String;
    /// Destination-specific line shown after a failed one. It is the whole status, so it is one
    /// short sentence a reviewer can read, never the underlying error. The cause goes to the log.
    fn failure_message(&self) -> String;
}

fn counted_comments(count: usize) -> String {
    let noun = if count == 1 { "comment" } else { "comments" };
    format!("{count} {noun}")
}

/// A clipboard tool and the args that make it read stdin into the system clipboard. Tried in
/// order — the first one present on `PATH` wins. macOS ships `pbcopy`; Linux needs one of these
/// installed (Wayland `wl-copy`, or X11 `xclip`/`xsel`). OSC 52 and Windows are roadmap.
const CLIPBOARD_TOOLS: &[(&str, &[&str])] = &[
    ("pbcopy", &[]),
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
];

/// The system clipboard, via the first available platform clipboard tool.
#[derive(Debug)]
pub struct Clipboard;

impl ExportTarget for Clipboard {
    fn label(&self) -> &'static str {
        "clipboard"
    }

    fn success_message(&self, count: usize) -> String {
        format!("copied {}", counted_comments(count))
    }

    fn failure_message(&self) -> String {
        "clipboard failed".to_string()
    }

    fn export(&self, text: &str) -> Result<()> {
        let (cmd, args) = select_tool(CLIPBOARD_TOOLS, crate::proc::on_path).context(
            "no clipboard tool found (install wl-clipboard, xclip, or xsel) — \
             use Send instead",
        )?;
        let mut child = crate::proc::command(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning {cmd}"))?;
        child
            .stdin
            .as_mut()
            .with_context(|| format!("{cmd} stdin unavailable"))?
            .write_all(text.as_bytes())
            .with_context(|| format!("writing to {cmd}"))?;
        if !child.wait().with_context(|| format!("waiting for {cmd}"))?.success() {
            bail!("{cmd} exited non-zero");
        }
        Ok(())
    }
}

/// The first clipboard tool the `present` predicate accepts, preserving list order.
fn select_tool(
    tools: &'static [(&'static str, &'static [&'static str])],
    present: impl Fn(&str) -> bool,
) -> Option<(&'static str, &'static [&'static str])> {
    tools.iter().copied().find(|(cmd, _)| present(cmd))
}

/// One chosen agent pane: fill its input via `herdr pane send-text`, then focus it.
///
/// The pane is decided before the export runs, by the sole-agent path or by the picker, and
/// nothing re-resolves it here. A pane that closed in between fails the send and keeps every
/// comment.
#[derive(Clone, Debug)]
pub struct Agent {
    pub pane: String,
    pub name: String,
}

impl ExportTarget for Agent {
    fn label(&self) -> &'static str {
        "agent"
    }

    /// Names the agent it addressed. The send is irreversible and consumes the whole set, so
    /// this line is the reviewer's only record of where the review went.
    fn success_message(&self, count: usize) -> String {
        format!("added {} to {}", counted_comments(count), self.name)
    }

    /// The pane was resolved before the send and closed in between, which is the only way this
    /// happens in practice. herdr's own wording is a JSON envelope around a pane id, so the
    /// reviewer gets this instead and the payload goes to the log.
    fn failure_message(&self) -> String {
        "agent not found".to_string()
    }

    fn export(&self, text: &str) -> Result<()> {
        herdr::send_text(&self.pane, text)?;
        // Focus is a convenience once the text is delivered; a focus failure must NOT fail the
        // export, or the comments stay unconsumed and the next Send duplicates the whole review.
        let _ = herdr::focus(&self.pane);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Agent, CLIPBOARD_TOOLS, Clipboard, ExportTarget, format_all, format_comment, format_quote,
        select_tool,
    };
    use crate::model::{Comment, Side};

    #[test]
    fn clipboard_tool_selection_prefers_list_order_and_can_be_empty() {
        // None present -> no tool (the caller surfaces the "install one" error).
        assert!(select_tool(CLIPBOARD_TOOLS, |_| false).is_none());
        // Only an X11 tool present -> it's chosen, with its selection args.
        assert_eq!(
            select_tool(CLIPBOARD_TOOLS, |c| c == "xclip"),
            Some(("xclip", &["-selection", "clipboard"][..]))
        );
        // When several are present, earlier in the list wins (pbcopy over xclip).
        assert_eq!(
            select_tool(CLIPBOARD_TOOLS, |c| c == "pbcopy" || c == "xclip").map(|(cmd, _)| cmd),
            Some("pbcopy")
        );
    }

    #[test]
    fn export_confirmations_name_the_actual_result_and_pluralize_comments() {
        // The agent line names the pane it addressed, so a mis-send is visible the moment it
        // lands.
        let agent = Agent { pane: "w8:p1".into(), name: "release-bot".into() };
        assert_eq!(agent.success_message(1), "added 1 comment to release-bot");
        assert_eq!(agent.success_message(2), "added 2 comments to release-bot");
        assert_eq!(Clipboard.success_message(1), "copied 1 comment");
        assert_eq!(Clipboard.success_message(2), "copied 2 comments");
    }

    fn comment(file: &str, side: Side, start: u32, end: u32, lines: &str, text: &str) -> Comment {
        Comment {
            file: file.into(),
            side,
            start,
            end,
            lines: lines.into(),
            text: text.into(),
            diff_anchored: true,
            rev: crate::model::Rev::Worktree,
            quote: None,
        }
    }

    fn quoted(c: Comment, text: &str) -> Comment {
        let quote = crate::model::Quote { text: text.into(), start_col: 0, end_col: 1 };
        Comment { quote: Some(quote), ..c }
    }

    #[test]
    fn block_is_location_snippet_text() {
        let c = comment(
            "extruct/core/llm_registry.py",
            Side::New,
            40,
            41,
            "-from .z import w\n+from .x import y",
            "this import path looks wrong",
        );
        assert_eq!(
            format_comment(&c),
            "extruct/core/llm_registry.py:40-41\n-from .z import w\n+from .x import y\nthis import path looks wrong"
        );
    }

    #[test]
    fn a_quoted_comment_names_its_span_after_the_location() {
        let c = quoted(comment("plan.md", Side::New, 3, 3, " 缓存层设计", "why here?"), "缓存层");
        assert_eq!(format_comment(&c), "plan.md:3 · 「缓存层」\n 缓存层设计\nwhy here?");
        // A multi-line span flattens to one line, blank lines included, and a long one is cut.
        let c = quoted(comment("a.rs", Side::New, 1, 3, "+a\n+\n+b", "t"), "alpha(\n\n    beta)");
        assert_eq!(format_comment(&c), "a.rs:1-3 · 「alpha( beta)」\n+a\n+\n+b\nt");
        let c = quoted(comment("a.rs", Side::Old, 9, 9, "-x", "t"), &"y".repeat(90));
        let head = format_comment(&c).lines().next().unwrap().to_string();
        assert_eq!(head, format!("a.rs:9 (removed) · 「{}…」", "y".repeat(79)));
    }

    #[test]
    fn a_quoted_comment_delivers_as_one_quote_line_then_its_text() {
        let c = quoted(comment("plan.md", Side::New, 3, 3, " 缓存层设计", "why here?"), "缓存层");
        assert_eq!(format_quote(&c), "> plan.md:3「缓存层」\nwhy here?");
        // The quote flattens and cuts exactly like the heading, and the text keeps its breaks.
        let c = quoted(comment("a.rs", Side::Old, 1, 3, "-a\n-b", "one\n\n two\n"), "a(\n\n  b)");
        assert_eq!(format_quote(&c), "> a.rs:1-3 (removed)「a( b)」\none\n two");
        let c = quoted(comment("a.rs", Side::New, 9, 9, "+x", "t"), &"y".repeat(90));
        let head = format_quote(&c).lines().next().unwrap().to_string();
        assert_eq!(head, format!("> a.rs:9「{}…」", "y".repeat(79)));
    }

    #[test]
    fn a_line_comment_delivers_its_snippet_behind_quote_markers() {
        let c = comment("a.rs", Side::New, 40, 41, "-from .z import w\n+from .x import y", "wrong");
        assert_eq!(
            format_quote(&c),
            "> a.rs:40-41\n> -from .z import w\n> +from .x import y\nwrong"
        );
        // A context line keeps its space marker.
        let c = comment("a.rs", Side::New, 7, 7, "     keep()", "why?");
        assert_eq!(format_quote(&c), "> a.rs:7\n>      keep()\nwhy?");
    }

    #[test]
    fn removed_side_marks_the_header() {
        let c = comment("a.rs", Side::Old, 38, 38, "-    cleanup()", "still needed");
        assert_eq!(format_comment(&c), "a.rs:38 (removed)\n-    cleanup()\nstill needed");
    }

    #[test]
    fn multiline_text_keeps_breaks_but_drops_blank_lines() {
        let c = comment("a.rs", Side::New, 1, 1, "+x", "first line\n\n  \nsecond line\n");
        assert_eq!(format_comment(&c), "a.rs:1\n+x\nfirst line\nsecond line");
    }

    #[test]
    fn all_sorts_by_file_then_start_with_blank_separator() {
        let b = comment("b.rs", Side::New, 5, 5, "+x", "two");
        let a2 = comment("a.rs", Side::New, 20, 20, "+y", "later");
        let a1 = comment("a.rs", Side::New, 3, 3, "+z", "earlier");
        let out = format_all(&[&b, &a2, &a1]);
        assert_eq!(out, "a.rs:3\n+z\nearlier\n\na.rs:20\n+y\nlater\n\nb.rs:5\n+x\ntwo");
    }
}
