// @vitest-environment jsdom
//
// Contract test for the PanelsSettings panel (#3035). Like DiffSettings, this
// persists through useWebSettings + localStorage (key `aoe-web-settings`), not
// PATCH /api/settings. The contract is the JSON shape written to that key:
// all panes default false, and all three flip
// independently.

import { beforeEach, describe, expect, it } from "vitest";
import { fireEvent, render } from "@testing-library/react";
import { PanelsSettings } from "../PanelsSettings";

const KEY = "aoe-web-settings";

function readStored(): Record<string, unknown> {
  const raw = window.localStorage.getItem(KEY);
  return raw ? (JSON.parse(raw) as Record<string, unknown>) : {};
}

beforeEach(() => {
  window.localStorage.clear();
});

describe("PanelsSettings localStorage contract", () => {
  it("pane auto-open toggles default off", () => {
    const { container } = render(<PanelsSettings />);
    const boxes = container.querySelectorAll("input[type=checkbox]");
    expect(boxes).toHaveLength(3);
    const [diff, terminal, plugins] = Array.from(boxes) as HTMLInputElement[];
    expect(diff!.checked).toBe(false);
    expect(terminal!.checked).toBe(false);
    expect(plugins!.checked).toBe(false);
  });

  it("each toggle writes its own key independently", () => {
    const { container } = render(<PanelsSettings />);
    const [diff, terminal, plugins] = Array.from(
      container.querySelectorAll("input[type=checkbox]"),
    ) as HTMLInputElement[];

    fireEvent.click(diff!);
    expect(readStored().autoOpenDiffPane).toBe(true);
    expect(readStored().autoOpenTerminalPane).not.toBe(true);
    expect(readStored().autoOpenPluginPanes).toBe(false);

    fireEvent.click(terminal!);
    expect(readStored().autoOpenTerminalPane).toBe(true);

    fireEvent.click(plugins!);
    expect(readStored().autoOpenPluginPanes).toBe(true);

    // Flipping back restores false.
    fireEvent.click(diff!);
    expect(readStored().autoOpenDiffPane).toBe(false);
  });
});
