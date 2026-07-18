/// <reference lib="webworker" />

import { decodeServer } from "./protocol";
import type { ServerMessage } from "./protocol";
import type { ScreenSnapshot } from "./types";

const snapshots = new Map<
  string,
  {
    snapshot: ScreenSnapshot;
    chunkCount: number;
    contents: string[];
    cells: ScreenSnapshot["styledCells"][];
  }
>();
const applied = new Map<string, ScreenSnapshot>();

self.onmessage = (event: MessageEvent<ArrayBuffer>) => {
  try {
    const message = decodeServer(event.data);
    if (typeof message === "string") {
      self.postMessage({ type: "message", message });
      return;
    }
    if ("FullSnapshotBegin" in message) {
      const { snapshot, chunk_count } = message.FullSnapshotBegin;
      snapshots.set(snapshot.sessionId, {
        snapshot,
        chunkCount: chunk_count,
        contents: [],
        cells: [],
      });
      return;
    }
    if ("FullSnapshotChunk" in message) {
      const chunk = message.FullSnapshotChunk;
      const pending = snapshots.get(chunk.session_id);
      if (!pending || pending.snapshot.snapshotEpoch !== chunk.snapshot_epoch)
        return;
      pending.contents[chunk.chunk_index] = chunk.contents;
      pending.cells[chunk.chunk_index] = chunk.styled_cells;
      return;
    }
    if ("FullSnapshotEnd" in message) {
      const end = message.FullSnapshotEnd;
      const pending = snapshots.get(end.session_id);
      if (
        !pending ||
        pending.snapshot.snapshotEpoch !== end.snapshot_epoch ||
        pending.snapshot.sequenceNumber !== end.sequence_number ||
        pending.contents.length !== pending.chunkCount ||
        pending.cells.length !== pending.chunkCount
      ) {
        snapshots.delete(end.session_id);
        throw new Error("FullSnapshotのchunkが不足または世代不一致です");
      }
      pending.snapshot.contents = pending.contents.join("");
      pending.snapshot.styledCells = pending.cells.flat();
      snapshots.delete(end.session_id);
      applied.set(end.session_id, pending.snapshot);
      const completed: ServerMessage = { FullSnapshot: pending.snapshot };
      self.postMessage({ type: "message", message: completed });
      return;
    }
    if ("ScreenDiff" in message) {
      const diff = message.ScreenDiff;
      const current = applied.get(diff.snapshot.sessionId);
      if (
        !current ||
        current.daemonEpoch !== diff.snapshot.daemonEpoch ||
        current.sessionGeneration !== diff.snapshot.sessionGeneration ||
        current.snapshotEpoch !== diff.snapshot.snapshotEpoch ||
        current.sequenceNumber !== diff.base_sequence_number
      ) {
        self.postMessage({
          type: "resync",
          sessionId: diff.snapshot.sessionId,
        });
        return;
      }
      const cells = new Map(
        current.styledCells.map((cell) => [`${cell[0]}:${cell[1]}`, cell]),
      );
      for (const [row, column] of diff.cleared_cells)
        cells.delete(`${row}:${column}`);
      for (const cell of diff.changed_cells)
        cells.set(`${cell[0]}:${cell[1]}`, cell);
      const next: ScreenSnapshot = {
        ...diff.snapshot,
        styledCells: [...cells.values()].sort(
          (left, right) => left[0] - right[0] || left[1] - right[1],
        ),
      };
      applied.set(next.sessionId, next);
      const completed: ServerMessage = { FullSnapshot: next };
      self.postMessage({ type: "message", message: completed });
      return;
    }
    self.postMessage({ type: "message", message });
  } catch (error) {
    self.postMessage({
      type: "error",
      error: error instanceof Error ? error.message : String(error),
    });
  }
};
