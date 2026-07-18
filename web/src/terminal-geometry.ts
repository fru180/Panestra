export type TerminalDisplaySize = "small" | "medium" | "large";

// The glyph atlas is rendered at a higher resolution, but every owning view
// presents one logical cell at this fixed size.
export const CELL_WIDTH = 6;
export const CELL_HEIGHT = 12;
export const ATLAS_CELL_WIDTH = 20;
export const ATLAS_CELL_HEIGHT = 40;
export const PANE_HEADER_HEIGHT = 44;
export const PANE_GAP = 10;
export const MIN_COLS = 40;
export const MAX_COLS = 300;
export const MIN_ROWS = 12;
export const MAX_ROWS = 120;

const PANE_BODIES: Record<
  TerminalDisplaySize,
  { width: number; height: number }
> = {
  small: { width: 384, height: 230.4 },
  medium: { width: 540, height: 324 },
  large: { width: 720, height: 432 },
};

export type PaneBounds = {
  x: number;
  y: number;
  width: number;
  height: number;
};

export type TerminalSessionSize = {
  id: string;
  cols: number;
  rows: number;
};

export type LogicalTerminalSize = {
  cols: number;
  rows: number;
  pixelWidth: number;
  pixelHeight: number;
};

export type ResponsivePane = PaneBounds & { id: string };

export type ResponsiveLayout = {
  panes: ResponsivePane[];
  width: number;
  height: number;
};

export function isTerminalDisplaySize(
  value: string | null,
): value is TerminalDisplaySize {
  return value === "small" || value === "medium" || value === "large";
}

export function terminalPaneSize(size: TerminalDisplaySize): {
  width: number;
  bodyHeight: number;
  height: number;
} {
  const body = PANE_BODIES[size];
  return {
    width: body.width,
    bodyHeight: body.height,
    height: body.height + PANE_HEADER_HEIGHT,
  };
}

export function logicalTerminalSize(
  width: number,
  height: number,
): LogicalTerminalSize {
  const pixelWidth = Math.min(65_535, Math.max(0, Math.floor(width)));
  const pixelHeight = Math.min(65_535, Math.max(0, Math.floor(height)));
  return {
    cols: clamp(Math.floor(pixelWidth / CELL_WIDTH), MIN_COLS, MAX_COLS),
    rows: clamp(Math.floor(pixelHeight / CELL_HEIGHT), MIN_ROWS, MAX_ROWS),
    pixelWidth,
    pixelHeight,
  };
}

export function logicalSizeForDisplay(
  size: TerminalDisplaySize,
): LogicalTerminalSize {
  const pane = terminalPaneSize(size);
  return logicalTerminalSize(pane.width, pane.bodyHeight);
}

export function responsivePaneLayout(
  sessions: TerminalSessionSize[],
  viewportWidth: number,
  viewportHeight: number,
  size: TerminalDisplaySize,
): ResponsiveLayout {
  if (sessions.length === 0)
    return {
      panes: [],
      width: Math.max(0, viewportWidth),
      height: Math.max(0, viewportHeight),
    };

  const availableWidth = Math.max(0, viewportWidth);
  const pane = terminalPaneSize(size);
  const panes: ResponsivePane[] = [];
  let x = 0;
  let y = 0;
  let rowHeight = 0;
  let usedWidth = 0;

  for (const session of sessions) {
    if (x > 0 && x + pane.width > availableWidth) {
      y += rowHeight + PANE_GAP;
      x = 0;
      rowHeight = 0;
    }
    panes.push({
      id: session.id,
      x,
      y: y + PANE_HEADER_HEIGHT,
      width: pane.width,
      height: pane.bodyHeight,
    });
    usedWidth = Math.max(usedWidth, x + pane.width);
    rowHeight = Math.max(rowHeight, pane.height);
    x += pane.width + PANE_GAP;
  }

  return {
    panes,
    width: Math.max(availableWidth, usedWidth),
    height: Math.max(viewportHeight, y + rowHeight),
  };
}

export function expandedPaneLayout(
  session: TerminalSessionSize | undefined,
  viewportWidth: number,
  viewportHeight: number,
): ResponsiveLayout {
  const width = Math.max(0, viewportWidth);
  const height = Math.max(0, viewportHeight);
  if (!session) return { panes: [], width, height };
  return {
    panes: [
      {
        id: session.id,
        x: 0,
        y: PANE_HEADER_HEIGHT,
        width,
        height: Math.max(0, height - PANE_HEADER_HEIGHT),
      },
    ],
    width,
    height,
  };
}

export function terminalViewport(
  pane: PaneBounds,
  cols: number,
  rows: number,
): PaneBounds & { scale: number } {
  const contentWidth = cols * CELL_WIDTH;
  const contentHeight = rows * CELL_HEIGHT;
  // Resize owners normally render at scale 1. A non-owning tab may need to
  // shrink the one shared PTY grid so the complete screen remains visible.
  const scale =
    contentWidth > 0 && contentHeight > 0
      ? Math.min(1, pane.width / contentWidth, pane.height / contentHeight)
      : 0;
  return {
    x: pane.x,
    y: pane.y,
    width: contentWidth * scale,
    height: contentHeight * scale,
    scale,
  };
}

function clamp(value: number, minimum: number, maximum: number): number {
  return Math.min(maximum, Math.max(minimum, value));
}
