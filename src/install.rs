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
    /// Printed prominently as a multi-line "action required" block.
    ///
    /// `blocking` controls whether `aggregate` treats this as a failure:
    ///
    /// - `false` (install side): soft success. The binary IS placed; the
    ///   user just needs to do a one-time wire-up. Exit code stays 0 so
    ///   the install wrapper finishes cleanly.
    ///
    /// - `true` (uninstall side): MUST fail aggregate. The user still has
    ///   a `[mcp_servers.<name>]` entry in some client config that we
    ///   can't touch programmatically; if exit were 0, the wrapper would
    ///   delete the binary at uninstall.sh:70 / uninstall.ps1 and leave a
    ///   dangling MCP entry pointing at a deleted path. Failure keeps the
    ///   binary in place until the user re-runs uninstall (with
    ///   `--skip-<client>` after they've done the manual cleanup, or
    ///   without it once the client is gone).
    Manual {
        headline: String,
        body: String,
        blocking: bool,
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
            Outcome::Manual {
                headline,
                body,
                blocking,
            } => {
                println!("==> {label}: {headline}");
                for line in body.lines() {
                    println!("    {line}");
                }
                if *blocking {
                    failure_msgs.push(format!("{label}: {headline}"));
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
    if which::which("claude").is_ok() {
        return register_claude_via_cli(bin, scope);
    }
    let Some((has_desktop, has_cli_config)) = detect_claude_products(scope) else {
        return Outcome::Skipped("claude not installed".into());
    };
    Outcome::Manual {
        headline: "detected — needs one-time manual setup (CLI not on PATH)".into(),
        body: claude_manual_install_body(bin, scope, has_desktop, has_cli_config),
        blocking: false,
    }
}

/// Detect Claude products visible without the CLI on PATH and return
/// `(has_desktop, has_cli_config)`, or `None` if neither signals — caller
/// should emit a quiet Skipped.
///
/// Claude Desktop fallback is gated to `scope=User`: Local/Project are
/// Claude Code-only concepts without a Desktop analog, so silently
/// rerouting to Desktop's single global config would violate the user's
/// intent. The CLI-config branch is scope-agnostic — its hint just echoes
/// whatever scope was requested.
fn detect_claude_products(scope: ClaudeScope) -> Option<(bool, bool)> {
    let has_desktop = scope == ClaudeScope::User && detect_claude_desktop();
    let has_cli_config = claude_code_present_indirectly();
    (has_desktop || has_cli_config).then_some((has_desktop, has_cli_config))
}

fn register_claude_via_cli(bin: &Path, scope: ClaudeScope) -> Outcome {
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

/// Probe `/Applications/<name>` and `~/Applications/<name>` for a macOS
/// `.app` bundle. Returns false on non-macOS so callers don't need cfg
/// guards. Used by detection probes for Codex / Claude Desktop / Qoder —
/// all three follow the same Mac install convention.
#[cfg(target_os = "macos")]
fn macos_app_bundle_present(name: &str) -> bool {
    if Path::new("/Applications").join(name).exists() {
        return true;
    }
    dirs::home_dir().is_some_and(|h| h.join("Applications").join(name).exists())
}

#[cfg(not(target_os = "macos"))]
fn macos_app_bundle_present(_name: &str) -> bool {
    false
}

/// Detect a Codex desktop app installation. Currently macOS-only because
/// that's where the Mac app's canonical install path is well-known;
/// Linux/Windows desktop builds exist but install paths vary across
/// packagers, so for those the CLI path remains the supported route.
fn detect_codex_app() -> bool {
    macos_app_bundle_present("Codex.app")
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
        blocking: false,
    }
}

/// Detect Claude Desktop (Anthropic's GUI chat app — distinct from the
/// Claude Code CLI). Probes the macOS `.app` bundle and the OS-specific
/// data dir; the data dir is created the first time Claude Desktop runs,
/// regardless of platform, making it the most portable signal.
fn detect_claude_desktop() -> bool {
    if macos_app_bundle_present("Claude.app") {
        return true;
    }
    claude_desktop_config_dir().is_some_and(|p| p.exists())
}

/// Per-platform Claude Desktop data dir (the parent of
/// `claude_desktop_config.json`). Uses `dirs::config_dir()` because its
/// platform mapping happens to match Claude Desktop's actual layout:
/// macOS → `~/Library/Application Support`, Windows → `%APPDATA%`,
/// Linux → `~/.config`.
fn claude_desktop_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("Claude"))
}

fn claude_desktop_config_path() -> Option<PathBuf> {
    claude_desktop_config_dir().map(|d| d.join("claude_desktop_config.json"))
}

/// Display form of the Claude Desktop config path for hint messages.
/// Falls back to the bare filename when `dirs::config_dir()` returns None
/// (extremely rare — only headless / chrooted environments).
fn claude_desktop_config_path_display() -> String {
    claude_desktop_config_path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "claude_desktop_config.json".into())
}

/// "Is Claude Code (CLI) installed?" probe for the no-PATH path.
/// `~/.claude.json` is the user-scope config the CLI manages; `~/.claude/`
/// can hold the bootstrapped binary on installs that drop into
/// `~/.claude/local/`. Either presence is a strong "Claude Code has been
/// used here" signal that survives PATH-hidden subshells.
fn claude_code_present_indirectly() -> bool {
    if let Some(home) = dirs::home_dir() {
        if home.join(".claude.json").exists() {
            return true;
        }
        if home.join(".claude").exists() {
            return true;
        }
    }
    false
}

/// Build the install-side Manual body. Conditionally includes a Claude
/// Desktop section, a Claude Code (CLI) section, or both — depending on
/// which products were actually detected. Kept in one function so the
/// interleaving formatting (header, two product subsections, separators)
/// stays readable.
fn claude_manual_install_body(
    bin: &Path,
    scope: ClaudeScope,
    has_desktop: bool,
    has_cli_config: bool,
) -> String {
    let mut body = String::from(
        "Claude CLI not on PATH. Paste-ready steps below for whichever \
         Claude products you have installed:\n\n",
    );
    if has_desktop {
        let snippet = mcp_servers_json_snippet(bin);
        let cfg_path = claude_desktop_config_path_display();
        body.push_str(&format!(
            "Claude Desktop:\n\
             Add this to {cfg_path} (create it if missing), then restart \
             Claude Desktop:\n\
             \n\
             {snippet}\n"
        ));
        if has_cli_config {
            body.push('\n');
        }
    }
    if has_cli_config {
        body.push_str(
            "Claude Code (CLI):\n\
             From a shell where `claude` is on PATH, run:\n\n",
        );
        body.push_str(&format!(
            "    claude mcp add {SERVER_NAME} --scope {scope} -- {}",
            bin.display()
        ));
    }
    body
}

/// Build the uninstall-side Manual body — symmetric to install.
fn claude_manual_uninstall_body(
    scope: ClaudeScope,
    has_desktop: bool,
    has_cli_config: bool,
) -> String {
    let mut body = String::from(
        "Claude CLI not on PATH. Cleanup steps for whichever Claude \
         products you have. The binary will be kept until cleanup is \
         confirmed — after the steps below, finish uninstall with \
         `SKIP_CLAUDE=1` (env var on the wrapper) or `--skip-claude` \
         (on direct binary invocation):\n\n",
    );
    if has_desktop {
        let cfg_path = claude_desktop_config_path_display();
        body.push_str(&format!(
            "Claude Desktop:\n\
             Open {cfg_path} and remove the `{SERVER_NAME}` entry under \
             `mcpServers`, then restart Claude Desktop.\n"
        ));
        if has_cli_config {
            body.push('\n');
        }
    }
    if has_cli_config {
        body.push_str(
            "Claude Code (CLI):\n\
             From a shell where `claude` is on PATH, run:\n\n",
        );
        body.push_str(&format!(
            "    claude mcp remove {SERVER_NAME} --scope {scope}"
        ));
    }
    body
}

fn deregister_claude(scope: ClaudeScope) -> Outcome {
    if which::which("claude").is_ok() {
        return deregister_claude_via_cli(scope);
    }
    let Some((has_desktop, has_cli_config)) = detect_claude_products(scope) else {
        return Outcome::Skipped("claude not installed".into());
    };
    Outcome::Manual {
        headline: "detected — needs manual cleanup (CLI not on PATH)".into(),
        body: claude_manual_uninstall_body(scope, has_desktop, has_cli_config),
        blocking: true,
    }
}

fn deregister_claude_via_cli(scope: ClaudeScope) -> Outcome {
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
                 The binary will be kept until cleanup is confirmed — \
                 finish uninstall with `SKIP_CODEX=1` (env var on the \
                 wrapper) or `--skip-codex` (on direct binary invocation)."
            ),
            blocking: true,
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
    if macos_app_bundle_present("Qoder.app") {
        return true;
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

/// Build a `{ "mcpServers": { "<name>": { "command": "<path>" } } }` JSON
/// snippet. Qoder Settings → MCP → + Add and Claude Desktop's
/// `claude_desktop_config.json` both accept this exact format, so they
/// share this builder. Uses serde_json so backslashes in Windows paths
/// are escaped correctly.
fn mcp_servers_json_snippet(bin: &Path) -> String {
    let value = serde_json::json!({
        "mcpServers": {
            SERVER_NAME: {
                "command": bin.to_string_lossy(),
            }
        }
    });
    serde_json::to_string_pretty(&value)
        .expect("serde_json::to_string_pretty cannot fail on owned Value")
}

fn register_qoder(bin: &Path) -> Outcome {
    if !detect_qoder() {
        return Outcome::Skipped("Qoder not found, skipping".into());
    }
    let snippet = mcp_servers_json_snippet(bin);
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
        blocking: false,
    }
}

fn deregister_qoder() -> Outcome {
    if !detect_qoder() {
        return Outcome::Skipped("Qoder not found, skipping".into());
    }
    // Non-blocking by design. Codex (`~/.codex/config.toml`) and Claude
    // Desktop (per-OS well-known JSON path) document where their MCP
    // config lives, so we can tell the user *which file to edit* and
    // blocking the uninstall is a meaningful safety net — they have a
    // concrete next action. Qoder's config path is undocumented (UI-only
    // management), so the best we can offer is "open Settings → MCP";
    // blocking would trap users who can't comply with anything more
    // specific. If the user forgets, Qoder will surface the dangling
    // entry on next launch and they can remove it then.
    Outcome::Manual {
        headline: "detected — manual cleanup is your responsibility".into(),
        body: format!(
            "Qoder has no programmatic API for MCP servers and its config \
             file path isn't documented, so perfetto-mcp-rs can't verify \
             cleanup. Open Qoder Settings → MCP and remove the \
             `{SERVER_NAME}` entry. (Uninstall will proceed regardless.)"
        ),
        blocking: false,
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
    fn claude_manual_install_body_includes_desktop_section_when_detected() {
        let bin = PathBuf::from("/usr/local/bin/perfetto-mcp-rs");
        let body = claude_manual_install_body(&bin, ClaudeScope::User, true, false);
        // Desktop section present, CLI section absent.
        assert!(body.contains("Claude Desktop:"));
        assert!(!body.contains("Claude Code (CLI):"));
        // Body embeds the JSON snippet (verified separately above).
        assert!(body.contains("\"mcpServers\""));
    }

    #[test]
    fn claude_manual_install_body_includes_cli_section_when_detected() {
        let bin = PathBuf::from("/usr/local/bin/perfetto-mcp-rs");
        let body = claude_manual_install_body(&bin, ClaudeScope::User, false, true);
        assert!(!body.contains("Claude Desktop:"));
        assert!(body.contains("Claude Code (CLI):"));
        // CLI hint must include the actual paste-ready command with scope
        // and binary path the caller passed in.
        assert!(body.contains("claude mcp add perfetto-mcp-rs --scope user --"));
        assert!(body.contains("/usr/local/bin/perfetto-mcp-rs"));
    }

    #[test]
    fn claude_manual_install_body_includes_both_sections_when_both_detected() {
        let bin = PathBuf::from("/usr/local/bin/perfetto-mcp-rs");
        let body = claude_manual_install_body(&bin, ClaudeScope::User, true, true);
        assert!(body.contains("Claude Desktop:"));
        assert!(body.contains("Claude Code (CLI):"));
        // Order: Desktop block before CLI block.
        let desktop_idx = body.find("Claude Desktop:").unwrap();
        let cli_idx = body.find("Claude Code (CLI):").unwrap();
        assert!(desktop_idx < cli_idx);
    }

    #[test]
    fn claude_manual_install_body_propagates_local_scope_into_cli_hint() {
        // Local/Project scope skips the Desktop fallback (gated upstream),
        // but the CLI hint must echo the scope the user requested.
        let bin = PathBuf::from("/usr/local/bin/perfetto-mcp-rs");
        let body = claude_manual_install_body(&bin, ClaudeScope::Local, false, true);
        assert!(body.contains("--scope local"));
    }

    // Lock the install vs uninstall asymmetry: install Manual is a soft
    // success (binary IS placed; user just needs a one-time wire-up),
    // uninstall Manual MUST fail aggregate (otherwise the wrapper deletes
    // the binary while a stale `[mcp_servers.<name>]` entry still points
    // at it — exactly the bug the upstream review flagged).
    #[test]
    fn aggregate_treats_blocking_manual_as_failure() {
        let outcomes = vec![(
            "Claude",
            Outcome::Manual {
                headline: "needs cleanup".into(),
                body: "edit config".into(),
                blocking: true,
            },
        )];
        assert!(
            aggregate(outcomes).is_err(),
            "blocking Manual must propagate as failure so the wrapper keeps the binary"
        );
    }

    // Pins Qoder's documented asymmetry vs Codex/Claude Desktop: its
    // config path is undocumented, so blocking the uninstall would trap
    // users with no concrete file to edit. On test machines without
    // Qoder this passes vacuously (Skipped); when Qoder IS detected the
    // outcome MUST be a non-blocking Manual — never a blocking one.
    #[test]
    fn qoder_deregister_is_non_blocking_when_detected() {
        if let Outcome::Manual { blocking, .. } = deregister_qoder() {
            assert!(
                !blocking,
                "Qoder uninstall must be non-blocking — config path is undocumented"
            );
        }
    }

    #[test]
    fn aggregate_treats_non_blocking_manual_as_success() {
        let outcomes = vec![(
            "Claude",
            Outcome::Manual {
                headline: "needs setup".into(),
                body: "paste this".into(),
                blocking: false,
            },
        )];
        assert!(
            aggregate(outcomes).is_ok(),
            "install Manual is soft success — binary IS placed, user just wires up"
        );
    }

    #[test]
    fn mcp_servers_json_snippet_contains_server_name_and_command() {
        let bin = PathBuf::from("/usr/local/bin/perfetto-mcp-rs");
        let s = mcp_servers_json_snippet(&bin);
        // Must be valid JSON the user can paste directly.
        let parsed: serde_json::Value =
            serde_json::from_str(&s).expect("snippet must emit valid JSON");
        assert_eq!(
            parsed["mcpServers"][SERVER_NAME]["command"],
            "/usr/local/bin/perfetto-mcp-rs"
        );
    }

    #[test]
    fn mcp_servers_json_snippet_escapes_windows_backslashes() {
        // serde_json must escape `\` so the pasted JSON parses inside the
        // target client. Manual string-formatting would be a footgun; this
        // guards against someone "simplifying" the builder later.
        let bin = PathBuf::from(r"C:\Users\me\.local\bin\perfetto-mcp-rs.exe");
        let s = mcp_servers_json_snippet(&bin);
        let parsed: serde_json::Value =
            serde_json::from_str(&s).expect("snippet must emit valid JSON");
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
