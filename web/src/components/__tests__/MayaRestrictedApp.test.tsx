// @vitest-environment jsdom

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ServerAbout } from "../../lib/api";
import type { SessionResponse } from "../../lib/types";
import { MayaRestrictedApp, mayaRestrictedCreateRequest } from "../MayaRestrictedApp";

const createSessionMock = vi.fn();
const renameSessionMock = vi.fn();
const applySessionMock = vi.fn();
const refreshMock = vi.fn();
const fetchMock = vi.fn();
let sessionsFixture: SessionResponse[] = [];

vi.mock("../../lib/api", async () => ({
  ...(await vi.importActual<typeof import("../../lib/api")>("../../lib/api")),
  createSession: (...args: unknown[]) => createSessionMock(...args),
  renameSession: (...args: unknown[]) => renameSessionMock(...args),
}));

vi.mock("../../hooks/useSessions", () => ({
  useSessions: () => ({
    sessions: sessionsFixture,
    loaded: true,
    error: false,
    injectSession: vi.fn(),
    refresh: refreshMock,
    applySession: applySessionMock,
  }),
}));

vi.mock("../acp/StructuredView", () => ({
  StructuredView: ({
    archivedAt,
    trashedAt,
    onRestore,
    restricted,
  }: {
    archivedAt: string | null;
    trashedAt: string | null;
    onRestore?: () => Promise<boolean> | void;
    restricted?: boolean;
  }) => (
    <div
      data-testid="structured-view"
      data-archived-at={archivedAt ?? ""}
      data-trashed-at={trashedAt ?? ""}
      data-has-restore={onRestore ? "true" : "false"}
      data-restricted={restricted ? "true" : "false"}
    />
  ),
}));

const ABOUT = {
  acp_show_tool_durations: true,
  acp_replay_events: 0,
} as ServerAbout;

function session(overrides: Partial<SessionResponse> = {}): SessionResponse {
  return {
    id: "s-1",
    title: "Review strategy",
    project_path: "/home/aaiyer/maya/maya-main",
    tool: "codex",
    status: "Idle",
    archived_at: null,
    trashed_at: null,
    acp_worker_state: "running",
    acp_agent: "codex",
    ...overrides,
  } as SessionResponse;
}

function renderApp(path = "/") {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <MayaRestrictedApp about={ABOUT} loginRequired={false} onLogout={vi.fn()} />
    </MemoryRouter>,
  );
}

function jsonResponse(body: unknown, ok = true, status = ok ? 200 : 500) {
  return {
    ok,
    status,
    json: vi.fn().mockResolvedValue(body),
  };
}

describe("MayaRestrictedApp", () => {
  beforeEach(() => {
    sessionsFixture = [];
    createSessionMock.mockReset();
    renameSessionMock.mockReset();
    applySessionMock.mockReset();
    refreshMock.mockReset().mockResolvedValue(undefined);
    fetchMock.mockReset();
    vi.stubGlobal("fetch", fetchMock);
  });

  it("projects new chats to the exact fixed server request", async () => {
    createSessionMock.mockResolvedValueOnce({ ok: false, error: "fixture stop" });
    renderApp();

    fireEvent.change(screen.getByLabelText("New chat title (optional)"), {
      target: { value: "  Review the strategy  " },
    });
    fireEvent.click(screen.getByRole("button", { name: "New" }));

    await waitFor(() =>
      expect(createSessionMock).toHaveBeenCalledWith({
        title: "Review the strategy",
      }),
    );
  });

  it("omits authority controls and keeps only chat creation and selection chrome", () => {
    sessionsFixture = [session()];
    renderApp();

    expect(screen.getByText("Maya Codex")).toBeTruthy();
    expect(screen.getByRole("button", { name: "New" })).toBeTruthy();
    for (const forbidden of [
      "Settings",
      "Projects",
      "Plugins",
      "GitHub",
      "Worktree",
      "Switch agent",
      "Provider",
      "Configuration",
      "Tunnel",
      "Delete worktree",
    ]) {
      expect(screen.queryByText(forbidden)).toBeNull();
    }
    expect(screen.queryByRole("button", { name: /permanently/i })).toBeNull();
  });

  it("archives and unarchives through the exact route and applies each returned snapshot", async () => {
    sessionsFixture = [session()];
    const archived = session({ archived_at: "2026-08-20T01:00:00Z" });
    fetchMock.mockResolvedValueOnce(jsonResponse(archived));
    const first = renderApp();

    fireEvent.click(screen.getByRole("button", { name: "Archive Review strategy" }));

    await waitFor(() => expect(applySessionMock).toHaveBeenCalledWith(archived));
    expect(fetchMock).toHaveBeenCalledWith("/api/sessions/s-1/archive", {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ archived: true, kill_pane: true }),
    });

    first.unmount();
    sessionsFixture = [archived];
    const unarchived = session();
    fetchMock.mockResolvedValueOnce(jsonResponse(unarchived));
    renderApp();

    fireEvent.click(screen.getByRole("button", { name: "Unarchive Review strategy" }));

    await waitFor(() => expect(applySessionMock).toHaveBeenLastCalledWith(unarchived));
    expect(fetchMock).toHaveBeenLastCalledWith("/api/sessions/s-1/archive", {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ archived: false, kill_pane: true }),
    });
  });

  it("moves a chat to trash and restores it by applying server snapshots", async () => {
    sessionsFixture = [session()];
    const trashed = session({ trashed_at: "2026-08-20T02:00:00Z" });
    fetchMock.mockResolvedValueOnce(jsonResponse(trashed));
    const first = renderApp();

    fireEvent.click(screen.getByRole("button", { name: "Move Review strategy to trash" }));

    await waitFor(() => expect(applySessionMock).toHaveBeenCalledWith(trashed));
    expect(fetchMock).toHaveBeenCalledWith("/api/sessions/s-1/trash", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ kill_pane: true }),
    });

    first.unmount();
    sessionsFixture = [trashed];
    const restored = session();
    fetchMock.mockResolvedValueOnce(jsonResponse(restored));
    renderApp();

    fireEvent.click(screen.getByRole("button", { name: "Restore Review strategy" }));

    await waitFor(() => expect(applySessionMock).toHaveBeenLastCalledWith(restored));
    expect(fetchMock).toHaveBeenLastCalledWith("/api/sessions/s-1/restore", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
    });
  });

  it("permits permanent purge only for trashed chats and only after confirmation", async () => {
    sessionsFixture = [session({ trashed_at: "2026-08-20T02:00:00Z" })];
    const confirm = vi.spyOn(window, "confirm").mockReturnValueOnce(false).mockReturnValueOnce(true);
    fetchMock.mockResolvedValueOnce(jsonResponse({ status: "deleted", messages: [] }));
    renderApp("/session/s-1");

    const purge = screen.getByRole("button", { name: "Delete Review strategy permanently" });
    fireEvent.click(purge);
    expect(confirm).toHaveBeenCalledWith('Permanently delete "Review strategy"? This cannot be undone.');
    expect(fetchMock).not.toHaveBeenCalled();

    fireEvent.click(purge);

    await waitFor(() => expect(refreshMock).toHaveBeenCalledTimes(1));
    expect(fetchMock).toHaveBeenCalledWith("/api/sessions/s-1", {
      method: "DELETE",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        delete_worktree: false,
        delete_branch: false,
        delete_sandbox: false,
        force_delete: false,
        keep_scratch: false,
      }),
    });
  });

  it("passes the real archive and trash timestamps to the restricted structured view", async () => {
    sessionsFixture = [
      session({
        archived_at: "2026-08-19T03:00:00Z",
        trashed_at: "2026-08-20T04:00:00Z",
      }),
    ];
    renderApp("/session/s-1");

    const view = await screen.findByTestId("structured-view");
    expect(view.getAttribute("data-archived-at")).toBe("2026-08-19T03:00:00Z");
    expect(view.getAttribute("data-trashed-at")).toBe("2026-08-20T04:00:00Z");
    expect(view.getAttribute("data-has-restore")).toBe("true");
    expect(view.getAttribute("data-restricted")).toBe("true");
  });

  it("does not emit an empty title override", () => {
    expect(mayaRestrictedCreateRequest("   ")).toEqual({});
  });
});
