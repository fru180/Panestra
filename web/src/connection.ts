import { issueWebSocketTicket } from "./api";
import {
  encodeClient,
  PROTOCOL_VERSION,
  type ClientMessage,
  type ProtocolWorkerMessage,
  type ServerMessage,
} from "./protocol";

export type ConnectionState =
  "connecting" | "connected" | "resyncing" | "disconnected";

export class PanestraConnection {
  #socket: WebSocket | null = null;
  #worker = new Worker(new URL("./protocol.worker.ts", import.meta.url), {
    type: "module",
  });
  #subscriptions = new Set<string>();
  #closed = false;
  #retry = 0;
  #inputSequence = 0;
  onMessage: (message: ServerMessage) => void = () => undefined;
  onState: (state: ConnectionState) => void = () => undefined;
  onError: (message: string) => void = () => undefined;

  constructor() {
    this.#worker.onmessage = (event: MessageEvent<ProtocolWorkerMessage>) => {
      if (event.data.type === "message") this.onMessage(event.data.message);
      else if (event.data.type === "resync")
        this.requestResync(String(event.data.sessionId));
      else this.onError(String(event.data.error));
    };
  }

  async connect(): Promise<void> {
    this.#closed = false;
    this.onState(this.#retry === 0 ? "connecting" : "resyncing");
    try {
      const { protocol } = await issueWebSocketTicket();
      const url = new URL("/api/ws", location.href);
      url.protocol = location.protocol === "https:" ? "wss:" : "ws:";
      const socket = new WebSocket(url, protocol);
      socket.binaryType = "arraybuffer";
      this.#socket = socket;
      socket.onopen = () => {
        this.#retry = 0;
        this.send({ Hello: { protocol_version: PROTOCOL_VERSION } });
        this.sendSubscriptions();
        this.requestSubscribedResizeLeases();
        this.onState("connected");
      };
      socket.onmessage = (event: MessageEvent<ArrayBuffer>) => {
        if (event.data instanceof ArrayBuffer)
          this.#worker.postMessage(event.data, [event.data]);
      };
      socket.onerror = () =>
        this.onError("WebSocket接続でエラーが発生しました");
      socket.onclose = () => {
        if (this.#socket === socket) this.#socket = null;
        this.onState("disconnected");
        if (!this.#closed) this.scheduleReconnect();
      };
    } catch (error) {
      this.onError(error instanceof Error ? error.message : String(error));
      this.onState("disconnected");
      if (!this.#closed) this.scheduleReconnect();
    }
  }

  close(): void {
    this.#closed = true;
    this.#socket?.close();
    this.#worker.terminate();
  }

  subscribe(sessionIds: Iterable<string>): void {
    const previous = this.#subscriptions;
    this.#subscriptions = new Set(sessionIds);
    this.sendSubscriptions();
    for (const sessionId of this.#subscriptions) {
      if (!previous.has(sessionId)) this.requestResizeLease(sessionId);
    }
  }

  requestInputLease(sessionId: string, force = false): void {
    this.send({ InputLeaseRequest: { session_id: sessionId, force } });
  }

  requestResizeLease(sessionId: string, force = false): void {
    this.send({ ResizeLeaseRequest: { session_id: sessionId, force } });
  }

  resize(
    sessionId: string,
    size: {
      cols: number;
      rows: number;
      pixelWidth: number;
      pixelHeight: number;
    },
  ): void {
    this.send({
      Resize: {
        session_id: sessionId,
        cols: size.cols,
        rows: size.rows,
        pixel_width: size.pixelWidth,
        pixel_height: size.pixelHeight,
      },
    });
  }

  input(sessionId: string, data: string | Uint8Array): void {
    const bytes =
      typeof data === "string" ? new TextEncoder().encode(data) : data;
    this.send({
      Input: {
        session_id: sessionId,
        input_sequence: ++this.#inputSequence,
        data: bytes,
      },
    });
  }

  snapshotAck(snapshot: {
    sessionId: string;
    snapshotEpoch: string;
    sequenceNumber: number;
  }): void {
    this.send({
      SnapshotAck: {
        session_id: snapshot.sessionId,
        snapshot_epoch: snapshot.snapshotEpoch,
        sequence_number: snapshot.sequenceNumber,
      },
    });
  }

  requestResync(sessionId: string): void {
    this.send({ ResyncRequest: { session_id: sessionId } });
  }

  private sendSubscriptions(): void {
    if (this.#socket?.readyState === WebSocket.OPEN) {
      this.send({ Subscribe: { session_ids: [...this.#subscriptions] } });
    }
  }

  private requestSubscribedResizeLeases(): void {
    for (const sessionId of this.#subscriptions)
      this.requestResizeLease(sessionId);
  }

  private send(message: ClientMessage): void {
    if (this.#socket?.readyState === WebSocket.OPEN) {
      this.#socket.send(Uint8Array.from(encodeClient(message)).buffer);
    }
  }

  private scheduleReconnect(): void {
    const delay = Math.min(500 * 2 ** this.#retry++, 8_000);
    window.setTimeout(() => void this.connect(), delay);
  }
}
