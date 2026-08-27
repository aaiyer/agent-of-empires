use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use super::AppState;

pub use crate::session::profile_config::{
    MAYA_PROFILE_NAME as PROFILE_NAME, MAYA_TERMINAL_COMMAND as TERMINAL_COMMAND,
    MAYA_TERMINAL_HOME as TERMINAL_HOME, MAYA_TERMINAL_PATH as TERMINAL_PATH,
    MAYA_TERMINAL_SHELL as TERMINAL_SHELL,
};
pub const PROJECT_PATH: &str = "/home/aaiyer/maya/maya-main";
pub const MAGIC_DNS_HOST: &str = "maya-devbox.tail564f89.ts.net";
pub const MAGIC_DNS_ORIGIN: &str = "https://maya-devbox.tail564f89.ts.net";
pub const HOST: &str = "127.0.0.1";
pub const PORT: u16 = 3773;
pub const CODEX_COMMAND: &str = "/usr/bin/sudo";
pub const CODEX_ARGS: &[&str] = &[
    "-n",
    "-u",
    "#1001",
    "--",
    "/usr/local/libexec/maya-aoe/maya-codex-acp",
];

pub fn codex_agent_spec() -> crate::acp::AgentSpec {
    crate::acp::AgentSpec {
        command: CODEX_COMMAND.to_string(),
        args: CODEX_ARGS.iter().map(|arg| (*arg).to_string()).collect(),
        description: "Maya restricted Codex ACP bridge".to_string(),
        env_allowlist: None,
    }
}

pub fn bind_codex_agent_spec(
    spec: &mut crate::acp::AgentSpec,
    aoe_session_id: &str,
    assigned_codex_session_id: Option<&str>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        spec.command == CODEX_COMMAND
            && spec.args
                == CODEX_ARGS
                    .iter()
                    .copied()
                    .map(String::from)
                    .collect::<Vec<_>>(),
        "Maya Codex launcher does not match the built-in prefix"
    );
    anyhow::ensure!(
        is_aoe_session_id(aoe_session_id),
        "invalid AoE session identity"
    );
    if let Some(id) = assigned_codex_session_id {
        anyhow::ensure!(
            canonical_codex_session_id(id),
            "invalid assigned Codex session identity"
        );
    }

    spec.args.extend([
        "--maya-aoe-session-id".to_string(),
        aoe_session_id.to_string(),
    ]);
    if let Some(id) = assigned_codex_session_id {
        spec.args.extend([
            "--maya-assigned-codex-session-id".to_string(),
            id.to_string(),
        ]);
    }
    Ok(())
}

pub fn is_restricted_session(instance: &crate::session::Instance) -> bool {
    instance.tool == "codex"
        && instance.is_structured()
        && instance.project_path == PROJECT_PATH
        && instance.source_profile == PROFILE_NAME
        && !instance.scratch
        && instance.worktree_info.is_none()
        && instance.workspace_info.is_none()
        && instance.agent_name.is_none()
        && instance.agent_model.as_deref().is_none_or(is_managed_model)
        && instance.acp_mode_id.as_deref().is_none_or(is_managed_mode)
        && instance.acp_effort.as_deref().is_none_or(is_managed_effort)
}

pub fn first_turn_title(prompt: &str) -> String {
    let normalized = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title: String = normalized.chars().take(60).collect();
    if normalized.chars().count() > 60 {
        if let Some(last_space) = title.rfind(' ') {
            title.truncate(last_space);
        }
        title.push('…');
    }
    if title.is_empty() {
        "Codex Chat".to_string()
    } else {
        title
    }
}

fn is_aoe_session_id(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

pub(crate) fn is_managed_model(value: &str) -> bool {
    matches!(
        value,
        "gpt-5.6-sol"
            | "gpt-5.6-terra"
            | "gpt-5.6-luna"
            | "gpt-5.5"
            | "gpt-5.4"
            | "gpt-5.4-mini"
            | "gpt-5.2"
            | "codex-auto-review"
    )
}

pub(crate) fn is_managed_mode(value: &str) -> bool {
    matches!(value, "read-only" | "agent" | "agent-full-access")
}

pub(crate) fn is_managed_effort(value: &str) -> bool {
    matches!(value, "low" | "medium" | "high" | "xhigh" | "max" | "ultra")
}

fn canonical_codex_session_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|parsed| parsed.to_string() == value)
}

pub async fn apply_first_turn_name(
    state: &Arc<AppState>,
    session_id: &str,
    prompt: &str,
) -> anyhow::Result<bool> {
    let title = first_turn_title(prompt);
    let (profile, file_watch) = {
        let instances = state.instances.read().await;
        let Some(instance) = instances.iter().find(|instance| instance.id == session_id) else {
            return Ok(false);
        };
        if !is_restricted_session(instance)
            || !crate::session::civilizations::is_default_civ_name(&instance.title)
        {
            return Ok(false);
        }
        (instance.source_profile.clone(), state.file_watch.clone())
    };

    let id = session_id.to_string();
    let title_for_disk = title.clone();
    let changed = tokio::task::spawn_blocking(move || {
        let storage = crate::session::Storage::new(&profile, file_watch)?;
        storage.update(|instances, _groups| {
            let Some(instance) = instances.iter_mut().find(|instance| instance.id == id) else {
                return Ok(false);
            };
            if !is_restricted_session(instance)
                || !crate::session::civilizations::is_default_civ_name(&instance.title)
            {
                return Ok(false);
            }
            instance.title = title_for_disk;
            Ok(true)
        })
    })
    .await??;

    if changed {
        let mut instances = state.instances.write().await;
        if let Some(instance) = instances
            .iter_mut()
            .find(|instance| instance.id == session_id)
        {
            if is_restricted_session(instance)
                && crate::session::civilizations::is_default_civ_name(&instance.title)
            {
                instance.title = title;
            }
        }
    }
    Ok(changed)
}

fn session_api_suffix(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/sessions/")?;
    let (_, suffix) = rest.split_once('/')?;
    Some(suffix)
}

fn session_ws_suffix(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/sessions/")?;
    let (_, suffix) = rest.split_once('/')?;
    Some(suffix)
}

fn restricted_session_id(path: &str) -> Option<&str> {
    let rest = path
        .strip_prefix("/api/sessions/")
        .or_else(|| path.strip_prefix("/sessions/"))?;
    let id = rest.split('/').next()?;
    (!id.is_empty()).then_some(id)
}

pub fn route_allowed(method: &Method, path: &str) -> bool {
    if path == "/api/about" && *method == Method::GET {
        return true;
    }
    if path == "/api/login/status" && *method == Method::GET {
        return true;
    }
    if matches!(path, "/api/login" | "/api/logout") && *method == Method::POST {
        return true;
    }
    if path == "/api/sessions" {
        return matches!(*method, Method::GET | Method::POST);
    }
    if path == "/api/workspace-ordering" && *method == Method::PUT {
        return true;
    }
    if matches!(
        path,
        "/api/presence" | "/api/tips/show" | "/api/app-state/tip-seen"
    ) && *method == Method::POST
    {
        return true;
    }
    if path == "/api/app-state/web-ui-state" {
        return matches!(*method, Method::GET | Method::PATCH);
    }
    if path == "/api/tips" && *method == Method::GET {
        return true;
    }
    if path == "/api/skills" && *method == Method::GET {
        return true;
    }
    if matches!(
        path,
        "/api/settings" | "/api/settings/schema" | "/api/sounds"
    ) && *method == Method::GET
    {
        return true;
    }
    if path.starts_with("/api/sounds/file/") && *method == Method::GET {
        return true;
    }
    if path == "/api/profiles/maya/settings" && *method == Method::PATCH {
        return true;
    }
    if path == "/api/theme" && *method == Method::PATCH {
        return true;
    }
    if *method == Method::GET
        && (path == "/api/themes"
            || path == "/api/theme/current"
            || path.starts_with("/api/themes/"))
    {
        return true;
    }
    if let Some(rest) = path.strip_prefix("/api/sessions/") {
        if !rest.contains('/') {
            return matches!(*method, Method::PATCH | Method::DELETE);
        }
        return match (method, session_api_suffix(path)) {
            (
                &Method::PATCH,
                Some("archive" | "group" | "notifications" | "pin" | "color" | "snooze" | "unread"),
            ) => true,
            (&Method::POST, Some("trash" | "restore" | "summarize" | "stop" | "start")) => true,
            (
                &Method::POST,
                Some(
                    "smart-rename" | "acp/spawn" | "acp/prompt" | "acp/cancel"
                    | "acp/force_end_turn",
                ),
            ) => true,
            (&Method::GET, Some("acp/replay")) => true,
            (&Method::GET, Some("acp/files" | "acp/context-primer")) => true,
            (&Method::POST, Some("acp/mode" | "acp/config-option")) => true,
            (&Method::GET, Some("diff/files" | "diff/file" | "file")) => true,
            (&Method::POST | &Method::DELETE, Some("terminal")) => true,
            (&Method::GET | &Method::POST | &Method::DELETE, Some("queue")) => true,
            (&Method::PATCH | &Method::DELETE, Some(suffix)) if suffix.starts_with("queue/") => {
                true
            }
            (&Method::GET, Some(suffix)) if suffix.starts_with("acp/attachments/") => true,
            (&Method::GET, Some(suffix)) if suffix.starts_with("artifacts/") => true,
            (&Method::POST, Some(suffix))
                if suffix.starts_with("acp/approvals/")
                    || suffix.starts_with("acp/elicitations/") =>
            {
                true
            }
            _ => false,
        };
    }
    if let Some(suffix) = session_ws_suffix(path) {
        return *method == Method::GET && matches!(suffix, "acp/ws" | "terminal/live-ws");
    }

    // Static assets and the SPA entry are read-only. Every API and session
    // transport path must be named above; an upstream route added later fails
    // closed until the restricted profile deliberately admits it.
    *method == Method::GET && !path.starts_with("/api/") && !path.starts_with("/sessions/")
}

pub async fn enforce_routes(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !state.maya_restricted {
        return next.run(request).await;
    }
    let path = request.uri().path();
    if !route_allowed(request.method(), path) {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "maya_restricted",
                "message": "This route is disabled by the Maya restricted profile"
            })),
        )
            .into_response();
    }
    let session_id = restricted_session_id(path);
    if let Some(session_id) = session_id {
        let allowed = state
            .instances
            .read()
            .await
            .iter()
            .any(|instance| instance.id == session_id && is_restricted_session(instance));
        if !allowed {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({
                    "error": "not_found",
                    "message": "Session not found"
                })),
            )
                .into_response();
        }
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_allowlist_keeps_only_the_maya_chat_contract() {
        for (method, path) in [
            (Method::GET, "/api/sessions"),
            (Method::POST, "/api/sessions"),
            (Method::PATCH, "/api/sessions/s-1"),
            (Method::DELETE, "/api/sessions/s-1"),
            (Method::PATCH, "/api/sessions/s-1/archive"),
            (Method::PATCH, "/api/sessions/s-1/group"),
            (Method::PATCH, "/api/sessions/s-1/notifications"),
            (Method::PATCH, "/api/sessions/s-1/pin"),
            (Method::PATCH, "/api/sessions/s-1/color"),
            (Method::PATCH, "/api/sessions/s-1/snooze"),
            (Method::PATCH, "/api/sessions/s-1/unread"),
            (Method::POST, "/api/sessions/s-1/trash"),
            (Method::POST, "/api/sessions/s-1/restore"),
            (Method::POST, "/api/sessions/s-1/smart-rename"),
            (Method::POST, "/api/sessions/s-1/summarize"),
            (Method::POST, "/api/sessions/s-1/stop"),
            (Method::POST, "/api/sessions/s-1/start"),
            (Method::PUT, "/api/workspace-ordering"),
            (Method::POST, "/api/sessions/s-1/acp/prompt"),
            (Method::POST, "/api/sessions/s-1/acp/cancel"),
            (Method::POST, "/api/sessions/s-1/acp/force_end_turn"),
            (Method::GET, "/api/sessions/s-1/acp/replay"),
            (Method::GET, "/api/sessions/s-1/acp/files"),
            (Method::GET, "/api/sessions/s-1/acp/context-primer"),
            (Method::POST, "/api/sessions/s-1/acp/mode"),
            (Method::POST, "/api/sessions/s-1/acp/config-option"),
            (Method::GET, "/api/sessions/s-1/diff/files"),
            (Method::GET, "/api/sessions/s-1/diff/file"),
            (Method::GET, "/api/sessions/s-1/file"),
            (Method::POST, "/api/sessions/s-1/terminal"),
            (Method::DELETE, "/api/sessions/s-1/terminal"),
            (Method::GET, "/sessions/s-1/terminal/live-ws"),
            (Method::GET, "/api/sessions/s-1/queue"),
            (Method::POST, "/api/sessions/s-1/queue"),
            (Method::DELETE, "/api/sessions/s-1/queue"),
            (Method::PATCH, "/api/sessions/s-1/queue/q-1"),
            (Method::DELETE, "/api/sessions/s-1/queue/q-1"),
            (Method::GET, "/api/sessions/s-1/artifacts/plot.png"),
            (Method::POST, "/api/sessions/s-1/acp/approvals/n-1"),
            (Method::POST, "/api/sessions/s-1/acp/elicitations/n-1"),
            (Method::GET, "/sessions/s-1/acp/ws"),
            (Method::POST, "/api/presence"),
            (Method::GET, "/api/tips"),
            (Method::GET, "/api/skills"),
            (Method::GET, "/api/settings"),
            (Method::GET, "/api/settings/schema"),
            (Method::GET, "/api/sounds"),
            (Method::GET, "/api/sounds/file/approval.wav"),
            (Method::PATCH, "/api/profiles/maya/settings"),
            (Method::POST, "/api/tips/show"),
            (Method::POST, "/api/app-state/tip-seen"),
            (Method::GET, "/api/app-state/web-ui-state"),
            (Method::PATCH, "/api/app-state/web-ui-state"),
            (Method::GET, "/api/themes"),
            (Method::GET, "/api/theme/current"),
            (Method::GET, "/api/themes/zinc"),
            (Method::PATCH, "/api/theme"),
        ] {
            assert!(
                route_allowed(&method, path),
                "expected allowed: {method} {path}"
            );
        }

        for (method, path) in [
            (Method::GET, "/api/agents"),
            (Method::GET, "/api/sessions/s-1/acp/worker-log"),
            (Method::PATCH, "/api/settings"),
            (Method::PATCH, "/api/profiles/other/settings"),
            (Method::GET, "/api/profiles"),
            (Method::GET, "/api/projects"),
            (Method::DELETE, "/api/workspaces"),
            (Method::POST, "/api/git/clone"),
            (Method::GET, "/api/mcp/servers"),
            (Method::GET, "/api/plugins"),
            (Method::POST, "/api/skills"),
            (Method::GET, "/api/skills/codex/example"),
            (Method::POST, "/api/sessions/s-1/archive"),
            (Method::PATCH, "/api/sessions/s-1/trash"),
            (Method::DELETE, "/api/sessions/s-1/trash"),
            (Method::POST, "/api/sessions/s-1/archive/extra"),
            (Method::DELETE, "/api/sessions/s-1/delete-worktree"),
            (Method::POST, "/api/sessions/s-1/acp/switch-agent"),
            (Method::GET, "/sessions/s-1/live-ws"),
        ] {
            assert!(
                !route_allowed(&method, path),
                "expected denied: {method} {path}"
            );
        }
    }

    #[test]
    fn restricted_terminal_routes_allow_only_the_stock_host_shell() {
        for (method, path) in [
            (Method::POST, "/api/sessions/s-1/container-terminal"),
            (Method::GET, "/sessions/s-1/live-ws"),
            (Method::GET, "/sessions/s-1/container-terminal/live-ws"),
        ] {
            assert!(
                !route_allowed(&method, path),
                "expected denied: {method} {path}"
            );
        }
    }

    #[test]
    fn lifecycle_routes_bind_the_identity_guard_to_the_exact_session() {
        for path in [
            "/api/sessions/s-1",
            "/api/sessions/s-1/archive",
            "/api/sessions/s-1/group",
            "/api/sessions/s-1/notifications",
            "/api/sessions/s-1/pin",
            "/api/sessions/s-1/color",
            "/api/sessions/s-1/snooze",
            "/api/sessions/s-1/unread",
            "/api/sessions/s-1/trash",
            "/api/sessions/s-1/restore",
            "/api/sessions/s-1/summarize",
            "/api/sessions/s-1/stop",
            "/api/sessions/s-1/start",
        ] {
            assert_eq!(restricted_session_id(path), Some("s-1"), "path: {path}");
        }
        assert_eq!(restricted_session_id("/api/workspaces"), None);
        assert_eq!(restricted_session_id("/api/sessions/"), None);
    }

    #[test]
    fn codex_command_is_the_exact_root_owned_wrapper_argv() {
        let spec = codex_agent_spec();
        assert_eq!(spec.command, "/usr/bin/sudo");
        assert_eq!(
            spec.args,
            [
                "-n",
                "-u",
                "#1001",
                "--",
                "/usr/local/libexec/maya-aoe/maya-codex-acp"
            ]
        );
    }

    #[test]
    fn codex_assignment_argv_is_server_derived_and_prefix_bound() {
        let mut spec = codex_agent_spec();
        bind_codex_agent_spec(
            &mut spec,
            "0123456789abcdef",
            Some("11111111-1111-4111-8111-111111111111"),
        )
        .expect("bind persisted identities");
        assert_eq!(
            spec.args,
            [
                "-n",
                "-u",
                "#1001",
                "--",
                "/usr/local/libexec/maya-aoe/maya-codex-acp",
                "--maya-aoe-session-id",
                "0123456789abcdef",
                "--maya-assigned-codex-session-id",
                "11111111-1111-4111-8111-111111111111",
            ]
        );

        let mut crafted = codex_agent_spec();
        crafted.args.push("--maya-aoe-session-id".into());
        crafted.args.push("ffffffffffffffff".into());
        assert!(
            bind_codex_agent_spec(&mut crafted, "0123456789abcdef", None).is_err(),
            "caller-crafted argv must fail before the persisted assignment is appended"
        );
    }

    #[test]
    fn managed_selectors_keep_restricted_sessions_visible() {
        let mut instance = crate::session::Instance::new("Maya", PROJECT_PATH);
        instance.tool = "codex".into();
        instance.view = crate::session::View::Structured;
        instance.source_profile = PROFILE_NAME.into();
        instance.agent_model = Some("gpt-5.6-sol".into());
        instance.acp_mode_id = Some("agent-full-access".into());
        instance.acp_effort = Some("max".into());
        assert!(is_restricted_session(&instance));

        for (model, mode, effort) in [
            (Some("other"), Some("agent"), Some("high")),
            (Some("gpt-5.6-sol"), Some("other"), Some("high")),
            (Some("gpt-5.6-sol"), Some("agent"), Some("other")),
        ] {
            instance.agent_model = model.map(str::to_owned);
            instance.acp_mode_id = mode.map(str::to_owned);
            instance.acp_effort = effort.map(str::to_owned);
            assert!(!is_restricted_session(&instance));
        }
    }

    #[test]
    fn first_turn_title_is_bounded_and_deterministic() {
        assert_eq!(
            first_turn_title("  Fix   the login race\nplease  "),
            "Fix the login race please"
        );
        let title = first_turn_title(&"word ".repeat(30));
        assert!(title.chars().count() <= 60);
        assert!(title.ends_with('…'));
    }
}
