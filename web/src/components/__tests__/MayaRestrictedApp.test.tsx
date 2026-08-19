// @vitest-environment jsdom

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";

import type { ServerAbout } from "../../lib/api";
import { MayaRestrictedApp, mayaRestrictedCreateRequest } from "../MayaRestrictedApp";

const createSessionMock = vi.fn();
const renameSessionMock = vi.fn();

vi.mock("../../lib/api", async () => ({
  ...(await vi.importActual<typeof import("../../lib/api")>("../../lib/api")),
  createSession: (...args: unknown[]) => createSessionMock(...args),
  renameSession: (...args: unknown[]) => renameSessionMock(...args),
}));

vi.mock("../../hooks/useSessions", () => ({
  useSessions: () => ({
    sessions: [],
    loaded: true,
    error: false,
    injectSession: vi.fn(),
    refresh: vi.fn(),
  }),
}));

const ABOUT = {
  acp_show_tool_durations: true,
  acp_replay_events: 0,
} as ServerAbout;

describe("MayaRestrictedApp", () => {
  it("projects new chats to the exact fixed server request", async () => {
    createSessionMock.mockResolvedValueOnce({ ok: false, error: "fixture stop" });
    render(
      <MemoryRouter>
        <MayaRestrictedApp about={ABOUT} loginRequired={false} onLogout={vi.fn()} />
      </MemoryRouter>,
    );

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
    render(
      <MemoryRouter>
        <MayaRestrictedApp about={ABOUT} loginRequired={false} onLogout={vi.fn()} />
      </MemoryRouter>,
    );

    expect(screen.getByText("Maya Codex")).toBeTruthy();
    expect(screen.getByRole("button", { name: "New" })).toBeTruthy();
    for (const forbidden of ["Settings", "Projects", "Plugins", "GitHub", "Worktree", "Switch agent"]) {
      expect(screen.queryByText(forbidden)).toBeNull();
    }
  });

  it("does not emit an empty title override", () => {
    expect(mayaRestrictedCreateRequest("   ")).toEqual({});
  });
});
