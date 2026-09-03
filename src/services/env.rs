//! Making the app's `PATH` look like your terminal's.
//!
//! An app launched from Finder, the Dock or a `.dmg` does not inherit the
//! shell environment. It gets roughly `/usr/bin:/bin:/usr/sbin:/sbin`, which
//! is why `az` reports as "not found on PATH" in the packaged build and works
//! perfectly under `cargo run` — the terminal passed its own `PATH` down.
//!
//! Homebrew installs `az` into `/opt/homebrew/bin`, which a bundle never sees.
//! So at startup we ask the login shell what it thinks `PATH` should be and
//! adopt it, falling back to the usual locations when that fails.
//!
//! A *non-interactive* login shell is used deliberately: it reads the profile
//! where `brew shellenv` lives, without the risk that an interactive shell
//! blocks on something and hangs startup before a window ever appears.
//!
//! Windows has the same symptom from a different cause. There is no login
//! shell to ask, but Explorer hands a GUI app the environment it held at
//! sign-in: install the Azure CLI and every *newly opened* cmd.exe finds `az`
//! while this app still does not, until the user signs out and back in. So
//! the CLI's own install directories are added as fallbacks there.
//!
//! Note that `PATH` is not a `:`-separated string everywhere — Windows uses
//! `;`, and its entries contain a drive-letter colon. Everything below goes
//! through `std::env::split_paths`/`join_paths` for that reason.

use std::collections::HashSet;
use std::path::PathBuf;

/// Directories worth having even if the environment tells us nothing.
#[cfg(not(windows))]
fn fallback_dirs() -> Vec<String> {
    const FALLBACKS: &[&str] = &[
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ];
    FALLBACKS.iter().map(|s| s.to_string()).collect()
}

/// Where the Azure CLI's two Windows installers put `az.cmd`.
#[cfg(windows)]
fn fallback_dirs() -> Vec<String> {
    const CANDIDATES: &[(&str, &str)] = &[
        ("ProgramFiles", r"Microsoft SDKs\Azure\CLI2\wbin"),
        ("ProgramFiles(x86)", r"Microsoft SDKs\Azure\CLI2\wbin"),
        ("LOCALAPPDATA", r"Programs\Azure CLI\wbin"),
    ];
    CANDIDATES
        .iter()
        .filter_map(|(var, suffix)| {
            let base = std::env::var_os(var)?;
            Some(
                std::path::Path::new(&base)
                    .join(suffix)
                    .to_string_lossy()
                    .into_owned(),
            )
        })
        .collect()
}

/// Adopts the login shell's `PATH`, merged with whatever we already have.
/// Call once, before anything runs a subprocess.
pub fn adopt_login_path() {
    let current = std::env::var("PATH").unwrap_or_default();
    let login = login_shell_path();
    let cargo_bin = dirs::home_dir().map(|h| h.join(".cargo/bin").to_string_lossy().to_string());

    let mut extras = fallback_dirs();
    if let Some(bin) = cargo_bin {
        extras.push(bin);
    }

    let merged = merge_paths(&current, login.as_deref(), &extras);
    // SAFETY: called first thing in main, before any other threads start.
    unsafe {
        std::env::set_var("PATH", merged);
    }
}

/// Login shell first (it is the informed answer), then what we already had,
/// then the fallbacks. Order is preserved and duplicates are dropped, so the
/// first place a tool is found stays the one that wins.
///
/// `extras` is a list of single directories, never a pre-joined `PATH`.
pub fn merge_paths(current: &str, login: Option<&str>, extras: &[String]) -> String {
    let mut candidates: Vec<PathBuf> = vec![];
    for source in [login.unwrap_or_default(), current] {
        candidates.extend(std::env::split_paths(source));
    }
    candidates.extend(extras.iter().map(PathBuf::from));

    let mut seen = HashSet::new();
    let out: Vec<PathBuf> = candidates
        .into_iter()
        .filter_map(tidy)
        .filter(|dir| seen.insert(dir.clone()))
        .collect();

    // `join_paths` only refuses when a directory contains the separator
    // itself. Leaving PATH untouched beats replacing it with a mangled one.
    std::env::join_paths(&out)
        .map(|joined| joined.to_string_lossy().into_owned())
        .unwrap_or_else(|_| current.to_string())
}

/// Drops blank and whitespace-only entries, which a ragged `PATH` is full of
/// and which resolve to the current directory rather than to nothing.
fn tidy(dir: PathBuf) -> Option<PathBuf> {
    let text = dir.to_string_lossy();
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

#[cfg(unix)]
fn login_shell_path() -> Option<String> {
    use std::process::{Command, Stdio};

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let output = Command::new(&shell)
        // -l reads the profile (where `brew shellenv` normally is); no -i, so
        // an interactive prompt can never block startup.
        .args(["-lc", "printf '%s' \"$PATH\""])
        .stdin(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(not(unix))]
fn login_shell_path() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extras() -> Vec<String> {
        vec!["/opt/homebrew/bin".to_string(), "/usr/bin".to_string()]
    }

    #[test]
    fn a_bundles_bare_path_gains_the_places_tools_actually_live() {
        // The exact failure: az is in /opt/homebrew/bin, the bundle sees neither.
        let merged = merge_paths("/usr/bin:/bin", None, &extras());
        assert!(merged.split(':').any(|p| p == "/opt/homebrew/bin"));
    }

    #[test]
    fn the_login_shell_is_believed_before_anything_else() {
        let merged = merge_paths("/usr/bin", Some("/opt/homebrew/bin:/usr/bin"), &extras());
        assert!(merged.starts_with("/opt/homebrew/bin"));
    }

    #[test]
    fn a_directory_never_appears_twice() {
        let merged = merge_paths(
            "/usr/bin:/bin",
            Some("/usr/bin:/opt/homebrew/bin"),
            &extras(),
        );
        let count = merged.split(':').filter(|p| *p == "/usr/bin").count();
        assert_eq!(count, 1, "got {merged}");
    }

    #[test]
    fn order_decides_which_copy_of_a_tool_wins() {
        let merged = merge_paths("/usr/bin", Some("/opt/homebrew/bin:/usr/bin"), &extras());
        let entries: Vec<&str> = merged.split(':').collect();
        let brew = entries
            .iter()
            .position(|p| *p == "/opt/homebrew/bin")
            .unwrap();
        let usr = entries.iter().position(|p| *p == "/usr/bin").unwrap();
        assert!(brew < usr);
    }

    #[test]
    fn empty_and_ragged_input_does_not_produce_empty_entries() {
        let merged = merge_paths("::/usr/bin: :", Some(""), &extras());
        assert!(!merged.split(':').any(|p| p.trim().is_empty()));
    }

    #[test]
    fn nothing_at_all_still_yields_a_usable_path() {
        let merged = merge_paths("", None, &extras());
        assert!(merged.split(':').any(|p| p == "/usr/bin"));
    }

    /// `extras` entries are whole directories. The old code joined them into
    /// one `:`-separated string and re-split it, which on Windows tore
    /// `C:\\Users\\...\\.cargo\\bin` in half at the drive letter.
    #[test]
    fn an_extra_entry_is_treated_as_one_whole_directory() {
        let dir = if cfg!(windows) {
            r"C:\Users\mb\.cargo\bin"
        } else {
            "/Users/mb/.cargo/bin"
        };

        let merged = merge_paths("", None, &[dir.to_string()]);

        let out: Vec<String> = std::env::split_paths(&merged)
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(out, vec![dir.to_string()]);
    }

    /// Windows-only because there is nothing to assert elsewhere: on Unix a
    /// directory containing `:` genuinely cannot live in a `PATH`.
    ///
    /// The bug this guards: merging with a hardcoded `:` split a Windows
    /// `PATH` on its drive-letter colons and rejoined it with the wrong
    /// separator, gluing the last real directory to a list of `/usr/bin`-style
    /// fallbacks. Last is exactly where an MSI appends itself — the Azure CLI
    /// included, which is how `az` went missing for a GUI app on a machine
    /// where every terminal finds it.
    ///
    /// Note that CI only runs the test suite on Linux, so this one is
    /// currently exercised by `cargo test` on a Windows box and nowhere else.
    #[cfg(windows)]
    #[test]
    fn a_windows_path_survives_the_merge_intact() {
        let wbin = r"C:\Program Files\Microsoft SDKs\Azure\CLI2\wbin";
        let current = format!(r"C:\Windows\system32;{wbin}");

        let merged = merge_paths(&current, None, &extras());

        let out: Vec<String> = std::env::split_paths(&merged)
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert!(
            out.iter().any(|dir| dir == wbin),
            "the Azure CLI directory must come out whole, got {merged}"
        );
    }

    /// Not an assertion about this machine — just proof the probe returns
    /// something shaped like a PATH when a shell is available.
    #[test]
    fn the_login_shell_probe_returns_a_path_or_nothing() {
        if let Some(path) = login_shell_path() {
            assert!(path.contains('/'), "got {path}");
        }
    }
}
