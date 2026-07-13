use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use super::{InventoryItem, ScanEvent, Source};

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

const REGISTRY: &[(&str, &str, &[&str], Source)] = &[
    ("Claude Code", "claude", &["--version"], Source::Tool),
    ("Codex CLI", "codex", &["--version"], Source::Tool),
    ("OpenCode", "opencode", &["--version"], Source::Tool),
    ("Hermes Agent", "hermes", &["--version"], Source::Tool),
    ("Isonapse", "isonapse", &["--version"], Source::Tool),
    ("GitHub CLI", "gh", &["--version"], Source::Tool),
    ("Docker", "docker", &["--version"], Source::Tool),
    ("Tailscale", "tailscale", &["version"], Source::Tool),
    ("Ollama", "ollama", &["--version"], Source::Tool),
    ("rustc", "rustc", &["--version"], Source::Language),
    ("cargo", "cargo", &["--version"], Source::Language),
    ("Python", "python3", &["--version"], Source::Language),
    ("Go", "go", &["version"], Source::Language),
    ("Node.js", "node", &["--version"], Source::Language),
    ("npm", "npm", &["--version"], Source::Language),
    ("Java", "java", &["-version"], Source::Language),
    ("javac", "javac", &["-version"], Source::Language),
    ("Swift", "swift", &["--version"], Source::Language),
    ("Clang", "clang", &["--version"], Source::Language),
    ("Git", "git", &["--version"], Source::Language),
];

pub fn scan(tx: &Sender<ScanEvent>) {
    // ponytail: one thread per present binary (~20 max, mostly blocked on the
    // child); a bounded pool if the registry ever grows into the hundreds.
    std::thread::scope(|s| {
        for &(name, bin, args, source) in REGISTRY {
            // Absent on PATH is not an error: emit nothing.
            let Some(path) = find_on_path(bin) else {
                continue;
            };
            let tx = tx.clone();
            s.spawn(move || {
                let version = probe(&path, args);
                let _ = tx.send(ScanEvent::Item(InventoryItem {
                    name: name.to_string(),
                    version,
                    source,
                    path: Some(path),
                }));
            });
        }
    });
}

fn find_on_path(bin: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|c| {
        c.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

/// Runs the binary with its version args under a hard timeout. Both streams
/// are captured (java prints its version to stderr). Any failure or timeout
/// yields None — the item is still emitted, just without a version.
fn probe(path: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new(path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            // ponytail: 25ms try_wait polling; version output is tiny so the
            // pipe buffer can't fill and block the child before it exits.
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    let streams = [out.stdout, out.stderr].map(|b| String::from_utf8_lossy(&b).into_owned());
    streams
        .iter()
        .flat_map(|s| s.lines())
        .find(|l| !l.trim().is_empty())
        .and_then(extract_version)
}

/// First run of `[0-9.]` starting with a digit that contains a dot, trailing
/// dots trimmed: "rustc 1.79.0 (…)" -> "1.79.0", "v20.11.1" -> "20.11.1".
fn extract_version(line: &str) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            let token = token.trim_end_matches('.');
            if token.contains('.') {
                return Some(token.to_string());
            }
        } else {
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_versions_from_real_outputs() {
        assert_eq!(
            extract_version("rustc 1.79.0 (129f3b996 2024-06-10)"),
            Some("1.79.0".to_string())
        );
        assert_eq!(
            extract_version("openjdk version \"21.0.2\" 2024-01-16"),
            Some("21.0.2".to_string())
        );
        assert_eq!(
            extract_version("Python 3.12.4"),
            Some("3.12.4".to_string())
        );
        assert_eq!(extract_version("v20.11.1"), Some("20.11.1".to_string()));
        assert_eq!(
            extract_version("go version go1.22.4 darwin/arm64"),
            Some("1.22.4".to_string())
        );
        assert_eq!(
            extract_version("Docker version 27.0.3, build 7d4bcd8"),
            Some("27.0.3".to_string())
        );
    }

    #[test]
    fn no_version_when_none_present() {
        assert_eq!(extract_version("built on 2024-01-16"), None);
        assert_eq!(extract_version("no digits here"), None);
        assert_eq!(extract_version(""), None);
    }
}
