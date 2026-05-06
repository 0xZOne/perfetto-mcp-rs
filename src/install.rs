// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

//! `install` / `uninstall` subcommand logic.
//!
//! Binary self-registers with Claude Code and Codex; cleans its own
//! `dirs::data_local_dir()` cache. Shell wrappers (install.sh / install.ps1)
//! keep only distribution + platform glue.
//!
//! Qoder is handled via `Outcome::Manual` — Qoder has no public programmatic
//! MCP-registration API yet (UI-only flow per docs.qoder.com), so when Qoder
//! is detected we emit a paste-ready JSON snippet instead of writing files.
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
    /// Path to register with Claude/Codex. Required — we deliberately do
    /// NOT fall back to `current_exe()` even when the binary is invoking
    /// itself: on Linux `current_exe()` reads `/proc/self/exe` which is
    /// always the symlink-resolved target. For a versioned install like
    /// `~/bin/perfetto-mcp-rs -> ~/opt/perfetto-mcp-rs-0.8.0/bin`, falling
    /// back would pin the 0.8.0 path and break future symlink re-point
    /// upgrades. Shell wrappers pass this automatically. For manual
    /// invocation use `$(which perfetto-mcp-rs)` — `which` does NOT
    /// follow symlinks, so it yields the right stable path.
    #[arg(long, required = true)]
    pub binary_path: PathBuf,

    /// Claude scope (user/local/project). Ignored by Codex (no scope concept).
    /// For `--scope local` / `project`, run from the target project directory.
    #[arg(long, value_enum, default_value_t = ClaudeScope::User)]
    pub scope: ClaudeScope,

    #[arg(long)]
    pub skip_claude: bool,

    #[arg(long)]
    pub skip_codex: bool,

    /// Suppress the Qoder paste-ready snippet even when Qoder is detected.
    /// Qoder has no programmatic MCP-registration API yet, so detection
    /// triggers a printed JSON snippet rather than file writes.
    #[arg(long)]
    pub skip_qoder: bool,
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

    /// Suppress the Qoder manual-cleanup hint even when Qoder is detected.
    #[arg(long)]
    pub skip_qoder: bool,
}

enum Outcome {
    Done(String),
    Skipped(String),
    Failed(String),
    /// Target client detected but no programmatic registration API exists.
    /// Printed prominently as a multi-line "action required" block; treated
    /// like a soft success by `aggregate` (does NOT cause a non-zero exit).
    Manual {
        headline: String,
        body: String,
    },
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
    let bin = std::path::absolute(&args.binary_path).with_context(|| {
        format!(
            "failed to make --binary-path absolute: {}",
            args.binary_path.display()
        )
    })?;

    // Existence + regular-file + executable check. Without these, `install
    // --binary-path X` silently writes a dead MCP entry: claude/codex `mcp
    // add` accept any string and the failure only surfaces at client spawn
    // time, long after the user has moved on.
    //
    // The executable check matters for the advertised "direct" workflow in
    // README ("download from releases, then `perfetto-mcp-rs install
    // --binary-path /path`"): browser-downloaded Unix binaries land as
    // 0644, so `is_file` isn't enough. On Windows there's no exec bit, so
    // the Unix check is gated behind `#[cfg(unix)]`.
    let md = std::fs::metadata(&bin).map_err(|e| {
        anyhow!(
            "--binary-path {} is not accessible: {e}. Registering it would write a \
             broken MCP entry; re-check the path, or use the install.sh / install.ps1 \
             wrapper which downloads to a known location first.",
            bin.display()
        )
    })?;
    if !md.is_file() {
        return Err(anyhow!(
            "--binary-path {} is not a regular file",
            bin.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = md.permissions().mode();
        if mode & 0o111 == 0 {
            return Err(anyhow!(
                "--binary-path {} is not executable (mode {:o}). Browser-downloaded \
                 release binaries commonly land as 0644; run `chmod +x {}` first, or \
                 use the install.sh wrapper which sets the bit automatically.",
                bin.display(),
                mode & 0o777,
                bin.display()
            ));
        }
    }

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
        (
            "Qoder",
            if args.skip_qoder {
                Outcome::Skipped("--skip-qoder".into())
            } else {
                register_qoder(&bin)
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
            "Qoder",
            if args.skip_qoder {
                Outcome::Skipped("--skip-qoder".into())
            } else {
                deregister_qoder()
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
            // Multi-line Skipped: first line goes after the `==>` header,
            // subsequent lines indent under it. Keeps backwards-compat with
            // existing single-line skips while letting register_codex /
            // friends append a paste-ready manual-fallback hint.
            Outcome::Skipped(msg) => {
                let mut lines = msg.lines();
                if let Some(first) = lines.next() {
                    println!("==> {label} skipped: {first}");
                    for line in lines {
                        println!("    {line}");
                    }
                }
            }
            Outcome::Failed(msg) => {
                eprintln!("warning: {label} failed: {msg}");
                failure_msgs.push(format!("{label}: {msg}"));
            }
            Outcome::Manual { headline, body } => {
                println!("==> {label}: {headline}");
                for line in body.lines() {
                    println!("    {line}");
                }
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
    if which::which("codex").is_ok() {
        return register_codex_via_cli(bin);
    }
    // CLI not on PATH — fall back to indirect detection. All Codex surfaces
    // (CLI, VS Code extension, Mac/desktop app) read the same
    // `~/.codex/config.toml`, so any user with Codex configured can be
    // helped by a paste-ready TOML snippet — they just have to edit the
    // file themselves. Without a positive install signal we stay silent;
    // showing a "if Codex is actually installed..." hint to users who
    // genuinely don't use Codex is just noise.
    if codex_present_indirectly() {
        return codex_manual_install_outcome(bin);
    }
    Outcome::Skipped("codex not installed".into())
}

fn register_codex_via_cli(bin: &Path) -> Outcome {
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

/// Detect a Codex desktop app installation. Codex CLI (`which codex`) is
/// the primary signal; this is the secondary signal for users who only
/// installed the Mac/desktop app — the app reads the same
/// `~/.codex/config.toml` the CLI manages, so we can guide them to it.
///
/// Currently macOS-only because that's where the Mac app's canonical
/// install path is well-known. Linux/Windows desktop builds exist but
/// install paths vary across packagers; for those, the CLI path remains
/// the supported route.
fn detect_codex_app() -> bool {
    #[cfg(target_os = "macos")]
    {
        if Path::new("/Applications/Codex.app").exists() {
            return true;
        }
        if let Some(home) = dirs::home_dir() {
            if home.join("Applications/Codex.app").exists() {
                return true;
            }
        }
    }
    false
}

/// "Is Codex installed?" probe for the no-CLI path. Returns true if we have
/// any positive signal that the user is a Codex user — either the Mac
/// desktop app, or a `~/.codex/` directory created by some Codex surface
/// (CLI, VS Code extension, Mac app, etc.). The directory survives
/// uninstall of any single surface as long as login state / config remain,
/// so it's a robust "user has used Codex" signal across platforms.
///
/// Negative result is meant to be silent: don't tell users who genuinely
/// don't use Codex how to register manually.
fn codex_present_indirectly() -> bool {
    if detect_codex_app() {
        return true;
    }
    if let Some(home) = dirs::home_dir() {
        if home.join(".codex").exists() {
            return true;
        }
    }
    false
}

/// Build a paste-ready snippet for `~/.codex/config.toml`. Uses TOML
/// literal strings (`'...'`) so backslashes and double quotes in the path
/// pass through unchanged — important for Windows-form paths even though
/// the desktop-app fallback is currently macOS-only (future-proofing).
/// Falls back to a basic string with proper escaping if the path itself
/// contains a single quote (TOML literal strings can't escape `'`).
fn codex_toml_snippet(bin: &Path) -> String {
    let path_str = bin.to_string_lossy();
    if path_str.contains('\'') {
        let escaped = path_str.replace('\\', "\\\\").replace('"', "\\\"");
        format!("[mcp_servers.{SERVER_NAME}]\ncommand = \"{escaped}\"\n")
    } else {
        format!("[mcp_servers.{SERVER_NAME}]\ncommand = '{path_str}'\n")
    }
}

fn codex_manual_install_outcome(bin: &Path) -> Outcome {
    let snippet = codex_toml_snippet(bin);
    let body = format!(
        "Codex CLI not on PATH. All Codex surfaces (CLI, Mac/desktop app, \
         VS Code ext) read the same `~/.codex/config.toml`; add this and \
         restart Codex:\n\
         \n\
         {snippet}\n\
         (Create `~/.codex/config.toml` if it doesn't exist. \
         Reference: https://developers.openai.com/codex/config-reference)"
    );
    Outcome::Manual {
        headline: "detected — needs one-time manual setup (CLI not on PATH)".into(),
        body,
    }
}

fn deregister_claude(scope: ClaudeScope) -> Outcome {
    if which::which("claude").is_err() {
        return Outcome::Skipped("claude not found, skipping".into());
    }

    // No `mcp list` probe — list is not passive (it spawns visible stdio
    // servers for health checks). Attempt `remove` and classify the stderr.
    match run_cmd(
        "claude",
        &["mcp", "remove", SERVER_NAME, "--scope", scope.as_str()],
    ) {
        Ok(_) => Outcome::Done(format!("deregistered from Claude Code (scope={scope})")),

        // `not found` response classification depends on scope:
        //
        // - scope == User: user-scope entries are visible from any CWD, so a
        //   "not found" genuinely means nothing to remove — benign Skipped.
        //   The wrapper treats Skipped as success and may remove the binary.
        //
        // - scope == Local/Project: these scopes are CWD-keyed. "not found"
        //   here could mean "this scope never had an entry" **or** "the user
        //   is in the wrong CWD and the real registration is still alive
        //   somewhere else". We can't tell which — and the wrapper would
        //   happily delete the binary on Skipped, leaving a dangling
        //   registration pointing at a deleted path. Return Failed with a
        //   hint so the wrapper keeps the binary and the user can retry
        //   from the right directory (or confirm the entry was truly gone).
        Err(e) if claude_remove_error_is_not_found(&e) => match scope {
            ClaudeScope::User => {
                Outcome::Skipped("no user-scoped perfetto-mcp-rs registration to remove".into())
            }
            ClaudeScope::Local | ClaudeScope::Project => Outcome::Failed(format!(
                "scope={scope}: `claude mcp remove` reported no such entry here. Either the \
                 registration was already removed, or you are not in the project directory \
                 used at install time. Run `uninstall --scope {scope}` from that directory; \
                 if the entry is truly gone, pass --skip-claude to skip this check. Keeping \
                 binary in place."
            )),
        },

        Err(e) => Outcome::Failed(claude_scope_hint(scope, format!("remove: {e}"))),
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
    if which::which("codex").is_ok() {
        // `codex mcp remove` exits 0 whether the entry existed or not
        // (empirically verified). A non-zero exit IS a real failure —
        // don't downgrade it.
        return match run_cmd("codex", &["mcp", "remove", SERVER_NAME]) {
            Ok(_) => Outcome::Done("deregistered from Codex".into()),
            Err(e) => Outcome::Failed(format!("codex remove: {e}")),
        };
    }
    if codex_present_indirectly() {
        return Outcome::Manual {
            headline: "detected — needs manual cleanup (CLI not on PATH)".into(),
            body: format!(
                "Open `~/.codex/config.toml` and remove the \
                 `[mcp_servers.{SERVER_NAME}]` table, then restart Codex. \
                 (The binary and cache will still be removed below.)"
            ),
        };
    }
    Outcome::Skipped("codex not installed".into())
}

/// Detect a Qoder installation. Qoder is an Electron-based AI IDE; it
/// publishes a `qoder` CLI on macOS/Linux and ships an `.app` bundle on
/// macOS / installer-placed program directory on Windows. We probe all
/// known locations because users can install through any of them.
fn detect_qoder() -> bool {
    if which::which("qoder").is_ok() {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        if Path::new("/Applications/Qoder.app").exists() {
            return true;
        }
        if let Some(home) = dirs::home_dir() {
            if home.join("Applications/Qoder.app").exists() {
                return true;
            }
        }
    }
    #[cfg(windows)]
    {
        if let Some(local) = dirs::data_local_dir() {
            // Default Qoder Windows installer location.
            if local.join("Programs").join("Qoder").exists() {
                return true;
            }
        }
    }
    false
}

/// Build the JSON snippet a user pastes into Qoder Settings → MCP → + Add.
/// Uses serde_json so backslashes in Windows paths are escaped correctly.
fn qoder_snippet(bin: &Path) -> String {
    let value = serde_json::json!({
        "mcpServers": {
            SERVER_NAME: {
                "command": bin.to_string_lossy(),
            }
        }
    });
    // Pretty-print is the readable form Qoder's UI accepts; serde_json never
    // fails on a value built from owned data, so unwrap is sound here.
    serde_json::to_string_pretty(&value)
        .expect("serde_json::to_string_pretty cannot fail on owned Value")
}

fn register_qoder(bin: &Path) -> Outcome {
    if !detect_qoder() {
        return Outcome::Skipped("Qoder not found, skipping".into());
    }
    let snippet = qoder_snippet(bin);
    let body = format!(
        "Open Qoder Settings → MCP → + Add and paste this JSON:\n\
         \n\
         {snippet}\n\
         \n\
         (Qoder has no programmatic MCP-registration API yet; \
         see https://docs.qoder.com/user-guide/chat/model-context-protocol)"
    );
    Outcome::Manual {
        headline: "detected — needs one-time manual setup".into(),
        body,
    }
}

fn deregister_qoder() -> Outcome {
    if !detect_qoder() {
        return Outcome::Skipped("Qoder not found, skipping".into());
    }
    Outcome::Manual {
        headline: "detected — needs manual cleanup".into(),
        body: format!(
            "Open Qoder Settings → MCP and remove the `{SERVER_NAME}` entry.\n\
             (Qoder has no CLI for this yet; the binary and cache will still \
             be removed below.)"
        ),
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
        // PermissionDenied / "Access is denied" typically means another
        // MCP client (Claude Code, Codex) has a live perfetto-mcp-rs
        // instance with trace_processor_shell still mapped — we can't
        // unlink it from this process. Don't escalate to Failed: Claude/
        // Codex deregistration already succeeded above, so the binary
        // SHOULD be removable. Cache leftovers are harmless (the dir will
        // just re-populate on the next install). Tell the user how to
        // finish the cleanup manually if they want it gone.
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Outcome::Skipped(format!(
            "could not remove cache {} ({e}). On Windows this usually means an \
             MCP client still has trace_processor_shell open; close Claude Code / \
             Codex and re-run uninstall, or delete the directory manually.",
            root.display()
        )),
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
///
/// Program discovery is done via `which::which` (not plain `Command::new`)
/// so both `detect_cli` and the actual spawn see the same executable. This
/// matters on Windows: npm-installed CLIs like Codex land as `codex.cmd`
/// shims, `which` applies PATHEXT and finds them, but `Command::new("codex")`
/// has historically had version-dependent PATHEXT behavior — and even when
/// discovery works, `CreateProcess` can't exec `.cmd`/`.bat` directly, so
/// on Windows we detour batch files through `cmd /c`.
fn run_cmd(cmd: &str, args: &[&str]) -> std::result::Result<String, String> {
    let resolved =
        which::which(cmd).map_err(|e| format!("spawn `{cmd} {}` failed: {e}", args.join(" ")))?;
    let output = match build_spawn_command(&resolved).args(args).output() {
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

/// On Windows, route `.cmd`/`.bat` shims through `cmd /c` — `CreateProcessW`
/// can't exec them directly. On all other platforms (and for Windows .exe
/// targets) spawn directly.
#[cfg(windows)]
fn build_spawn_command(resolved: &Path) -> Command {
    let ext = resolved
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase);
    if matches!(ext.as_deref(), Some("cmd") | Some("bat")) {
        let mut c = Command::new("cmd");
        c.arg("/c").arg(resolved);
        c
    } else {
        Command::new(resolved)
    }
}

#[cfg(not(windows))]
fn build_spawn_command(resolved: &Path) -> Command {
    Command::new(resolved)
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
    fn codex_toml_snippet_uses_literal_string_for_simple_path() {
        let bin = PathBuf::from("/Users/jhon.lgh/.local/bin/perfetto-mcp-rs");
        let s = codex_toml_snippet(&bin);
        assert!(s.contains(&format!("[mcp_servers.{SERVER_NAME}]")));
        // Literal-string form (single-quoted) — no escape processing.
        assert!(s.contains("command = '/Users/jhon.lgh/.local/bin/perfetto-mcp-rs'"));
    }

    #[test]
    fn codex_toml_snippet_passes_windows_backslashes_through_literal_string() {
        // TOML literal strings don't process escapes, so backslashes
        // appear as-is. This is the correct on-disk form for Windows
        // paths if/when the desktop-app fallback expands beyond macOS.
        let bin = PathBuf::from(r"C:\Users\me\.local\bin\perfetto-mcp-rs.exe");
        let s = codex_toml_snippet(&bin);
        assert!(s.contains(r"command = 'C:\Users\me\.local\bin\perfetto-mcp-rs.exe'"));
        // Belt-and-braces: must NOT have doubled backslashes (that's basic-
        // string escaping, wrong for literal strings).
        assert!(!s.contains(r"\\Users"));
    }

    #[test]
    fn codex_toml_snippet_falls_back_to_basic_string_when_path_has_quote() {
        // Single quote in path forces basic-string form with escaping —
        // TOML literal strings have no way to escape `'`.
        let bin = PathBuf::from("/tmp/it's-a-path/perfetto-mcp-rs");
        let s = codex_toml_snippet(&bin);
        // Basic string: double-quoted, no escapes needed for single quote.
        assert!(s.contains(r#"command = "/tmp/it's-a-path/perfetto-mcp-rs""#));
    }

    #[test]
    fn qoder_snippet_contains_server_name_and_command() {
        let bin = PathBuf::from("/usr/local/bin/perfetto-mcp-rs");
        let s = qoder_snippet(&bin);
        // Must be valid JSON the user can paste directly.
        let parsed: serde_json::Value =
            serde_json::from_str(&s).expect("qoder_snippet must emit valid JSON");
        assert_eq!(
            parsed["mcpServers"][SERVER_NAME]["command"],
            "/usr/local/bin/perfetto-mcp-rs"
        );
    }

    #[test]
    fn qoder_snippet_escapes_windows_backslashes() {
        // serde_json must escape `\` so the pasted JSON parses inside Qoder.
        // Manual string-formatting would be a footgun; this guards against
        // someone "simplifying" qoder_snippet later.
        let bin = PathBuf::from(r"C:\Users\me\.local\bin\perfetto-mcp-rs.exe");
        let s = qoder_snippet(&bin);
        let parsed: serde_json::Value =
            serde_json::from_str(&s).expect("qoder_snippet must emit valid JSON");
        assert_eq!(
            parsed["mcpServers"][SERVER_NAME]["command"],
            r"C:\Users\me\.local\bin\perfetto-mcp-rs.exe"
        );
        // Belt-and-braces: the on-the-wire form must contain escaped
        // backslashes, not raw ones.
        assert!(s.contains(r"\\"));
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
