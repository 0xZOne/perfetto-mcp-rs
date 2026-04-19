// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

//! `install` / `uninstall` subcommand logic.
//!
//! Binary self-registers with Claude Code and Codex; cleans its own
//! `dirs::data_local_dir()` cache. Shell wrappers (install.sh / install.ps1)
//! keep only distribution + platform glue.
//!
//! Uses sync `std::process::Command`; the parent is `#[tokio::main]` but
//! install/uninstall don't need async — they're one-shot CLI paths.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};

const SERVER_NAME: &str = "perfetto-mcp-rs";

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeScope {
    User,
    Local,
    Project,
}

impl ClaudeScope {
    fn as_str(self) -> &'static str {
        match self {
            ClaudeScope::User => "user",
            ClaudeScope::Local => "local",
            ClaudeScope::Project => "project",
        }
    }
}

impl std::fmt::Display for ClaudeScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(clap::Args, Debug)]
pub struct InstallArgs {
    /// Path to register with Claude/Codex. Shell wrappers MUST pass this
    /// with `${INSTALL_DIR}/${bin_file}` — on Linux, `current_exe()` reads
    /// `/proc/self/exe` which is already the symlink target, and we want
    /// to preserve the `$INSTALL_DIR` path users see.
    #[arg(long)]
    pub binary_path: Option<PathBuf>,

    /// Claude scope (user/local/project). Ignored by Codex (no scope concept).
    /// For `--scope local` / `project`, run from the target project directory.
    #[arg(long, value_enum, default_value_t = ClaudeScope::User)]
    pub scope: ClaudeScope,

    #[arg(long)]
    pub skip_claude: bool,

    #[arg(long)]
    pub skip_codex: bool,
}

#[derive(clap::Args, Debug)]
pub struct UninstallArgs {
    /// Must match the scope used at install. For `--scope local` / `project`,
    /// this command MUST run from the original project directory; `local`
    /// is keyed by CWD inside `~/.claude.json`, `project` lives in that
    /// directory's `.mcp.json`.
    #[arg(long, value_enum, default_value_t = ClaudeScope::User)]
    pub scope: ClaudeScope,

    #[arg(long)]
    pub keep_cache: bool,

    #[arg(long)]
    pub skip_claude: bool,

    #[arg(long)]
    pub skip_codex: bool,
}

enum Outcome {
    Done(String),
    Skipped(String),
    Failed(String),
}

pub fn run_install(args: InstallArgs) -> Result<()> {
    // `std::path::absolute` makes the path absolute lexically (no symlink
    // resolution — matches our "preserve the `$INSTALL_DIR/<bin>` path the
    // user sees" invariant). Needed because MCP clients later spawn from
    // their own CWD, so a relative `./perfetto-mcp-rs` would silently break.
    //
    // Windows POSIX paths (`/c/Users/...`) from Git Bash / MSYS are NOT
    // handled here: Rust's Windows path parser doesn't understand them and
    // we can't reach `cygpath` from the binary. install.sh / install.ps1
    // are responsible for `cygpath -m` before passing `--binary-path`.
    let bin = match args.binary_path {
        Some(p) => std::path::absolute(&p)
            .with_context(|| format!("failed to make --binary-path absolute: {}", p.display()))?,
        // `current_exe()` already returns an absolute path per its docs.
        None => std::env::current_exe()
            .context("cannot determine current executable path (and --binary-path not provided)")?,
    };
    println!(
        "==> Installing {SERVER_NAME} (binary={}, scope={})",
        bin.display(),
        args.scope
    );

    let outcomes = vec![
        (
            "Claude",
            if args.skip_claude {
                Outcome::Skipped("--skip-claude".into())
            } else {
                register_claude(&bin, args.scope)
            },
        ),
        (
            "Codex",
            if args.skip_codex {
                Outcome::Skipped("--skip-codex".into())
            } else {
                register_codex(&bin)
            },
        ),
    ];
    aggregate(outcomes)
}

pub fn run_uninstall(args: UninstallArgs) -> Result<()> {
    println!("==> Uninstalling {SERVER_NAME} (scope={})", args.scope);

    let outcomes = vec![
        (
            "Claude",
            if args.skip_claude {
                Outcome::Skipped("--skip-claude".into())
            } else {
                deregister_claude(args.scope)
            },
        ),
        (
            "Codex",
            if args.skip_codex {
                Outcome::Skipped("--skip-codex".into())
            } else {
                deregister_codex()
            },
        ),
        (
            "Cache",
            if args.keep_cache {
                Outcome::Skipped("--keep-cache".into())
            } else {
                clean_cache()
            },
        ),
    ];
    aggregate(outcomes)
}

fn aggregate(outcomes: Vec<(&str, Outcome)>) -> Result<()> {
    let mut failure_msgs = Vec::new();
    for (label, outcome) in &outcomes {
        match outcome {
            Outcome::Done(msg) => println!("==> {label}: {msg}"),
            Outcome::Skipped(msg) => println!("==> {label} skipped: {msg}"),
            Outcome::Failed(msg) => {
                eprintln!("warning: {label} failed: {msg}");
                failure_msgs.push(format!("{label}: {msg}"));
            }
        }
    }
    if failure_msgs.is_empty() {
        println!("==> Done.");
        Ok(())
    } else {
        Err(anyhow!(
            "one or more steps failed:\n  {}",
            failure_msgs.join("\n  ")
        ))
    }
}

fn register_claude(bin: &Path, scope: ClaudeScope) -> Outcome {
    if which::which("claude").is_err() {
        return Outcome::Skipped("claude not found, skipping".into());
    }

    let bin_str = bin.to_string_lossy().to_string();
    let scope_str = scope.as_str();

    // Idempotent remove-then-add. We used to pre-probe with `claude mcp
    // list` to decide whether to call `remove` at all — but `mcp list` is
    // **not a passive probe**: per Claude Code docs it skips the workspace-
    // trust dialog and actually spawns every visible stdio server to run a
    // health check. Running it from `--scope local|project` in a hostile or
    // broken project dir is unsafe (arbitrary command execution) and
    // unreliable (hangs / fails on any broken sibling server). Instead we
    // always try `remove` first and classify its failure by stderr: benign
    // "not found" → continue to `add`; anything else → abort.
    if let Err(e) = run_cmd(
        "claude",
        &["mcp", "remove", SERVER_NAME, "--scope", scope_str],
    ) {
        if !claude_remove_error_is_not_found(&e) {
            return Outcome::Failed(claude_scope_hint(scope, format!("remove: {e}")));
        }
    }

    match run_cmd(
        "claude",
        &["mcp", "add", SERVER_NAME, "--scope", scope_str, &bin_str],
    ) {
        Ok(_) => Outcome::Done(format!("registered with Claude Code (scope={scope})")),
        Err(e) => Outcome::Failed(claude_scope_hint(scope, format!("add: {e}"))),
    }
}

fn register_codex(bin: &Path) -> Outcome {
    if which::which("codex").is_err() {
        return Outcome::Skipped("codex not found, skipping".into());
    }

    let bin_str = bin.to_string_lossy().to_string();

    // `codex mcp remove` exits 0 whether the entry existed or not (verified
    // empirically), so running it unconditionally is safe for idempotence.
    // A non-zero exit here IS a real failure (config unreadable, etc).
    if let Err(e) = run_cmd("codex", &["mcp", "remove", SERVER_NAME]) {
        return Outcome::Failed(format!("codex remove: {e}"));
    }
    match run_cmd("codex", &["mcp", "add", SERVER_NAME, "--", &bin_str]) {
        Ok(_) => Outcome::Done("registered with Codex".into()),
        Err(e) => Outcome::Failed(format!("codex add: {e}")),
    }
}

fn deregister_claude(scope: ClaudeScope) -> Outcome {
    if which::which("claude").is_err() {
        return Outcome::Skipped("claude not found, skipping".into());
    }

    // Same reasoning as `register_claude`: no `mcp list` probe. Attempt
    // `remove` and classify the stderr. `claude_remove_error_is_not_found`
    // recognises the benign "no <scope>-scoped MCP server found" response
    // and rejects any stderr that also contains corruption/error markers.
    match run_cmd(
        "claude",
        &["mcp", "remove", SERVER_NAME, "--scope", scope.as_str()],
    ) {
        Ok(_) => Outcome::Done(format!("deregistered from Claude Code (scope={scope})")),
        Err(e) if claude_remove_error_is_not_found(&e) => {
            Outcome::Skipped(not_found_skip_message(scope))
        }
        Err(e) => Outcome::Failed(claude_scope_hint(scope, format!("remove: {e}"))),
    }
}

fn not_found_skip_message(scope: ClaudeScope) -> String {
    match scope {
        ClaudeScope::User => "no user-scoped perfetto-mcp-rs registration to remove".into(),
        ClaudeScope::Local | ClaudeScope::Project => format!(
            "scope={scope}: not registered in this project directory. If you installed \
             from a different directory, re-run `perfetto-mcp-rs uninstall --scope {scope}` \
             from there; otherwise the entry is already gone."
        ),
    }
}

/// Claude's `mcp remove` wording for a missing entry is
/// `"No <scope>-scoped MCP server found with name: <name>"` (exit 1).
/// We match that pattern case-insensitively plus a `not found` fallback
/// for wording tweaks. **Tight rejection**: even when the benign marker is
/// present, we treat the output as a real failure if it also contains any
/// of the error/corruption markers below — Claude can append the normal
/// "not found" line after a config-corruption recovery message, and
/// uninstall must surface corruption rather than silently claim success.
fn claude_remove_error_is_not_found(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    let has_not_found_marker =
        lower.contains("scoped mcp server found with name") || lower.contains("not found");
    if !has_not_found_marker {
        return false;
    }
    // Only classify as benign when the output is JUST the not-found line —
    // any of these markers indicates something else went wrong too.
    let has_error_marker = lower.contains("error:")
        || lower.contains("corrupt")
        || lower.contains("backed up")
        || lower.contains("failed to")
        || lower.contains("could not")
        || lower.contains("unable to")
        || lower.contains("permission denied");
    !has_error_marker
}

fn deregister_codex() -> Outcome {
    if which::which("codex").is_err() {
        return Outcome::Skipped("codex not found, skipping".into());
    }
    // `codex mcp remove` exits 0 whether the entry existed or not (empirically
    // verified). A non-zero exit IS a real failure — don't downgrade it.
    match run_cmd("codex", &["mcp", "remove", SERVER_NAME]) {
        Ok(_) => Outcome::Done("deregistered from Codex".into()),
        Err(e) => Outcome::Failed(format!("codex remove: {e}")),
    }
}

fn clean_cache() -> Outcome {
    let root = match crate::download::cache_root() {
        Ok(p) => p,
        Err(e) => {
            return Outcome::Skipped(format!("cache root unavailable; skipping cleanup: {e}"));
        }
    };
    match std::fs::remove_dir_all(&root) {
        Ok(()) => Outcome::Done(format!("removed cache {}", root.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Outcome::Done(format!("cache {} already absent", root.display()))
        }
        Err(e) => Outcome::Failed(format!("remove_dir_all({}) failed: {e}", root.display())),
    }
}

fn claude_scope_hint(scope: ClaudeScope, underlying: String) -> String {
    if scope == ClaudeScope::User {
        underlying
    } else {
        format!(
            "scope={scope}: this command must run from the project directory \
             that was used at install time. If you're sure this is the right \
             directory, the underlying error follows:\n    {underlying}"
        )
    }
}

/// Run `cmd args...` and return stdout on success, a combined stderr/stdout
/// diagnostic on failure. Never panics.
fn run_cmd(cmd: &str, args: &[&str]) -> std::result::Result<String, String> {
    let output = match Command::new(cmd).args(args).output() {
        Ok(o) => o,
        Err(e) => {
            return Err(format!("spawn `{cmd} {}` failed: {e}", args.join(" ")));
        }
    };
    if output.status.success() {
        // stderr is intentionally ignored on the success path — fake CLIs (and
        // real ones) may print progress/warnings there and we don't want to
        // allocate or surface that noise when the call worked.
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = match (stderr.trim(), stdout.trim()) {
        ("", s) => s.to_string(),
        (s, "") => s.to_string(),
        (se, so) => format!("{se}\n{so}"),
    };
    Err(format!(
        "`{cmd} {}` exited {}: {}",
        args.join(" "),
        output.status,
        combined.trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_scope_hint_user_is_passthrough() {
        assert_eq!(claude_scope_hint(ClaudeScope::User, "boom".into()), "boom");
    }

    #[test]
    fn claude_scope_hint_non_user_wraps() {
        let s = claude_scope_hint(ClaudeScope::Local, "underlying-error".into());
        assert!(s.contains("scope=local"));
        assert!(s.contains("project directory"));
        assert!(s.contains("underlying-error"));
    }

    #[test]
    fn not_found_marker_matches_real_claude_output() {
        // Real wording observed from `claude mcp remove ... --scope <X>`:
        assert!(claude_remove_error_is_not_found(
            "No user-scoped MCP server found with name: perfetto-mcp-rs"
        ));
        assert!(claude_remove_error_is_not_found(
            "No local-scoped MCP server found with name: perfetto-mcp-rs"
        ));
        assert!(claude_remove_error_is_not_found(
            "no project-scoped mcp server found with name: perfetto-mcp-rs" // case-insensitive
        ));
        // Generic fallback marker.
        assert!(claude_remove_error_is_not_found("server not found"));
    }

    #[test]
    fn not_found_marker_rejects_real_errors() {
        // Broken config, read-only FS, random I/O errors — MUST NOT be
        // classified as benign "not found".
        assert!(!claude_remove_error_is_not_found(
            "Error: unable to parse ~/.claude.json: syntax error at line 42"
        ));
        assert!(!claude_remove_error_is_not_found(
            "EACCES: permission denied writing ~/.claude.json"
        ));
        assert!(!claude_remove_error_is_not_found("segmentation fault"));
        assert!(!claude_remove_error_is_not_found(""));
    }

    #[test]
    fn not_found_marker_rejects_mixed_corruption_plus_not_found() {
        // Codex's P2 concern: Claude can append a normal "not found" line
        // after a config-corruption recovery message. Both markers present
        // → must NOT be classified benign.
        assert!(!claude_remove_error_is_not_found(
            "Warning: ~/.claude.json was corrupted; backed up to ~/.claude.json.bak.\n\
             No user-scoped MCP server found with name: perfetto-mcp-rs"
        ));
        assert!(!claude_remove_error_is_not_found(
            "Error: failed to load ~/.claude.json: syntax error. Using fresh config.\n\
             No user-scoped MCP server found with name: perfetto-mcp-rs"
        ));
        assert!(!claude_remove_error_is_not_found(
            "could not write ~/.claude.json: permission denied. \
             No user-scoped MCP server found with name: perfetto-mcp-rs"
        ));
        assert!(!claude_remove_error_is_not_found(
            "unable to read config. No user-scoped MCP server found with name: x"
        ));
    }
}
