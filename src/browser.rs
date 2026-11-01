//! Open a URL in the browser: the configured `url_opener`, else the platform's default.

use std::process::{Command, Stdio};

use anyhow::{Context, Result};

#[cfg(target_os = "macos")]
const OPENERS: &[&str] = &["open"];
#[cfg(target_os = "linux")]
const OPENERS: &[&str] = &["xdg-open"];
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
const OPENERS: &[&str] = &["open", "xdg-open"];

/// The first platform opener the `present` predicate accepts, in list order.
#[cfg(not(windows))]
fn default_tool(present: impl Fn(&str) -> bool) -> Result<&'static str> {
    OPENERS
        .iter()
        .copied()
        .find(|candidate| present(candidate))
        .context("no link opener: install open or xdg-open, or set `url_opener`")
}

/// Open `url` with the platform's own opener, found on `PATH`.
#[cfg(not(windows))]
fn open_default(url: &str) -> Result<()> {
    let tool = default_tool(crate::proc::on_path)?;
    let mut command = crate::proc::command(tool);
    command.arg(url);
    spawn_detached(tool, command)
}

/// Open `url` through `ShellExecuteW`: no shell parses it.
#[cfg(windows)]
fn open_default(url: &str) -> Result<()> {
    opener::open(url)
        .map_err(|error| anyhow::anyhow!("the default browser could not start: {error}"))
}

/// Open an http(s) `url` through `url_opener`, else the platform default.
pub fn open(url: &str, configured: Option<&str>) -> Result<()> {
    let url = openable_url(url).map_err(anyhow::Error::msg)?;
    let Some(template) = configured else { return open_default(url) };
    let (program, args) = opener_argv(template, url).context("`url_opener` names no program")?;
    let mut command = crate::proc::user_command(&program)
        .with_context(|| format!("`url_opener` not found: {program}"))?;
    command.args(&args);
    spawn_detached(&program, command)
}

/// Start an opener and reap it on a background thread, never waited on.
fn spawn_detached(tool: &str, mut command: Command) -> Result<()> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| anyhow::anyhow!("{tool} could not start: {error}"))?;
    // Reaped off the frame thread, so a finished opener never lingers as a zombie.
    std::thread::spawn(move || drop(child.wait()));
    Ok(())
}

/// The configured opener's argv: `template` split like `editor`, `{url}` placed or appended.
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

/// A link the OS opener may take: http(s) with something after the scheme, and no character the display would hide.
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
    #[cfg(not(windows))]
    use super::default_tool;
    use super::{open, openable_url, opener_argv};

    #[test]
    fn a_configured_opener_that_cannot_start_is_reported_never_replaced() {
        let error = open("https://x.dev", Some("reviewr-no-such-opener {url}")).unwrap_err();
        assert!(error.to_string().contains("reviewr-no-such-opener"), "{error}");
    }

    #[cfg(not(windows))]
    #[test]
    fn the_default_opener_is_the_first_one_on_path_and_its_absence_says_what_to_install() {
        let all = |_: &str| true;
        #[cfg(target_os = "macos")]
        assert_eq!(default_tool(all).unwrap(), "open");
        #[cfg(target_os = "linux")]
        assert_eq!(default_tool(all).unwrap(), "xdg-open");
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        assert_eq!(default_tool(all).unwrap(), "open");
        assert_eq!(
            default_tool(|_| false).unwrap_err().to_string(),
            "no link opener: install open or xdg-open, or set `url_opener`"
        );
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
