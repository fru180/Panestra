import { describe, expect, it } from "vitest";

import { encodeClient, PROTOCOL_VERSION } from "./protocol";

describe("protocol envelope", () => {
  it("uses the Panestra magic, version, and network byte order", () => {
    const frame = encodeClient({
      Hello: { protocol_version: PROTOCOL_VERSION },
    });
    expect([...frame.slice(0, 4)]).toEqual([0x50, 0x41, 0x4e, 0x45]);
    const view = new DataView(frame.buffer, frame.byteOffset, frame.byteLength);
    expect(view.getUint16(4, false)).toBe(2);
    expect(view.getUint32(8, false)).toBe(frame.byteLength - 12);
  });
});
