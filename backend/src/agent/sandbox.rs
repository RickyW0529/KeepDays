//! Virtual Workspace & Real `agent-kernel` (`xai-grok-agent` + `xai-grok-tools`) Session Bridge.
//!
//! Wires `KeepDays` speaker-isolated workspaces (`spaces/{space_id}/users/{uid}` + `spaces/{space_id}/shared`)
//! directly into `agent-kernel`'s:
//! - [`VirtualWorkspaceJail`] (`~/private` + `~/shared` path virtualization + secret env stripping)
//! - [`JailedFs`] (`AsyncFileSystem` backing `GrokBuild:read_file`, `GrokBuild:search_replace`, etc.)
//! - [`JailedTerminalBackend`] + [`JailedSandboxLaunch`] (Seatbelt / seccomp + `LocalTerminalBackend` backing `GrokBuild:bash`)
//! - [`xai_grok_agent::AgentBuilder`] -> [`xai_grok_tools::bridge::ToolBridge`] (`Mode A` native tools + `GrokBuild` core tools)

use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use xai_grok_agent::{Agent, AgentBuilder};
use xai_grok_tools::{
    bridge::ToolBridge,
    computer::local::LocalTerminalBackend,
    notification::ToolNotificationHandle,
    virtual_jail::{JailedFs, JailedSandboxLaunch, JailedTerminalBackend, VirtualWorkspaceJail},
};

#[derive(Clone)]
pub struct VirtualFs {
    jail: VirtualWorkspaceJail,
    kernel_agent: Arc<Agent>,
}

impl std::fmt::Debug for VirtualFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualFs")
            .field("private_root", &self.jail.host_private_dir())
            .field("shared_root", &self.jail.host_shared_dir())
            .finish()
    }
}

impl VirtualFs {
    /// Initialize the speaker's isolated virtual workspace (`~/private` + `~/shared`) AND build
    /// a live `agent-kernel` [`Agent`] + [`ToolBridge`] confined strictly to those directories.
    pub async fn ensure_for(base_dir: &str, space_id: i64, user_id: i64) -> Result<Self, String> {
        let base = PathBuf::from(base_dir)
            .join("spaces")
            .join(space_id.to_string());
        let private_root = base.join("users").join(user_id.to_string());
        let shared_root = base.join("shared");

        let jail = VirtualWorkspaceJail::new(&private_root, &shared_root)
            .map_err(|e| format!("初始化沙箱工作区失败: {e}"))?;

        let launch_hook = Arc::new(JailedSandboxLaunch::new(jail.clone()));
        let raw_terminal = Arc::new(LocalTerminalBackend::new_with_sandbox_launch(launch_hook));
        let jailed_terminal = Arc::new(JailedTerminalBackend::new(jail.clone(), raw_terminal));
        let jailed_fs = Arc::new(JailedFs::new(jail.clone()));

        let state_file = jail.host_tmp_dir().join("tool_resources_state.json");
        let agent = AgentBuilder::new(
            jail.host_private_dir().to_path_buf(),
            jailed_terminal,
            ToolNotificationHandle::noop(),
        )
        .with_fs(jailed_fs)
        .with_state_path(state_file)
        .with_agents_md(false)
        .with_is_non_interactive(true)
        .with_owner_session_id(format!("kd_{space_id}_{user_id}"))
        .build()
        .await
        .map_err(|e| format!("构建 agent-kernel ToolBridge 失败: {e}"))?;

        Ok(Self {
            jail,
            kernel_agent: Arc::new(agent),
        })
    }

    pub fn private_root(&self) -> &Path {
        self.jail.host_private_dir()
    }

    pub fn shared_root(&self) -> &Path {
        self.jail.host_shared_dir()
    }

    pub fn jail(&self) -> &VirtualWorkspaceJail {
        &self.jail
    }

    pub fn tool_bridge(&self) -> &Arc<ToolBridge> {
        self.kernel_agent.tool_bridge()
    }

    /// Resolves a model-supplied virtual path (`"notes.md"`, `"~/private/a.txt"`, `"~/shared/plan.md"`)
    /// strictly inside either `private_root` or `shared_root`, blocking `..`, `.tmp`, and symlink escapes.
    pub fn resolve_virtual_path(&self, raw: &str) -> Result<(PathBuf, String), String> {
        let trimmed = raw.trim();
        let (target_root, prefix_label, rel_str) = if trimmed.is_empty()
            || trimmed == "."
            || trimmed == "~"
            || trimmed == "~/private"
            || trimmed == "/private"
            || trimmed == "private"
        {
            (self.private_root(), "~/private", "")
        } else if let Some(rest) = trimmed
            .strip_prefix("~/shared/")
            .or_else(|| trimmed.strip_prefix("/shared/"))
            .or_else(|| trimmed.strip_prefix("shared/"))
        {
            (self.shared_root(), "~/shared", rest)
        } else if trimmed == "~/shared" || trimmed == "/shared" || trimmed == "shared" {
            (self.shared_root(), "~/shared", "")
        } else if let Some(rest) = trimmed
            .strip_prefix("~/private/")
            .or_else(|| trimmed.strip_prefix("/private/"))
            .or_else(|| trimmed.strip_prefix("private/"))
            .or_else(|| trimmed.strip_prefix("~/"))
            .or_else(|| trimmed.strip_prefix("./"))
        {
            (self.private_root(), "~/private", rest)
        } else if trimmed.starts_with('/') {
            return Err(
                "禁止访问宿主机绝对路径；请仅使用相对路径、`~/private/...` 或 `~/shared/...`"
                    .to_string(),
            );
        } else {
            (self.private_root(), "~/private", trimmed)
        };

        let mut clean_parts: Vec<String> = Vec::new();
        for comp in Path::new(rel_str).components() {
            match comp {
                Component::CurDir => {}
                Component::Normal(os) => {
                    let s = os.to_string_lossy();
                    if s == ".tmp" && clean_parts.is_empty() {
                        return Err("内部 `.tmp` 目录不允许直接编辑".to_string());
                    }
                    clean_parts.push(s.into_owned());
                }
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(
                        "路径越界：仅允许在 `~/private` 与 `~/shared` 工作区内读写".to_string(),
                    );
                }
            }
        }

        let mut joined = target_root.to_path_buf();
        for p in &clean_parts {
            joined.push(p);
        }

        // Verify via kernel `VirtualWorkspaceJail::resolve_path` as second line of defense (catches symlinks!)
        let resolved = self
            .jail
            .resolve_path(&joined)
            .map_err(|e| self.scrub_output(&e.to_string()))?;

        let virtual_display = if clean_parts.is_empty() {
            prefix_label.to_string()
        } else {
            format!("{}/{}", prefix_label, clean_parts.join("/"))
        };

        Ok((resolved, virtual_display))
    }

    pub fn scrub_output(&self, raw: &str) -> String {
        self.jail.scrub_host_paths(raw)
    }

    /// Invoke any tool registered on `agent-kernel`'s [`ToolBridge`] (`GrokBuild:run_terminal_cmd`, `GrokBuild:read_file`,
    /// `GrokBuild:search_replace`, `GrokBuild:grep`, `GrokBuild:list_dir`, or custom Mode A tools)
    /// and return its scrubbed prompt text.
    pub async fn call_kernel_tool(
        &self,
        registry_id_or_name: &str,
        params: Value,
        call_id: &str,
    ) -> Result<String, String> {
        use xai_grok_tools::types::tool::ToolKind;
        let bridge = self.kernel_agent.tool_bridge();
        let kind_hint = match registry_id_or_name {
            "GrokBuild:run_terminal_cmd" | "GrokBuild:bash" | "run_terminal_command" => {
                Some(ToolKind::Execute)
            }
            "GrokBuild:read_file" => Some(ToolKind::Read),
            "GrokBuild:search_replace" => Some(ToolKind::Edit),
            "GrokBuild:grep" => Some(ToolKind::Search),
            "GrokBuild:list_dir" => Some(ToolKind::ListDir),
            _ => None,
        };
        let client_name = if let Some(kind) = kind_hint {
            if let Some(resolved) = bridge.tool_for_kind(kind).await {
                resolved
            } else {
                bridge
                    .tool_for_registry_id(registry_id_or_name)
                    .unwrap_or_else(|| registry_id_or_name.to_string())
            }
        } else {
            bridge
                .tool_for_registry_id(registry_id_or_name)
                .unwrap_or_else(|| registry_id_or_name.to_string())
        };

        let res = bridge
            .call(&client_name, params, call_id)
            .await
            .map_err(|e| self.scrub_output(&e.to_string()))?;

        Ok(self.scrub_output(&res.prompt_text))
    }

    /// Execute a sandboxed shell command through `agent-kernel`'s real `GrokBuild:run_terminal_cmd` tool
    /// (`LocalTerminalBackend` + `JailedTerminalBackend` + `JailedSandboxLaunch`).
    pub async fn run_bash(&self, script: &str) -> Result<String, String> {
        let trimmed = script.trim();
        if trimmed.is_empty() {
            return Err("命令不能为空".to_string());
        }
        let mut out = self
            .call_kernel_tool(
                "GrokBuild:run_terminal_cmd",
                json!({
                    "command": trimmed,
                    "description": "Execute command in virtual workspace",
                }),
                "kd_bash_call",
            )
            .await?;
        if !out.contains("[cwd: ~/private | shared: ~/shared]") {
            out.push_str("\n[cwd: ~/private | shared: ~/shared]");
        }
        Ok(out)
    }
}
