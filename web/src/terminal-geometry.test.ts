import { describe, expect, it } from "vitest";

import {
  ATLAS_CELL_HEIGHT,
  ATLAS_CELL_WIDTH,
  CELL_HEIGHT,
  CELL_WIDTH,
  PANE_GAP,
  expandedPaneLayout,
  logicalSizeForDisplay,
  logicalTerminalSize,
  responsivePaneLayout,
  terminalPaneSize,
  terminalViewport,
} from "./terminal-geometry";

describe("terminal geometry", () => {
  const session = (id: string, cols = 80, rows = 24) => ({ id, cols, rows });

  it("keeps physical pane presets independent from PTY dimensions", () => {
    expect(terminalPaneSize("small")).toEqual({
      width: 384,
      bodyHeight: 230.4,
      height: 274.4,
    });
    expect(terminalPaneSize("medium")).toEqual({
      width: 540,
      bodyHeight: 324,
      height: 368,
    });
    expect(terminalPaneSize("large")).toEqual({
      width: 720,
      bodyHeight: 432,
      height: 476,
    });
  });

  it("derives the expected logical size from each preset", () => {
    expect(logicalSizeForDisplay("small")).toMatchObject({
      cols: 64,
      rows: 19,
    });
    expect(logicalSizeForDisplay("medium")).toMatchObject({
      cols: 90,
      rows: 27,
    });
    expect(logicalSizeForDisplay("large")).toMatchObject({
      cols: 120,
      rows: 36,
    });
  });

  it("floors partial cells and clamps expanded dimensions", () => {
    expect(logicalTerminalSize(601, 251)).toEqual({
      cols: 100,
      rows: 20,
      pixelWidth: 601,
      pixelHeight: 251,
    });
    expect(logicalTerminalSize(1, 1)).toMatchObject({ cols: 40, rows: 12 });
    expect(logicalTerminalSize(10_000, 10_000)).toMatchObject({
      cols: 300,
      rows: 120,
    });
  });

  it("wraps according to viewport width", () => {
    const twoColumns = responsivePaneLayout(
      [session("a"), session("b"), session("c")],
      1090,
      800,
      "medium",
    );
    expect(twoColumns.panes.map(({ x, y }) => [x, y])).toEqual([
      [0, 44],
      [540 + PANE_GAP, 44],
      [0, 44 + 368 + PANE_GAP],
    ]);

    const oneColumn = responsivePaneLayout(
      [session("a"), session("b")],
      1089,
      800,
      "medium",
    );
    expect(oneColumn.panes.map(({ x }) => x)).toEqual([0, 0]);
  });

  it("derives an expanded body from the available viewport", () => {
    const layout = expandedPaneLayout(session("a"), 1000, 700);
    expect(layout.panes[0]).toEqual({
      id: "a",
      x: 0,
      y: 44,
      width: 1000,
      height: 656,
    });
  });

  it("keeps native cell size unless a non-owner must scale down", () => {
    expect(
      terminalViewport({ x: 30, y: 50, width: 1000, height: 480 }, 80, 24),
    ).toEqual({ x: 30, y: 50, width: 480, height: 288, scale: 1 });
    expect(
      terminalViewport({ x: 0, y: 0, width: 240, height: 144 }, 80, 24),
    ).toEqual({ x: 0, y: 0, width: 240, height: 144, scale: 0.5 });
  });

  it("keeps glyph atlas cells at the terminal cell aspect ratio", () => {
    expect(ATLAS_CELL_WIDTH / CELL_WIDTH).toBe(ATLAS_CELL_HEIGHT / CELL_HEIGHT);
  });
});
