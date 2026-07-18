import { describe, expect, it } from "vitest";

import { identifyBrowser, isAllowedBrowser } from "./browser-support";

describe("identifyBrowser", () => {
  it("accepts Chrome on macOS", () => {
    expect(
      identifyBrowser(
        "Mozilla/5.0 (Macintosh) AppleWebKit/537.36 Chrome/121.0.0.0 Safari/537.36",
        "MacIntel",
      ),
    ).toEqual({ name: "Google Chrome", majorVersion: 121, macOS: true });
  });

  it("distinguishes Edge and rejects Firefox", () => {
    expect(
      identifyBrowser(
        "Mozilla/5.0 (Macintosh) AppleWebKit/537.36 Chrome/140.0.0.0 Safari/537.36 Edg/140.0.0.0",
        "MacIntel",
      ).name,
    ).toBe("Microsoft Edge");
    expect(
      identifyBrowser(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:140.0) Gecko/20100101 Firefox/140.0",
        "MacIntel",
      ).name,
    ).toBe("unsupported");
  });

  it("enforces the minimum release version and macOS", () => {
    expect(
      isAllowedBrowser({
        name: "Google Chrome",
        majorVersion: 120,
        macOS: true,
      }),
    ).toBe(false);
    expect(
      isAllowedBrowser({
        name: "Microsoft Edge",
        majorVersion: 140,
        macOS: false,
      }),
    ).toBe(false);
  });
});
