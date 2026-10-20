use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

const ITERM_SCRIPT: &[&str] = &[
    "on run argv",
    "tell application \"iTerm\"",
    "activate",
    "if (count of windows) = 0 then",
    "create window with default profile",
    "else",
    "tell current window to create tab with default profile",
    "end if",
    "tell current session of current window to write text (item 1 of argv)",
    "end tell",
    "end run",
];

/// Opens `cmd` in a new iTerm tab, or copies it to the clipboard elsewhere.
/// Returns the user-facing outcome.
pub fn open(cmd: &str) -> Result<String> {
    if std::env::var("TERM_PROGRAM").as_deref() == Ok("iTerm.app") {
        let mut c = Command::new("osascript");
        for line in ITERM_SCRIPT {
            c.args(["-e", line]);
        }
        let out = c.arg(cmd).output().context("failed to run osascript")?;
        if !out.status.success() {
            bail!(
                "osascript: {}",
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
            );
        }
        return Ok("session opened in a new iTerm tab".into());
    }
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .context("failed to run pbcopy")?;
    child
        .stdin
        .take()
        .context("pbcopy stdin")?
        .write_all(cmd.as_bytes())?;
    child.wait()?;
    Ok("session command copied to clipboard".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_single_quotes_for_posix_shells() {
        assert_eq!(shell_quote("/a b/it's"), r"'/a b/it'\''s'");
        let out = Command::new("/bin/sh")
            .args(["-c", &format!("printf %s {}", shell_quote("x'y z"))])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "x'y z");
    }
}
