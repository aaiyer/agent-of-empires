// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { SettingsView } from "../SettingsView";

const fetchProfiles = vi.fn(() => Promise.resolve([]));
const fetchPlugins = vi.fn(() => Promise.resolve(null));

vi.mock("../../lib/api", () => ({
  fetchProfiles: () => fetchProfiles(),
  fetchPlugins: () => fetchPlugins(),
  fetchSettings: vi.fn(() => Promise.resolve({ sound: {}, theme: {} })),
  getSettingsSchema: vi.fn(() => Promise.resolve([])),
  setDefaultProfile: vi.fn(() => Promise.resolve(true)),
  updateProfileSettings: vi.fn(() => Promise.resolve(true)),
  updateTheme: vi.fn(() => Promise.resolve(true)),
}));

afterEach(() => {
  cleanup();
  fetchProfiles.mockClear();
  fetchPlugins.mockClear();
});

describe("SettingsView Maya presentation gate", () => {
  it("shows presentation controls without loading authority-bearing providers", async () => {
    render(
      <SettingsView
        onClose={() => {}}
        tab="panels"
        onSelectTab={() => {}}
        onServerAboutRefresh={() => {}}
        mayaRestricted
      />,
    );

    await waitFor(() => expect(screen.getAllByText("Panels").length).toBeGreaterThan(0));
    for (const label of ["Theme", "Conversation display", "Sound", "Panels"]) {
      expect(screen.getAllByText(label).length).toBeGreaterThan(0);
    }
    for (const blocked of ["Profiles", "Sandbox", "Worktree", "Plugins", "Updates"]) {
      expect(screen.queryByText(blocked)).toBeNull();
    }
    expect(fetchProfiles).not.toHaveBeenCalled();
    expect(fetchPlugins).not.toHaveBeenCalled();
  });
});
