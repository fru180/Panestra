import { describe, expect, it } from "vitest";

import type { ScreenSnapshot } from "./types";
import {
  collectGlyphSpans,
  glyphTextureBounds,
  layoutGlyphAtlas,
  type PaneRenderModel,
} from "./webgpu-renderer";

function paneWithCells(
  styledCells: ScreenSnapshot["styledCells"],
): PaneRenderModel {
  return {
    id: "terminal-1",
    snapshot: { styledCells } as ScreenSnapshot,
    x: 0,
    y: 0,
    width: 120,
    height: 120,
    active: false,
  };
}

describe("WebGPU glyph atlas", () => {
  it("uses the PTY cell width for styled full-width characters", () => {
    const spans = collectGlyphSpans([
      paneWithCells([
        [0, 0, "A", 1, null, null, 0],
        [0, 1, "日", 2, null, null, 0],
        [0, 2, "", 0, null, null, 0],
        [0, 3, "。", 2, null, null, 0],
      ]),
    ]);

    expect(spans.get("A")).toBe(1);
    expect(spans.get("日")).toBe(2);
    expect(spans.get("。")).toBe(2);
  });

  it("reserves adjacent atlas columns for full-width glyphs", () => {
    const { glyphs, rows } = layoutGlyphAtlas(
      new Map([
        ["A", 1],
        ["日", 2],
        ["B", 1],
      ]),
    );

    expect(glyphs.get("A")).toEqual([0, 0, 1]);
    expect(glyphs.get("日")).toEqual([1, 0, 2]);
    expect(glyphs.get("B")).toEqual([3, 0, 1]);
    expect(rows).toBe(1);
  });

  it("moves a full-width glyph to the next row instead of clipping it", () => {
    const characters = new Map<string, number>();
    for (let index = 0; index < 31; index += 1)
      characters.set(String(index), 1);
    characters.set("日", 2);

    const { glyphs, rows } = layoutGlyphAtlas(characters);

    expect(glyphs.get("日")).toEqual([0, 1, 2]);
    expect(rows).toBe(2);
  });

  it("samples both atlas columns for a full-width glyph", () => {
    const bounds = glyphTextureBounds([3, 1, 2], 4);

    expect(bounds).toEqual({
      u0: 3 / 32,
      u1: 5 / 32,
      v0: 1 / 4,
      v1: 2 / 4,
    });
  });

  it("keeps the atlas within the existing texture height limit", () => {
    const characters = new Map<string, number>();
    for (let index = 0; index < 4096; index += 1)
      characters.set(String(index), 2);

    const { glyphs, rows } = layoutGlyphAtlas(characters);

    expect(glyphs.size).toBe(2048);
    expect(rows).toBe(128);
  });
});
