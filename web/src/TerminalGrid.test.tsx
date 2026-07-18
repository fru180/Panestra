import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import { TerminalGrid } from "./TerminalGrid";
import type { PanestraConnection } from "./connection";
import type { Session } from "./types";

vi.mock("./webgpu-renderer", () => ({
  TerminalRenderer: class {
    onDeviceLost?: (reason: string) => void;
    render(): void {}
  },
}));

class ResizeObserverStub {
  observe(): void {}
  disconnect(): void {}
}

vi.stubGlobal("ResizeObserver", ResizeObserverStub);

afterEach(cleanup);

const session: Session = {
  id: "session-1",
  name: "Terminal",
  command: "/bin/zsh",
  args: [],
  launchCwd: "/tmp",
  processState: "running",
  cols: 80,
  rows: 24,
  daemonEpoch: "epoch",
  sessionGeneration: "generation",
  historyEnabled: true,
  createdAt: "2026-07-18T00:00:00Z",
};

function renderGrid(
  deletingIds: ReadonlySet<string>,
  onDelete: (id: string) => void,
) {
  return render(() => (
    <TerminalGrid
      sessions={[session]}
      snapshots={new Map()}
      actionContexts={new Map()}
      gitStatuses={new Map()}
      projects={[]}
      device={{} as GPUDevice}
      connection={{} as PanestraConnection}
      displaySize="medium"
      leaseOwned={false}
      resizeLeaseIds={new Set()}
      deletingIds={deletingIds}
      onActivate={() => {}}
      onDelete={onDelete}
      onToggleExpanded={() => {}}
      onBrowseHistory={() => {}}
      onDeviceLost={() => {}}
    />
  ));
}

describe("TerminalGrid terminal deletion", () => {
  it("requests deletion from the pane header", () => {
    const onDelete = vi.fn();
    renderGrid(new Set(), onDelete);

    fireEvent.click(screen.getByRole("button", { name: "Terminalを削除" }));

    expect(onDelete).toHaveBeenCalledWith("session-1");
  });

  it("disables duplicate deletion while the request is running", () => {
    const onDelete = vi.fn();
    renderGrid(new Set(["session-1"]), onDelete);

    const button = screen.getByRole("button", { name: "Terminalを削除" });
    expect((button as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(button);
    expect(onDelete).not.toHaveBeenCalled();
  });
});
