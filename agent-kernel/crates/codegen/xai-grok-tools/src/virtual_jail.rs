//! Virtual Workspace Jail (`~/private` + `~/shared`) for `xai-grok-tools`.
//!
//! Enforces strict kernel/filesystem-level speaker isolation without relying on LLM prompt memory:
//! 1. **Virtual Path Mapping**: Rewrites `~/private`, `/private`, `~/shared`, `/shared`, `~`, and
//!    relative paths strictly into `(host_private_dir, host_shared_dir)` and rejects any `..` or
//!    symlink escape outside those two roots with `PermissionDenied`.
//! 2. **`JailedFs` (`AsyncFileSystem`)**: Enforces the virtual jail on `read_file`, `read_file_bounded`,
//!    `write_file`, `file_exists`, and `delete_file` (used by `GrokBuild:read_file`, `GrokBuild:search_replace`, etc.).
//! 3. **`JailedTerminalBackend` + `JailedSandboxLaunch`**: Sanitizes `bash` child environments (stripping
//!    host secrets like `JWT_SECRET`, `DATABASE_URL`, `S3_*`, `AWS_*`, `*_API_KEY`), forces `HOME`/`TMPDIR`/`cwd`
//!    into the virtual workspace, wraps macOS child spawns in Seatbelt `sandbox-exec` (`(deny network*)` +
//!    write confinement to `{host_private_dir, host_shared_dir}`) and Linux child spawns in seccomp network
//!    restriction, and appends `[cwd: ~/private | shared: ~/shared]` while scrubbing raw host paths from output.

use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::computer::local::LocalFs;
use crate::computer::types::{
    AsyncFileSystem, BackgroundHandle, BackgroundedForeground, ComputerError, KillOutcome,
    KillSource, TaskSnapshot, TerminalBackend, TerminalRunRequest, TerminalRunResult,
};
use crate::notification::types::ToolNotificationHandle;
use crate::sandbox_launch::{
    BackendName, CallId, LaunchReceipt, OriginalArgv, SandboxLaunch, SandboxLaunchError,
};

#[derive(Debug, Clone)]
pub struct VirtualWorkspaceJail {
    host_private_dir: PathBuf,
    host_shared_dir: PathBuf,
    host_tmp_dir: PathBuf,
}

impl VirtualWorkspaceJail {
    /// Initialize and canonicalize the speaker's private root and the space's shared root.
    pub fn new(host_private_dir: impl AsRef<Path>, host_shared_dir: impl AsRef<Path>) -> io::Result<Self> {
        std::fs::create_dir_all(host_private_dir.as_ref())?;
        std::fs::create_dir_all(host_shared_dir.as_ref())?;
        let priv_canon = dunce::canonicalize(host_private_dir.as_ref())?;
        let shared_canon = dunce::canonicalize(host_shared_dir.as_ref())?;
        let tmp_dir = priv_canon.join(".tmp");
        std::fs::create_dir_all(&tmp_dir)?;
        // Create convenient symlinks inside private root so `ls ~/shared` or `./shared` works naturally in bash too!
        #[cfg(unix)]
        {
            let shared_link = priv_canon.join("shared");
            if !shared_link.exists() && shared_link.symlink_metadata().is_err() {
                let _ = std::os::unix::fs::symlink(&shared_canon, &shared_link);
            }
            let private_link = priv_canon.join("private");
            if !private_link.exists() && private_link.symlink_metadata().is_err() {
                let _ = std::os::unix::fs::symlink(".", &private_link);
            }
        }
        Ok(Self {
            host_private_dir: priv_canon,
            host_shared_dir: shared_canon,
            host_tmp_dir: tmp_dir,
        })
    }

    pub fn host_private_dir(&self) -> &Path {
        &self.host_private_dir
    }

    pub fn host_shared_dir(&self) -> &Path {
        &self.host_shared_dir
    }

    pub fn host_tmp_dir(&self) -> &Path {
        &self.host_tmp_dir
    }

    /// Resolve a model-supplied virtual or host path strictly inside `{host_private_dir, host_shared_dir}`.
    pub fn resolve_path(&self, raw: &Path) -> Result<PathBuf, ComputerError> {
        let raw_str = raw.to_string_lossy();
        let trimmed = raw_str.trim();

        // Check already-resolved physical workspace paths FIRST so macOS `/private/var/...` or `/private/tmp/...`
        // canonical paths are never mistaken for the virtual `/private/...` alias!
        let candidate = if raw.is_absolute()
            && (raw.starts_with(&self.host_private_dir) || raw.starts_with(&self.host_shared_dir))
        {
            raw.to_path_buf()
        } else if trimmed.is_empty()
            || trimmed == "~"
            || trimmed == "~/"
            || trimmed == "~/private"
            || trimmed == "/private"
        {
            self.host_private_dir.clone()
        } else if let Some(rest) = trimmed
            .strip_prefix("~/private/")
            .or_else(|| trimmed.strip_prefix("/private/"))
        {
            self.host_private_dir.join(rest)
        } else if trimmed == "~/shared" || trimmed == "/shared" {
            self.host_shared_dir.clone()
        } else if let Some(rest) = trimmed
            .strip_prefix("~/shared/")
            .or_else(|| trimmed.strip_prefix("/shared/"))
        {
            self.host_shared_dir.join(rest)
        } else if let Some(rest) = trimmed.strip_prefix("~/") {
            self.host_private_dir.join(rest)
        } else if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.host_private_dir.join(raw)
        };

        // Normalize lexical `..` and `.` components first without allowing escape past root.
        let normalized = lexical_normalize(&candidate);

        // Resolve existing ancestor canonically so symlinks pointing outside `{private, shared}` are caught!
        let effective = canonicalize_with_missing_tail(&normalized)?;
        if effective.starts_with(&self.host_private_dir)
            || effective.starts_with(&self.host_shared_dir)
        {
            Ok(effective)
        } else {
            xai_grok_sandbox::log_violation(&raw.display().to_string(), "virtual_jail");
            Err(ComputerError::io_with_kind(
                format!(
                    "Access denied: path '{}' is outside your virtual workspace (use ~/private or ~/shared)",
                    self.scrub_host_paths(&raw_str)
                ),
                io::ErrorKind::PermissionDenied,
            ))
        }
    }

    /// Convert a physical host path back to a model-friendly virtual path (`~/private/...` or `~/shared/...`).
    pub fn to_virtual_display(&self, path: &Path) -> String {
        let canon = dunce::canonicalize(path).unwrap_or_else(|_| lexical_normalize(path));
        if let Ok(rel) = canon.strip_prefix(&self.host_shared_dir) {
            let r = rel.to_string_lossy();
            return if r.is_empty() {
                "~/shared".to_string()
            } else {
                format!("~/shared/{r}")
            };
        }
        if let Ok(rel) = canon.strip_prefix(&self.host_private_dir) {
            let r = rel.to_string_lossy();
            return if r.is_empty() {
                "~/private".to_string()
            } else {
                format!("~/private/{r}")
            };
        }
        "~/private".to_string()
    }

    /// Scrub raw host directory prefixes from tool stdout/stderr so host layout never leaks into prompt context.
    pub fn scrub_host_paths(&self, text: &str) -> String {
        let shared_str = self.host_shared_dir.to_string_lossy();
        let priv_str = self.host_private_dir.to_string_lossy();
        let mut out = if !shared_str.is_empty() {
            text.replace(shared_str.as_ref(), "~/shared")
        } else {
            text.to_string()
        };
        if !priv_str.is_empty() {
            out = out.replace(priv_str.as_ref(), "~/private");
        }
        out
    }

    /// Build a strictly sanitized environment map for child shell processes (`env_clear` semantics).
    pub fn sanitized_env(&self, extra: &HashMap<String, String>) -> HashMap<String, String> {
        let mut env = HashMap::new();
        let path_val = std::env::var("PATH")
            .unwrap_or_else(|_| "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin".to_string());
        env.insert("PATH".to_string(), path_val);
        env.insert("LANG".to_string(), "en_US.UTF-8".to_string());
        env.insert("LC_ALL".to_string(), "en_US.UTF-8".to_string());
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert(
            "HOME".to_string(),
            self.host_private_dir.to_string_lossy().into_owned(),
        );
        env.insert(
            "PRIVATE_DIR".to_string(),
            self.host_private_dir.to_string_lossy().into_owned(),
        );
        env.insert(
            "SHARED_DIR".to_string(),
            self.host_shared_dir.to_string_lossy().into_owned(),
        );
        env.insert(
            "TMPDIR".to_string(),
            self.host_tmp_dir.to_string_lossy().into_owned(),
        );

        for (k, v) in extra {
            if !is_secret_env_name(k) {
                env.insert(k.clone(), v.clone());
            }
        }
        env
    }
}

fn is_secret_env_name(key: &str) -> bool {
    let u = key.to_ascii_uppercase();
    u.contains("SECRET")
        || u.contains("TOKEN")
        || u.contains("PASSWORD")
        || u.contains("API_KEY")
        || u.contains("PRIVATE_KEY")
        || u.starts_with("JWT_")
        || u.starts_with("DATABASE_")
        || u.starts_with("REDIS_")
        || u.starts_with("S3_")
        || u.starts_with("AWS_")
        || u.starts_with("OPENAI_")
        || u.starts_with("ANTHROPIC_")
        || u.starts_with("GEMINI_")
        || u.starts_with("DEEPSEEK_")
        || u.starts_with("GROK_")
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn canonicalize_with_missing_tail(path: &Path) -> Result<PathBuf, ComputerError> {
    if let Ok(canon) = dunce::canonicalize(path) {
        return Ok(canon);
    }
    let mut missing = Vec::new();
    let mut cur = path;
    while let Some(parent) = cur.parent() {
        if let Some(name) = cur.file_name() {
            missing.push(name.to_os_string());
        }
        if let Ok(canon_parent) = dunce::canonicalize(parent) {
            let mut rebuilt = canon_parent;
            for part in missing.into_iter().rev() {
                rebuilt.push(part);
            }
            return Ok(rebuilt);
        }
        cur = parent;
    }
    Ok(path.to_path_buf())
}

/// Virtual-root enforcing `AsyncFileSystem` wrapper around [`LocalFs`].
#[derive(Clone)]
pub struct JailedFs {
    jail: VirtualWorkspaceJail,
    inner: Arc<LocalFs>,
}

impl JailedFs {
    pub fn new(jail: VirtualWorkspaceJail) -> Self {
        Self {
            jail,
            inner: Arc::new(LocalFs),
        }
    }
}

#[async_trait::async_trait]
impl AsyncFileSystem for JailedFs {
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, ComputerError> {
        let resolved = self.jail.resolve_path(path)?;
        self.inner.read_file(&resolved).await
    }

    fn supports_bounded_read(&self) -> bool {
        self.inner.supports_bounded_read()
    }

    async fn read_file_bounded(
        &self,
        path: &Path,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ComputerError> {
        let resolved = self.jail.resolve_path(path)?;
        self.inner.read_file_bounded(&resolved, max_bytes).await
    }

    async fn write_file(&self, path: &Path, data: &[u8]) -> Result<(), ComputerError> {
        let resolved = self.jail.resolve_path(path)?;
        self.inner.write_file(&resolved, data).await
    }

    async fn file_exists(&self, path: &Path) -> Result<bool, ComputerError> {
        let resolved = self.jail.resolve_path(path)?;
        self.inner.file_exists(&resolved).await
    }

    async fn delete_file(&self, path: &Path) -> Result<(), ComputerError> {
        let resolved = self.jail.resolve_path(path)?;
        self.inner.delete_file(&resolved).await
    }
}

/// OS-level + environment-sanitizing `SandboxLaunch` hook for `LocalTerminalBackend`.
pub struct JailedSandboxLaunch {
    jail: VirtualWorkspaceJail,
}

impl JailedSandboxLaunch {
    pub fn new(jail: VirtualWorkspaceJail) -> Self {
        Self { jail }
    }

    #[cfg(target_os = "macos")]
    fn seatbelt_profile(&self) -> String {
        let escape = |p: &Path| {
            p.to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        };
        let priv_dir = escape(&self.jail.host_private_dir);
        let shared_dir = escape(&self.jail.host_shared_dir);
        format!(
            "(version 1)\n\
             (allow default)\n\
             (deny network*)\n\
             (deny file-write*)\n\
             (allow file-write* (subpath \"{priv_dir}\") (subpath \"{shared_dir}\") (subpath \"/dev\"))\n"
        )
    }
}

impl SandboxLaunch for JailedSandboxLaunch {
    fn prepare(
        &self,
        cmd: &mut tokio::process::Command,
        original: &OriginalArgv,
        _call: &CallId,
    ) -> Result<Option<LaunchReceipt>, SandboxLaunchError> {
        let safe_cwd = self
            .jail
            .resolve_path(&original.cwd)
            .unwrap_or_else(|_| self.jail.host_private_dir.clone());

        // Capture any safe caller-specified env vars before clearing inherited server secrets.
        let mut caller_env = HashMap::new();
        for (k, v) in cmd.as_std().get_envs() {
            if let (Some(ks), Some(vs)) = (k.to_str(), v.and_then(|val| val.to_str())) {
                caller_env.insert(ks.to_string(), vs.to_string());
            }
        }
        let clean_env = self.jail.sanitized_env(&caller_env);

        #[cfg(target_os = "macos")]
        {
            static CAN_NEST_SEATBELT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            let seatbelt_ok = *CAN_NEST_SEATBELT.get_or_init(|| {
                Path::new("/usr/bin/sandbox-exec").exists()
                    && std::process::Command::new("/usr/bin/sandbox-exec")
                        .args(["-p", "(version 1)\n(allow default)\n", "/usr/bin/true"])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                        .is_ok_and(|s| s.success())
            });
            if seatbelt_ok {
                let profile = self.seatbelt_profile();
                let mut wrapped = tokio::process::Command::new("/usr/bin/sandbox-exec");
                wrapped
                    .arg("-p")
                    .arg(profile)
                    .arg(&original.program)
                    .args(&original.args)
                    .current_dir(&safe_cwd)
                    .env_clear()
                    .envs(&clean_env);
                *cmd = wrapped;
                return Ok(Some(LaunchReceipt {
                    sandboxed: true,
                    backend: Some(BackendName::Seatbelt),
                }));
            }
        }

        let mut sanitized = tokio::process::Command::new(&original.program);
        sanitized
            .args(&original.args)
            .current_dir(&safe_cwd)
            .env_clear()
            .envs(&clean_env);
        *cmd = sanitized;
        Ok(Some(LaunchReceipt {
            sandboxed: cfg!(target_os = "linux"),
            backend: None,
        }))
    }
}

/// Virtual-root rewriting `TerminalBackend` wrapper that normalizes `working_directory`,
/// scrubs host paths from stdout/stderr, and appends `[cwd: ~/private | shared: ~/shared]`.
pub struct JailedTerminalBackend {
    jail: VirtualWorkspaceJail,
    inner: Arc<dyn TerminalBackend>,
}

impl JailedTerminalBackend {
    pub fn new(jail: VirtualWorkspaceJail, inner: Arc<dyn TerminalBackend>) -> Self {
        Self { jail, inner }
    }

    fn rewrite_command_virtual_paths(&self, cmd: &str) -> String {
        let priv_s = self.jail.host_private_dir.to_string_lossy();
        let shared_s = self.jail.host_shared_dir.to_string_lossy();
        cmd.replace("~/shared", shared_s.as_ref())
            .replace("~/private", priv_s.as_ref())
    }

    fn prepare_request(&self, mut req: TerminalRunRequest) -> Result<TerminalRunRequest, ComputerError> {
        let resolved_cwd = self
            .jail
            .resolve_path(&req.working_directory)
            .unwrap_or_else(|_| self.jail.host_private_dir.clone());
        req.working_directory = resolved_cwd;
        req.env = self.jail.sanitized_env(&req.env);
        if req.display_command.is_none() {
            req.display_command = Some(req.command.clone());
        }
        req.command = self.rewrite_command_virtual_paths(&req.command);
        if !req.output_file.starts_with(&self.jail.host_private_dir)
            && !req.output_file.starts_with(&self.jail.host_shared_dir)
        {
            let fname = req
                .output_file
                .file_name()
                .map(|n| n.to_os_string())
                .unwrap_or_else(|| std::ffi::OsString::from("bash_out.log"));
            req.output_file = self.jail.host_tmp_dir.join(fname);
        }
        Ok(req)
    }
}

#[async_trait::async_trait]
impl TerminalBackend for JailedTerminalBackend {
    async fn run(&self, request: TerminalRunRequest) -> Result<TerminalRunResult, ComputerError> {
        let virtual_cwd = self.jail.to_virtual_display(&request.working_directory);
        let prepared = self.prepare_request(request)?;
        let mut res = self.inner.run(prepared).await?;
        let scrubbed = self.jail.scrub_host_paths(&res.combined_output);
        let sep = if scrubbed.is_empty() || scrubbed.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        res.combined_output = format!(
            "{scrubbed}{sep}[cwd: {virtual_cwd} | shared: ~/shared]"
        );
        Ok(res)
    }

    async fn run_background(
        &self,
        request: TerminalRunRequest,
    ) -> Result<BackgroundHandle, ComputerError> {
        let prepared = self.prepare_request(request)?;
        self.inner.run_background(prepared).await
    }

    async fn get_task(&self, task_id: &str) -> Option<TaskSnapshot> {
        let mut snap = self.inner.get_task(task_id).await?;
        snap.output = self.jail.scrub_host_paths(&snap.output);
        snap.cwd = self.jail.to_virtual_display(Path::new(&snap.cwd));
        Some(snap)
    }

    async fn kill_task(&self, task_id: &str) -> KillOutcome {
        self.inner.kill_task(task_id).await
    }

    async fn kill_task_with_source(&self, task_id: &str, source: KillSource) -> KillOutcome {
        self.inner.kill_task_with_source(task_id, source).await
    }

    async fn kill_foreground_commands(&self) {
        self.inner.kill_foreground_commands().await
    }

    async fn kill_foreground_commands_by_owner(&self, owner_session_id: &str) {
        self.inner
            .kill_foreground_commands_by_owner(owner_session_id)
            .await
    }

    async fn kill_all_background_tasks(&self) {
        self.inner.kill_all_background_tasks().await
    }

    async fn kill_all_background_tasks_by_owner(&self, owner_session_id: &str) {
        self.inner
            .kill_all_background_tasks_by_owner(owner_session_id)
            .await
    }

    async fn warm_shell(&self, cwd: &Path) {
        let safe = self
            .jail
            .resolve_path(cwd)
            .unwrap_or_else(|_| self.jail.host_private_dir.clone());
        self.inner.warm_shell(&safe).await
    }

    async fn reparent_notifications(
        &self,
        old_owner_session_id: &str,
        new_owner_session_id: &str,
        new_handle: ToolNotificationHandle,
        backend_weak: std::sync::Weak<dyn TerminalBackend>,
    ) {
        self.inner
            .reparent_notifications(
                old_owner_session_id,
                new_owner_session_id,
                new_handle,
                backend_weak,
            )
            .await
    }

    async fn background_foreground_command(&self, tool_call_id: &str) -> bool {
        self.inner.background_foreground_command(tool_call_id).await
    }

    async fn background_foreground_commands(
        &self,
        owner_session_id: Option<&str>,
    ) -> Vec<BackgroundedForeground> {
        self.inner
            .background_foreground_commands(owner_session_id)
            .await
    }

    async fn wait_for_completion(
        &self,
        task_id: &str,
        timeout: Option<Duration>,
    ) -> Option<TaskSnapshot> {
        let mut snap = self.inner.wait_for_completion(task_id, timeout).await?;
        snap.output = self.jail.scrub_host_paths(&snap.output);
        snap.cwd = self.jail.to_virtual_display(Path::new(&snap.cwd));
        Some(snap)
    }

    async fn list_tasks(&self) -> Vec<TaskSnapshot> {
        let mut tasks = self.inner.list_tasks().await;
        for t in &mut tasks {
            t.output = self.jail.scrub_host_paths(&t.output);
            t.cwd = self.jail.to_virtual_display(Path::new(&t.cwd));
        }
        tasks
    }

    async fn get_shell_cwd(&self) -> Option<PathBuf> {
        self.inner.get_shell_cwd().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn virtual_jail_maps_private_and_shared_and_blocks_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let priv_dir = tmp.path().join("users/u1/private");
        let shared_dir = tmp.path().join("shared");
        let other_user = tmp.path().join("users/u2/private");
        std::fs::create_dir_all(&other_user).unwrap();
        std::fs::write(other_user.join("diary.md"), "secret").unwrap();

        let jail = VirtualWorkspaceJail::new(&priv_dir, &shared_dir).unwrap();
        let fs = JailedFs::new(jail.clone());

        fs.write_file(Path::new("~/private/note.md"), b"hello private")
            .await
            .unwrap();
        fs.write_file(Path::new("~/shared/menu.md"), b"tomato beef")
            .await
            .unwrap();

        assert_eq!(
            fs.read_file(Path::new("note.md")).await.unwrap(),
            b"hello private"
        );
        assert_eq!(
            fs.read_file(Path::new("/shared/menu.md")).await.unwrap(),
            b"tomato beef"
        );

        // Traversal into sibling user's private directory MUST fail with PermissionDenied
        let err = fs
            .read_file(Path::new("~/private/../../u2/private/diary.md"))
            .await
            .unwrap_err();
        assert_eq!(err.io_error_kind(), Some(io::ErrorKind::PermissionDenied));

        // Env sanitizer strips secrets
        let mut raw_env = HashMap::new();
        raw_env.insert("JWT_SECRET".to_string(), "super-secret".to_string());
        raw_env.insert("OPENAI_API_KEY".to_string(), "sk-123".to_string());
        raw_env.insert("USER_LOCALE".to_string(), "zh_CN".to_string());
        let clean = jail.sanitized_env(&raw_env);
        assert!(!clean.contains_key("JWT_SECRET"));
        assert!(!clean.contains_key("OPENAI_API_KEY"));
        assert_eq!(clean.get("USER_LOCALE").map(String::as_str), Some("zh_CN"));
    }
}
