export type BrowserSupport = {
  supported: boolean;
  reasons: string[];
  capabilities: {
    webGpu: boolean;
    webWorker: boolean;
    binaryWebSocket: boolean;
    textDecoder: boolean;
    clipboard: boolean;
    ime: boolean;
  };
};

export type BrowserIdentity = {
  name: "Google Chrome" | "Microsoft Edge" | "unsupported";
  majorVersion?: number;
  macOS: boolean;
};

export const SUPPORTED_BROWSER_DESCRIPTION =
  "macOS版 Google Chrome 121以降、またはMicrosoft Edge 121以降";

export function identifyBrowser(
  userAgent: string,
  platform: string,
): BrowserIdentity {
  const macOS = /Mac/i.test(platform) || /Macintosh|Mac OS X/i.test(userAgent);
  const edge = /Edg\/(\d+)/.exec(userAgent);
  if (edge)
    return {
      name: "Microsoft Edge",
      majorVersion: Number(edge[1]),
      macOS,
    };
  const chrome = /Chrome\/(\d+)/.exec(userAgent);
  if (chrome && !/OPR\//.test(userAgent))
    return {
      name: "Google Chrome",
      majorVersion: Number(chrome[1]),
      macOS,
    };
  return { name: "unsupported", macOS };
}

export function isAllowedBrowser(browser: BrowserIdentity): boolean {
  return (
    browser.macOS &&
    browser.name !== "unsupported" &&
    browser.majorVersion !== undefined &&
    browser.majorVersion >= 121
  );
}

export function detectBrowserSupport(): BrowserSupport {
  const browser = identifyBrowser(navigator.userAgent, navigator.platform);
  const capabilities = {
    webGpu: "gpu" in navigator,
    webWorker: "Worker" in window,
    binaryWebSocket: "WebSocket" in window && "ArrayBuffer" in window,
    textDecoder: "TextDecoder" in window,
    clipboard: Boolean(navigator.clipboard),
    ime: "CompositionEvent" in window,
  };
  const labels: Record<keyof typeof capabilities, string> = {
    webGpu: "WebGPU",
    webWorker: "Web Worker",
    binaryWebSocket: "binary WebSocket",
    textDecoder: "TextDecoder",
    clipboard: "Clipboard API",
    ime: "IME composition events",
  };
  const reasons = (Object.keys(capabilities) as (keyof typeof capabilities)[])
    .filter((key) => !capabilities[key])
    .map((key) => `${labels[key]}を利用できません`);
  if (!browser.macOS) reasons.unshift("初期版はmacOS専用です");
  if (!isAllowedBrowser(browser))
    reasons.unshift(
      `対応ブラウザではありません（${SUPPORTED_BROWSER_DESCRIPTION}）`,
    );
  return { supported: reasons.length === 0, reasons, capabilities };
}
