// @vitest-environment jsdom

import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import {
  HistoryRestoreIndicator,
  shouldRenderWorkingSpinner,
  shouldShowHistoryRestoreIndicator,
} from "./StructuredView";

afterEach(cleanup);

describe("chat history restore indicator", () => {
  it("shows while historical chunks are active before replay settlement", () => {
    const restoring = shouldShowHistoryRestoreIndicator({ importPending: true, lastSeq: 17, turnActive: true });
    expect(restoring).toBe(true);
    expect(
      shouldRenderWorkingSpinner({ restoringChatHistory: restoring, pendingApprovals: 0, pendingElicitations: 0 }),
    ).toBe(false);
    render(<HistoryRestoreIndicator />);
    expect(screen.getByRole("status").textContent).toBe("Restoring chat history…");
    expect(screen.queryByText(/Waiting on model/i)).toBeNull();
  });

  it("clears at replay settlement and allows the normal idle path", () => {
    const restoring = shouldShowHistoryRestoreIndicator({ importPending: true, lastSeq: 42, turnActive: false });
    expect(restoring).toBe(false);
    expect(
      shouldRenderWorkingSpinner({ restoringChatHistory: restoring, pendingApprovals: 0, pendingElicitations: 0 }),
    ).toBe(true);
  });
});
