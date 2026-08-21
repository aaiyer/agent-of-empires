import { expect, test } from "./helpers/mockedTest";

const session = {
  id: "maya-1",
  title: "Resume risk work",
  project_path: "/home/aaiyer/maya/maya-main",
  group_path: "",
  tool: "codex",
  status: "Idle",
  view: "structured",
  yolo_mode: false,
  created_at: "2026-08-21T00:00:00Z",
  last_accessed_at: "2026-08-21T01:00:00Z",
  idle_entered_at: null,
  last_error: null,
  branch: null,
  main_repo_path: null,
  is_sandboxed: false,
  has_managed_worktree: false,
  has_terminal: false,
  profile: "default",
  cleanup_defaults: { delete_to_trash: true },
  workspace_repos: [],
  acp_capable: true,
  acp_session_id: "codex-thread-1",
  acp_can_fork: true,
  default_name: true,
};

test("Maya reuses the normal shell while gating authority controls", async ({ page }) => {
  let createBody: unknown;
  const requests: string[] = [];
  page.on("request", (request) => requests.push(new URL(request.url()).pathname));

  await page.route("**/api/**", (route) => route.fulfill({ status: 403, json: { error: "maya_restricted" } }));
  await page.route("**/api/login/status", (route) => route.fulfill({ json: { required: false, authenticated: true } }));
  await page.route("**/api/about", (route) =>
    route.fulfill({
      json: {
        read_only: false,
        auth_mode: "none",
        behind_tunnel: false,
        profile: "main",
        cityhall_mode: false,
        maya_restricted: true,
      },
    }),
  );
  await page.route("**/api/sessions", async (route) => {
    if (route.request().method() === "POST") {
      createBody = route.request().postDataJSON();
      return route.fulfill({ json: {} });
    }
    return route.fulfill({ json: { sessions: [session], workspace_ordering: [] } });
  });
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");

  await expect(page.getByLabel("Toggle sidebar")).toBeVisible();
  const row = page.getByTestId("sidebar-session-row");
  await expect(row).toContainText("Resume risk work");
  await row.click({ button: "right" });
  await expect(page.getByTestId("sidebar-context-menu-rename")).toBeVisible();
  await expect(page.getByTestId("sidebar-context-menu-archive")).toBeVisible();
  await expect(page.getByTestId("sidebar-context-menu-delete")).toBeVisible();
  await expect(page.getByTestId("sidebar-context-menu-switch-agent")).toHaveCount(0);
  await expect(page.getByTestId("sidebar-context-menu-fork")).toHaveCount(0);
  await expect(page.getByLabel("Settings")).toHaveCount(0);

  await page.keyboard.press("Escape");
  await page.keyboard.press("n");
  const wizard = page.getByTestId("session-wizard");
  await expect(wizard).toBeVisible();
  await wizard.getByPlaceholder("Auto-generated if empty").fill("Continue old thread");
  await wizard.getByRole("button", { name: /Launch session/ }).click();
  await expect.poll(() => createBody).toEqual({ title: "Continue old thread" });

  const forbiddenBackgroundRoutes = [
    "/api/presence",
    "/api/settings",
    "/api/projects",
    "/api/theme/current",
    "/api/plugins",
    "/api/plugins/ui-state",
    "/api/system/update-status",
    "/api/telemetry/status",
    "/api/tips",
  ];
  expect(requests.filter((path) => forbiddenBackgroundRoutes.includes(path))).toEqual([]);
});
