use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use super::AppState;

pub const PROFILE_NAME: &str = "maya";
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
pub const SHELL_COMMAND: &str =
    "/usr/bin/sudo -n -u '#1001' -- /usr/local/libexec/maya-aoe/maya-shell";

pub fn codex_agent_spec() -> crate::acp::AgentSpec {
    crate::acp::AgentSpec {
        command: CODEX_COMMAND.to_string(),
        args: CODEX_ARGS.iter().map(|arg| (*arg).to_string()).collect(),
        description: "Maya restricted Codex ACP bridge".to_string(),
        env_allowlist: None,
    }
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
        && instance.agent_model.is_none()
        && instance.acp_effort.is_none()
}

/// Select the paired host-shell command at the terminal mutation boundary.
/// Ordinary AoE sessions retain their existing login-shell default. The Maya
/// profile has exactly one server-owned shell identity and never consumes a
/// caller command, cwd, environment, container, or terminal-index override.
pub fn paired_shell_command(
    maya_restricted: bool,
    instance: &crate::session::Instance,
    index: u32,
) -> anyhow::Result<Option<&'static str>> {
    if !maya_restricted {
        return Ok(None);
    }
    anyhow::ensure!(index == 0, "Maya restricted shell index must be 0");
    anyhow::ensure!(
        is_restricted_session(instance),
        "Maya restricted shell session identity mismatch"
    );
    Ok(Some(SHELL_COMMAND))
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

fn terminal_query_allowed(method: &Method, path: &str, query: Option<&str>) -> bool {
    let is_terminal_create =
        *method == Method::POST && session_api_suffix(path) == Some("terminal");
    let is_terminal_ws =
        *method == Method::GET && session_ws_suffix(path) == Some("terminal/live-ws");
    if !is_terminal_create && !is_terminal_ws {
        return true;
    }
    query.is_none() || query == Some("index=0")
}

pub fn route_allowed(method: &Method, path: &str, query: Option<&str>) -> bool {
    if !terminal_query_allowed(method, path, query) {
        return false;
    }
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
    if let Some(rest) = path.strip_prefix("/api/sessions/") {
        if !rest.contains('/') {
            return matches!(*method, Method::PATCH | Method::DELETE);
        }
        return match (method, session_api_suffix(path)) {
            (&Method::PATCH, Some("archive")) => true,
            (&Method::POST, Some("trash" | "restore")) => true,
            (&Method::POST, Some("terminal")) => true,
            (
                &Method::POST,
                Some(
                    "smart-rename" | "acp/spawn" | "acp/prompt" | "acp/cancel"
                    | "acp/force_end_turn",
                ),
            ) => true,
            (&Method::GET, Some("acp/replay")) => true,
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
    if !route_allowed(request.method(), path, request.uri().query()) {
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
            (Method::POST, "/api/sessions/s-1/trash"),
            (Method::POST, "/api/sessions/s-1/restore"),
            (Method::POST, "/api/sessions/s-1/terminal"),
            (Method::POST, "/api/sessions/s-1/smart-rename"),
            (Method::POST, "/api/sessions/s-1/acp/prompt"),
            (Method::POST, "/api/sessions/s-1/acp/cancel"),
            (Method::POST, "/api/sessions/s-1/acp/force_end_turn"),
            (Method::GET, "/api/sessions/s-1/acp/replay"),
            (Method::GET, "/api/sessions/s-1/artifacts/plot.png"),
            (Method::POST, "/api/sessions/s-1/acp/approvals/n-1"),
            (Method::POST, "/api/sessions/s-1/acp/elicitations/n-1"),
            (Method::GET, "/sessions/s-1/acp/ws"),
            (Method::GET, "/sessions/s-1/terminal/live-ws"),
        ] {
            assert!(
                route_allowed(&method, path, None),
                "expected allowed: {method} {path}"
            );
        }

        for (method, path) in [
            (Method::GET, "/api/agents"),
            (Method::GET, "/api/settings"),
            (Method::PATCH, "/api/settings"),
            (Method::GET, "/api/profiles"),
            (Method::GET, "/api/projects"),
            (Method::DELETE, "/api/workspaces"),
            (Method::POST, "/api/git/clone"),
            (Method::GET, "/api/mcp/servers"),
            (Method::GET, "/api/plugins"),
            (Method::POST, "/api/sessions/s-1/archive"),
            (Method::PATCH, "/api/sessions/s-1/trash"),
            (Method::DELETE, "/api/sessions/s-1/trash"),
            (Method::POST, "/api/sessions/s-1/archive/extra"),
            (Method::DELETE, "/api/sessions/s-1/delete-worktree"),
            (Method::POST, "/api/sessions/s-1/acp/switch-agent"),
            (Method::POST, "/api/sessions/s-1/acp/config-option"),
            (Method::GET, "/sessions/s-1/live-ws"),
        ] {
            assert!(
                !route_allowed(&method, path, None),
                "expected denied: {method} {path}"
            );
        }
    }

    #[test]
    fn restricted_terminal_routes_accept_only_the_fixed_index_zero_query() {
        for (method, path) in [
            (Method::POST, "/api/sessions/s-1/terminal"),
            (Method::GET, "/sessions/s-1/terminal/live-ws"),
        ] {
            assert!(route_allowed(&method, path, None));
            assert!(route_allowed(&method, path, Some("index=0")));
            for query in [
                "",
                "index=1",
                "index=-1",
                "index=zero",
                "index=0&index=0",
                "index=0&extra=1",
                "extra=1",
                "index%3D0",
            ] {
                assert!(
                    !route_allowed(&method, path, Some(query)),
                    "expected denied query: {method} {path}?{query}"
                );
            }
        }

        for (method, path) in [
            (Method::DELETE, "/api/sessions/s-1/terminal"),
            (Method::POST, "/api/sessions/s-1/container-terminal"),
            (Method::GET, "/sessions/s-1/live-ws"),
            (Method::GET, "/sessions/s-1/container-terminal/live-ws"),
        ] {
            assert!(!route_allowed(&method, path, None));
            assert!(!route_allowed(&method, path, Some("index=0")));
        }
    }

    #[test]
    fn restricted_shell_selection_is_fixed_and_ordinary_aoe_is_unchanged() {
        let mut restricted = crate::session::Instance::new("Maya", PROJECT_PATH);
        restricted.tool = "codex".to_string();
        restricted.view = crate::session::View::Structured;
        restricted.source_profile = PROFILE_NAME.to_string();

        assert_eq!(
            paired_shell_command(true, &restricted, 0).unwrap(),
            Some(SHELL_COMMAND)
        );
        assert!(paired_shell_command(true, &restricted, 1).is_err());

        let ordinary = crate::session::Instance::new("ordinary", "/tmp/project");
        assert_eq!(paired_shell_command(false, &ordinary, 0).unwrap(), None);
        assert_eq!(paired_shell_command(false, &ordinary, 7).unwrap(), None);
    }

    #[test]
    fn fixed_shell_selection_is_wired_into_create_and_respawn_entrypoints() {
        let create = include_str!("api/sessions.rs");
        let create_start = create.find("pub async fn ensure_terminal").unwrap();
        let create_end = create[create_start..]
            .find("pub async fn ensure_container_terminal")
            .map(|offset| create_start + offset)
            .unwrap();
        let create_entry = &create[create_start..create_end];
        assert!(create_entry.contains("maya_restricted::paired_shell_command"));
        assert!(create_entry.contains("start_terminal_with_size_indexed_command"));

        let pane = include_str!("pane.rs");
        let respawn_start = pane
            .find("pub(crate) async fn respawn_paired_if_dead")
            .unwrap();
        let respawn_end = pane[respawn_start..]
            .find("pub(crate) async fn respawn_container_if_dead")
            .map(|offset| respawn_start + offset)
            .unwrap();
        let respawn_entry = &pane[respawn_start..respawn_end];
        assert!(respawn_entry.contains("maya_restricted::paired_shell_command"));
        assert!(respawn_entry.contains("start_terminal_with_size_indexed_command"));
    }

    #[test]
    fn lifecycle_routes_bind_the_identity_guard_to_the_exact_session() {
        for path in [
            "/api/sessions/s-1",
            "/api/sessions/s-1/archive",
            "/api/sessions/s-1/trash",
            "/api/sessions/s-1/restore",
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
    fn shell_command_is_the_exact_root_owned_wrapper_command() {
        assert_eq!(
            SHELL_COMMAND,
            "/usr/bin/sudo -n -u '#1001' -- /usr/local/libexec/maya-aoe/maya-shell"
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_command_quotes_numeric_user_through_the_real_shell_parser() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let fixture = tempfile::tempdir().unwrap();
        let capture = fixture.path().join("capture-argv.sh");
        let output = fixture.path().join("argv.txt");
        std::fs::write(
            &capture,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$MAYA_SHELL_ARGV_CAPTURE\"\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&capture).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&capture, permissions).unwrap();

        let fixture_command = SHELL_COMMAND.replacen(
            "/usr/bin/sudo",
            capture.to_str().expect("fixture path is UTF-8"),
            1,
        );
        let status = Command::new("/bin/sh")
            .args(["-c", &fixture_command])
            .env("MAYA_SHELL_ARGV_CAPTURE", &output)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(output)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "-n",
                "-u",
                "#1001",
                "--",
                "/usr/local/libexec/maya-aoe/maya-shell"
            ]
        );
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
