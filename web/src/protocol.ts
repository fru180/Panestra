import { decode, encode } from "@msgpack/msgpack";
import type {
  ActionContext,
  AgentState,
  ScreenSnapshot,
  Session,
} from "./types";

const MAGIC = new Uint8Array([0x50, 0x41, 0x4e, 0x45]);
export const PROTOCOL_VERSION = 2;
const VERSION = PROTOCOL_VERSION;
const HEADER_LENGTH = 12;
const MAX_PAYLOAD = 256 * 1024;

export type ServerMessage =
  | { HelloAck: { daemon_epoch: string } }
  | { SessionList: Session[] }
  | { FullSnapshot: ScreenSnapshot }
  | {
      FullSnapshotBegin: { snapshot: ScreenSnapshot; chunk_count: number };
    }
  | {
      FullSnapshotChunk: {
        session_id: string;
        snapshot_epoch: string;
        chunk_index: number;
        contents: string;
        styled_cells: ScreenSnapshot["styledCells"];
      };
    }
  | {
      FullSnapshotEnd: {
        session_id: string;
        snapshot_epoch: string;
        sequence_number: number;
      };
    }
  | {
      ScreenDiff: {
        snapshot: ScreenSnapshot;
        base_sequence_number: number;
        changed_cells: ScreenSnapshot["styledCells"];
        cleared_cells: [number, number][];
      };
    }
  | { SessionState: Session }
  | { SessionDeleted: { session_id: string } }
  | { AgentState: { session_id: string; state: AgentState } }
  | { ActionContext: ActionContext }
  | { ActionContextCleared: { session_id: string } }
  | { InputLeaseState: { session_id: string; owned: boolean } }
  | {
      ResizeLeaseState: {
        session_id: string;
        owned: boolean;
        available: boolean;
      };
    }
  | { ProtocolError: { message: string } }
  | "Heartbeat";

export type ProtocolWorkerMessage =
  | { type: "message"; message: ServerMessage }
  | { type: "resync"; sessionId: string }
  | { type: "error"; error: string };

export type ClientMessage =
  | { Hello: { protocol_version: number } }
  | { Subscribe: { session_ids: string[] } }
  | { InputLeaseRequest: { session_id: string; force: boolean } }
  | { ResizeLeaseRequest: { session_id: string; force: boolean } }
  | {
      Resize: {
        session_id: string;
        cols: number;
        rows: number;
        pixel_width: number;
        pixel_height: number;
      };
    }
  | {
      Input: { session_id: string; input_sequence: number; data: Uint8Array };
    }
  | { ResyncRequest: { session_id: string } }
  | {
      SnapshotAck: {
        session_id: string;
        snapshot_epoch: string;
        sequence_number: number;
      };
    }
  | "Heartbeat";

export function encodeClient(message: ClientMessage): Uint8Array {
  const payload = encode(message);
  if (payload.byteLength > MAX_PAYLOAD) throw new Error("message is too large");
  const frame = new Uint8Array(HEADER_LENGTH + payload.byteLength);
  frame.set(MAGIC);
  const view = new DataView(frame.buffer);
  view.setUint16(4, VERSION, false);
  view.setUint8(6, 1);
  view.setUint8(7, 0);
  view.setUint32(8, payload.byteLength, false);
  frame.set(payload, HEADER_LENGTH);
  return frame;
}

export function decodeServer(frame: ArrayBuffer): ServerMessage {
  if (frame.byteLength < HEADER_LENGTH) throw new Error("short protocol frame");
  const bytes = new Uint8Array(frame);
  if (!MAGIC.every((value, index) => value === bytes[index])) {
    throw new Error("invalid protocol magic");
  }
  const view = new DataView(frame);
  if (view.getUint16(4, false) !== VERSION)
    throw new Error("unsupported protocol version");
  const payloadLength = view.getUint32(8, false);
  if (
    payloadLength > MAX_PAYLOAD ||
    HEADER_LENGTH + payloadLength !== frame.byteLength
  ) {
    throw new Error("invalid protocol payload length");
  }
  return decode(bytes.subarray(HEADER_LENGTH)) as ServerMessage;
}
