//! Open a URL in the user's browser — the `PR` tab's only outward action.
//!
//! A configured opener wins; otherwise the host platform's default is used.

use std::process::Stdio;

use anyhow::{Context, Result};

#[cfg(target_os = "macos")]
const OPENERS: &[&str] = &["open"];
#[cfg(target_os = "linux")]
const OPENERS: &[&str] = &["xdg-open"];
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const OPENERS: &[&str] = &["open", "xdg-open"];

/// Open `url` through the configured `url_opener`, else the first platform opener on `PATH`.
/// The opener runs detached from the frame: it is started and reaped on a background thread,
/// never waited on, so a command that lingers (a browser launched in the foreground, a bridge
/// to an unreachable host) can never freeze the pane. A command that cannot start is reported;
/// what it does once running is its own.
pub fn open(url: &str, configured: Option<&str>) -> Result<()> {
    let (tool, args, mut command) = if let Some(template) = configured {
        let (program, args) =
            opener_argv(template, url).context("the `url_opener` setting names no program")?;
        let command = crate::proc::user_command(&program)
            .with_context(|| format!("URL opener {program:?} was not found"))?;
        (program, args, command)
    } else {
        let tool = OPENERS
            .iter()
            .copied()
            .find(|candidate| crate::proc::on_path(candidate))
            .context("no URL opener found: install `open`/`xdg-open`, or set `url_opener`")?;
        (tool.to_string(), vec![url.to_string()], crate::proc::command(tool))
    };
    let mut child = command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| anyhow::anyhow!("URL opener {tool:?} could not start: {error}"))?;
    // Reaped off the frame thread, so a finished opener never lingers as a zombie.
    std::thread::spawn(move || drop(child.wait()));
    Ok(())
}

/// The configured opener's program and arguments: `template` split the way the `editor` key
/// is, `{url}` substituted per word — so a URL never splits — and appended when absent.
fn opener_argv(template: &str, url: &str) -> Option<(String, Vec<String>)> {
    let names_url = template.contains("{url}");
    let mut words =
        crate::editor::split_command(template).into_iter().map(|w| w.replace("{url}", url));
    let program = words.next().filter(|p| !p.is_empty())?;
    let mut args: Vec<String> = words.collect();
    if !names_url {
        args.push(url.to_string());
    }
    Some((program, args))
}

/// Gate a markdown link destination before it reaches the OS opener
/// : trimmed, case-insensitive `http://`/`https://` with something
/// after the scheme, and no control or bidirectional-override character anywhere — a
/// destination the display would sanitize must never open as different bytes.
pub fn openable_url(url: &str) -> Result<&str, &'static str> {
    let trimmed = url.trim();
    let hostile = trimmed.chars().any(crate::markdown::hostile_char);
    let b = trimmed.as_bytes();
    let schemed = (b.len() > 7 && b[..7].eq_ignore_ascii_case(b"http://"))
        || (b.len() > 8 && b[..8].eq_ignore_ascii_case(b"https://"));
    if !hostile && schemed { Ok(trimmed) } else { Err("unsupported link scheme") }
}

#[cfg(test)]
mod tests {
    use super::{open, openable_url, opener_argv};

    #[test]
    fn a_configured_opener_that_cannot_start_is_reported_never_replaced() {
        let error = open("https://x.dev", Some("reviewr-no-such-opener {url}")).unwrap_err();
        assert!(error.to_string().contains("reviewr-no-such-opener"), "{error}");
    }

    #[test]
    fn the_opener_template_splits_like_editor_and_places_the_url() {
        let url = "https://example.com/pr?a=1&b=2";
        let argv = |t: &str| opener_argv(t, url).map(|(p, a)| (p, a.join("|")));
        assert_eq!(argv("remote-open"), Some(("remote-open".into(), url.into())), "appended");
        assert_eq!(
            argv("ssh laptop 'open -g' {url}"),
            Some(("ssh".into(), format!("laptop|open -g|{url}"))),
            "quoted words stay whole",
        );
        assert_eq!(
            argv("bridge --url={url} --new"),
            Some(("bridge".into(), format!("--url={url}|--new"))),
            "placed where named, never appended twice",
        );
        assert_eq!(argv("   "), None, "no program");
        // A URL is one word whatever it holds, and is substituted once.
        let odd = "https://x/a b'c{url}";
        assert_eq!(opener_argv("bridge {url}", odd).map(|(_, a)| a), Some(vec![odd.to_string()]));
    }

    #[test]
    fn the_url_guard_admits_http_and_https_case_insensitively() {
        assert_eq!(openable_url("https://ci.example/1"), Ok("https://ci.example/1"));
        assert_eq!(openable_url("HTTP://ci.example"), Ok("HTTP://ci.example"));
        assert_eq!(openable_url("  https://x.dev  "), Ok("https://x.dev"), "trimmed");
    }

    #[test]
    fn the_url_guard_rejects_other_schemes_and_hostile_bytes() {
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https:evil", // scheme without authority
            "https://",   // nothing after the scheme
            "ftp://host",
            "https://a\u{202e}b",   // bidi override
            "https://a\u{1b}[31mb", // control character
            "",
        ] {
            assert!(openable_url(bad).is_err(), "{bad:?} must not open");
        }
    }
}
