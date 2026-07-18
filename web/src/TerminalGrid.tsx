import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";

import type { PanestraConnection } from "./connection";
import type {
  ActionContext,
  GitStatus,
  Project,
  ScreenSnapshot,
  Session,
} from "./types";
import {
  CELL_HEIGHT,
  CELL_WIDTH,
  PANE_HEADER_HEIGHT,
  expandedPaneLayout,
  logicalTerminalSize,
  responsivePaneLayout,
  terminalViewport,
  type TerminalDisplaySize,
} from "./terminal-geometry";
import { TerminalRenderer, type PaneRenderModel } from "./webgpu-renderer";

type Props = {
  sessions: Session[];
  snapshots: Map<string, ScreenSnapshot>;
  actionContexts: Map<string, ActionContext>;
  gitStatuses: Map<string, GitStatus>;
  projects: Project[];
  device: GPUDevice;
  connection: PanestraConnection;
  activeId?: string;
  expandedId?: string;
  displaySize: TerminalDisplaySize;
  leaseOwned: boolean;
  resizeLeaseIds: ReadonlySet<string>;
  onActivate: (id: string) => void;
  onToggleExpanded: (id: string) => void;
  onBrowseHistory: (id: string) => void;
  onDeviceLost: (reason: string) => void;
};

export function TerminalGrid(props: Props) {
  let scrollHost!: HTMLDivElement;
  let stage!: HTMLDivElement;
  let canvas!: HTMLCanvasElement;
  let input!: HTMLTextAreaElement;
  let renderer: TerminalRenderer | undefined;
  let resizeObserver: ResizeObserver | undefined;
  let composing = false;
  let expanded = false;
  const resizeTimers = new Map<string, number>();
  const lastRequestedSizes = new Map<string, string>();
  let listScroll = { left: 0, top: 0 };
  const [size, setSize] = createSignal({ width: 0, height: 0 });
  const [selection, setSelection] = createSignal<{
    sessionId: string;
    start: [number, number];
    end: [number, number];
  }>();

  const visibleSessions = createMemo(() => {
    if (props.expandedId)
      return props.sessions.filter(
        (session) => session.id === props.expandedId,
      );
    return props.sessions;
  });

  const layout = createMemo(() => {
    const { width, height } = size();
    const sessions = visibleSessions().map(({ id, cols, rows }) => ({
      id,
      cols,
      rows,
    }));
    return props.expandedId
      ? expandedPaneLayout(sessions[0], width, height)
      : responsivePaneLayout(sessions, width, height, props.displaySize);
  });

  const panes = createMemo<PaneRenderModel[]>(() => {
    return layout().panes.map((pane) => ({
      ...pane,
      snapshot: props.snapshots.get(pane.id),
      active: pane.id === props.activeId,
    }));
  });

  const stageStyle = () => ({
    width: `${layout().width}px`,
    height: `${layout().height}px`,
  });

  const overlayStyle = (id: string) => {
    const pane = panes().find((candidate) => candidate.id === id);
    if (!pane) return {};
    return {
      left: `${pane.x}px`,
      top: `${pane.y - PANE_HEADER_HEIGHT}px`,
      width: `${pane.width}px`,
      height: `${pane.height + PANE_HEADER_HEIGHT}px`,
    };
  };

  onMount(() => {
    resizeObserver = new ResizeObserver(([entry]) => {
      if (!entry) return;
      const width = Math.floor(entry.contentRect.width);
      const height = Math.floor(entry.contentRect.height);
      setSize((current) =>
        current.width === width && current.height === height
          ? current
          : { width, height },
      );
    });
    resizeObserver.observe(scrollHost);
  });
  onCleanup(() => {
    resizeObserver?.disconnect();
    for (const timer of resizeTimers.values()) window.clearTimeout(timer);
  });

  createEffect(() => {
    const currentDevice = props.device;
    if (!canvas) return;
    renderer = new TerminalRenderer(canvas, currentDevice);
    renderer.onDeviceLost = props.onDeviceLost;
  });

  createEffect(() => {
    void props.device;
    renderer?.render(panes());
  });

  createEffect(() => {
    if (props.activeId && props.leaseOwned)
      input?.focus({ preventScroll: true });
  });

  createEffect(() => {
    const isExpanded = Boolean(props.expandedId);
    if (isExpanded && !expanded) {
      listScroll = {
        left: scrollHost.scrollLeft,
        top: scrollHost.scrollTop,
      };
      scrollHost.scrollTo(0, 0);
    } else if (!isExpanded && expanded) {
      queueMicrotask(() => scrollHost.scrollTo(listScroll));
    }
    expanded = isExpanded;
  });

  createEffect(() => {
    const owned = props.resizeLeaseIds;
    const desired = new Map(
      layout().panes.map((pane) => [
        pane.id,
        logicalTerminalSize(pane.width, pane.height),
      ]),
    );
    for (const [sessionId, timer] of resizeTimers) {
      if (!desired.has(sessionId)) {
        window.clearTimeout(timer);
        resizeTimers.delete(sessionId);
      }
    }
    for (const session of visibleSessions()) {
      const timer = resizeTimers.get(session.id);
      if (!owned.has(session.id) || session.processState !== "running") {
        if (timer !== undefined) window.clearTimeout(timer);
        resizeTimers.delete(session.id);
        lastRequestedSizes.delete(session.id);
        continue;
      }
      const target = desired.get(session.id);
      if (!target) continue;
      const key = `${target.cols}x${target.rows}`;
      if (
        lastRequestedSizes.get(session.id) === key ||
        (session.cols === target.cols && session.rows === target.rows)
      ) {
        lastRequestedSizes.set(session.id, key);
        if (timer !== undefined) window.clearTimeout(timer);
        resizeTimers.delete(session.id);
        continue;
      }
      if (timer !== undefined) window.clearTimeout(timer);
      resizeTimers.set(
        session.id,
        window.setTimeout(() => {
          resizeTimers.delete(session.id);
          lastRequestedSizes.set(session.id, key);
          props.connection.resize(session.id, target);
        }, 75),
      );
    }
  });

  function activate(id: string): void {
    props.onActivate(id);
    queueMicrotask(() => input.focus({ preventScroll: true }));
  }

  function sendText(value: string): void {
    if (props.activeId && props.leaseOwned && value)
      props.connection.input(props.activeId, value);
  }

  function onInput(
    event: InputEvent & { currentTarget: HTMLTextAreaElement },
  ): void {
    if (!composing) sendText(event.currentTarget.value);
    event.currentTarget.value = "";
  }

  function onKeyDown(event: KeyboardEvent): void {
    if (composing || !props.activeId || !props.leaseOwned) return;
    const sequence = keySequence(event, props.snapshots.get(props.activeId));
    if (sequence !== undefined) {
      event.preventDefault();
      props.connection.input(props.activeId, sequence);
    }
  }

  function logicalCell(
    event: PointerEvent | WheelEvent,
    id: string,
  ): [number, number] | undefined {
    const pane = panes().find((candidate) => candidate.id === id);
    const snapshot = props.snapshots.get(id);
    if (!pane || !snapshot) return undefined;
    const viewport = terminalViewport(pane, snapshot.cols, snapshot.rows);
    const bounds = stage.getBoundingClientRect();
    const x = event.clientX - bounds.left - viewport.x;
    const y = event.clientY - bounds.top - viewport.y;
    if (x < 0 || y < 0 || x >= viewport.width || y >= viewport.height)
      return undefined;
    return [
      Math.floor(y / (CELL_HEIGHT * viewport.scale)),
      Math.floor(x / (CELL_WIDTH * viewport.scale)),
    ];
  }

  function pointerDown(event: PointerEvent, id: string): void {
    activate(id);
    const snapshot = props.snapshots.get(id);
    const cell = logicalCell(event, id);
    if (!snapshot || !cell) return;
    if (snapshot.mouseReporting && id === props.activeId && props.leaseOwned) {
      event.preventDefault();
      props.connection.input(
        id,
        mouseSequence(snapshot, event.button, cell, false),
      );
      return;
    }
    event.currentTarget instanceof Element &&
      event.currentTarget.setPointerCapture(event.pointerId);
    setSelection({ sessionId: id, start: cell, end: cell });
  }

  function pointerMove(event: PointerEvent, id: string): void {
    const current = selection();
    if (!current || current.sessionId !== id || event.buttons === 0) return;
    const cell = logicalCell(event, id);
    if (cell) setSelection({ ...current, end: cell });
  }

  function pointerUp(event: PointerEvent, id: string): void {
    const snapshot = props.snapshots.get(id);
    const cell = logicalCell(event, id);
    if (
      snapshot?.mouseReporting &&
      cell &&
      id === props.activeId &&
      props.leaseOwned
    ) {
      event.preventDefault();
      props.connection.input(
        id,
        mouseSequence(snapshot, event.button, cell, true),
      );
    }
  }

  return (
    <div class="terminal-grid-scroll" ref={scrollHost}>
      <div class="terminal-grid" ref={stage} style={stageStyle()}>
        <canvas ref={canvas} aria-hidden="true" />
        <For each={visibleSessions()}>
          {(session) => (
            <section
              classList={{
                "pane-overlay": true,
                active: session.id === props.activeId,
                attention: paneNeedsAttention(
                  session,
                  props.gitStatuses.get(session.id),
                ),
              }}
              style={overlayStyle(session.id)}
              onPointerDown={(event) => pointerDown(event, session.id)}
              onPointerMove={(event) => pointerMove(event, session.id)}
              onPointerUp={(event) => pointerUp(event, session.id)}
              onWheel={(event) => {
                const snapshot = props.snapshots.get(session.id);
                const cell = logicalCell(event, session.id);
                if (!snapshot?.mouseReporting) {
                  event.preventDefault();
                  props.onBrowseHistory(session.id);
                  return;
                }
                if (!cell || !props.leaseOwned || session.id !== props.activeId)
                  return;
                event.preventDefault();
                props.connection.input(
                  session.id,
                  mouseSequence(
                    snapshot,
                    event.deltaY < 0 ? 64 : 65,
                    cell,
                    false,
                  ),
                );
              }}
            >
              <header class="pane-header">
                <div
                  class="pane-title"
                  title={paneDescription(
                    session,
                    props.actionContexts.get(session.id),
                    projectName(props.projects, session.projectId),
                  )}
                >
                  <span
                    class={`state-dot state-${session.agentState ?? session.processState}`}
                  />
                  <span class="pane-heading">
                    <strong>{session.name}</strong>
                    <small>
                      {props.actionContexts.get(session.id)?.taskSummary ??
                        session.currentCwd ??
                        session.launchCwd}
                    </small>
                  </span>
                  <span class="pane-kind">{agentLabel(session)}</span>
                  <Show when={projectName(props.projects, session.projectId)}>
                    {(value) => <span class="pane-meta">{value()}</span>}
                  </Show>
                  <Show when={props.actionContexts.get(session.id)}>
                    {(action) => (
                      <span class="pane-meta" title={action().repositoryPath}>
                        {pathName(action().repositoryPath)}
                        {action().gitBranch ? ` · ${action().gitBranch}` : ""}
                      </span>
                    )}
                  </Show>
                  <Show when={props.gitStatuses.get(session.id)}>
                    {(status) => (
                      <span
                        classList={{
                          "git-summary": true,
                          conflict: status().conflicts > 0,
                          stale: status().stale,
                        }}
                        title={`working tree全体: staged ${status().staged}, unstaged ${status().unstaged}, untracked ${status().untracked}, +${status().additions} -${status().deletions}`}
                      >
                        {status().actualBranch ?? "detached"} ·{" "}
                        {status().changedFiles}変更
                      </span>
                    )}
                  </Show>
                </div>
                <div class="pane-actions">
                  <Show
                    when={session.id === props.activeId && !props.leaseOwned}
                  >
                    <button
                      type="button"
                      class="lease-warning"
                      title="別のタブから入力権を移動します"
                      onPointerDown={(event) => event.stopPropagation()}
                      onClick={() =>
                        props.connection.requestInputLease(session.id, true)
                      }
                    >
                      入力権を取得
                    </button>
                  </Show>
                  <Show when={selection()?.sessionId === session.id}>
                    <span class="lease-warning">範囲選択中</span>
                  </Show>
                  <button
                    type="button"
                    title={props.expandedId ? "一覧へ戻る" : "精読表示"}
                    onPointerDown={(event) => event.stopPropagation()}
                    onClick={() => {
                      props.onActivate(session.id);
                      props.onToggleExpanded(session.id);
                    }}
                  >
                    {props.expandedId ? "縮小" : "拡大"}
                  </button>
                </div>
              </header>
              <div class="pane-focus-ring" />
            </section>
          )}
        </For>
        <textarea
          ref={input}
          class="terminal-input"
          aria-label="アクティブターミナルへの入力"
          autocomplete="off"
          autocapitalize="off"
          spellcheck={false}
          onInput={onInput}
          onKeyDown={onKeyDown}
          onPaste={(event) => {
            if (!props.activeId || !props.leaseOwned) return;
            event.preventDefault();
            const text = event.clipboardData?.getData("text/plain") ?? "";
            const snapshot = props.snapshots.get(props.activeId);
            sendText(
              snapshot?.bracketedPaste ? `\x1b[200~${text}\x1b[201~` : text,
            );
          }}
          onCopy={(event) => {
            const current = selection();
            if (!current) return;
            const snapshot = props.snapshots.get(current.sessionId);
            if (!snapshot) return;
            event.preventDefault();
            event.clipboardData?.setData(
              "text/plain",
              selectedText(snapshot, current.start, current.end),
            );
          }}
          onCompositionStart={() => (composing = true)}
          onCompositionEnd={(event) => {
            composing = false;
            sendText(event.data);
            input.value = "";
          }}
          onFocus={() => {
            if (!props.activeId || !props.leaseOwned) return;
            const snapshot = props.snapshots.get(props.activeId);
            if (snapshot?.focusReporting)
              props.connection.input(props.activeId, "\x1b[I");
          }}
          onBlur={() => {
            if (!props.activeId || !props.leaseOwned) return;
            const snapshot = props.snapshots.get(props.activeId);
            if (snapshot?.focusReporting)
              props.connection.input(props.activeId, "\x1b[O");
          }}
        />
        <div
          class="screen-reader-mirror"
          role="region"
          aria-label="アクティブターミナル画面"
        >
          <pre>
            {props.activeId
              ? props.snapshots.get(props.activeId)?.contents
              : ""}
          </pre>
        </div>
        <Show when={visibleSessions().length === 0}>
          <div class="empty-grid">
            <p>まだセッションはありません</p>
            <span>
              右上の「新しいセッション」からローカルCLIを起動できます。
            </span>
          </div>
        </Show>
      </div>
    </div>
  );
}

function pathName(path: string): string {
  return path.split("/").filter(Boolean).at(-1) ?? path;
}

function projectName(projects: Project[], id?: string): string | undefined {
  return projects.find((project) => project.id === id)?.name;
}

function paneNeedsAttention(session: Session, git?: GitStatus): boolean {
  return (
    session.processState === "exited" ||
    session.processState === "killed" ||
    Boolean(git?.conflicts) ||
    ["waiting_approval", "awaiting_user", "integration_error"].includes(
      session.agentState ?? "",
    )
  );
}

function paneDescription(
  session: Session,
  action?: ActionContext,
  project?: string,
): string {
  return [
    session.name,
    project,
    action?.taskSummary,
    action?.repositoryPath,
    action?.gitBranch ? `申告ブランチ: ${action.gitBranch}` : undefined,
    `起動: ${session.launchCwd}`,
    session.currentCwd ? `現在: ${session.currentCwd}` : undefined,
    session.lastActivityAt ? `最終活動: ${session.lastActivityAt}` : undefined,
  ]
    .filter(Boolean)
    .join("\n");
}

function mouseSequence(
  snapshot: ScreenSnapshot,
  button: number,
  [row, col]: [number, number],
  release: boolean,
): string {
  if (snapshot.mouseEncoding === "sgr")
    return `\x1b[<${button};${col + 1};${row + 1}${release ? "m" : "M"}`;
  const code = release ? 3 : button;
  const encode = (value: number) =>
    String.fromCodePoint(
      Math.min(snapshot.mouseEncoding === "utf8" ? 2047 : 223, value + 32),
    );
  return `\x1b[M${encode(code)}${encode(col + 1)}${encode(row + 1)}`;
}

function selectedText(
  snapshot: ScreenSnapshot,
  start: [number, number],
  end: [number, number],
): string {
  let [startRow, startCol] = start;
  let [endRow, endCol] = end;
  if (startRow > endRow || (startRow === endRow && startCol > endCol)) {
    [startRow, endRow] = [endRow, startRow];
    [startCol, endCol] = [endCol, startCol];
  }
  if (snapshot.styledCells.length) {
    const cells = new Map(
      snapshot.styledCells.map((cell) => [`${cell[0]}:${cell[1]}`, cell]),
    );
    const rows: string[] = [];
    for (let row = startRow; row <= endRow; row += 1) {
      const from = row === startRow ? startCol : 0;
      const to = row === endRow ? endCol : snapshot.cols - 1;
      let line = "";
      for (let column = from; column <= to; column += 1) {
        const cell = cells.get(`${row}:${column}`);
        if (!cell) line += " ";
        else if (cell[3] !== 0) line += cell[2];
      }
      rows.push(line.replace(/\s+$/u, ""));
    }
    return rows.join("\n");
  }
  const lines = snapshot.contents.split("\n");
  return lines
    .slice(startRow, endRow + 1)
    .map((line, index, selected) => {
      const characters = [...line];
      const from = index === 0 ? startCol : 0;
      const to = index === selected.length - 1 ? endCol + 1 : characters.length;
      return characters.slice(from, to).join("");
    })
    .join("\n");
}

function agentLabel(session: Session): string {
  if (!session.agentIntegration)
    return session.processState === "running" ? "Terminal" : "終了";
  const provider = session.agentIntegration === "codex" ? "Codex" : "Claude";
  const labels = {
    initializing: "連携準備中",
    working: "作業中",
    waiting_approval: "承認待ち",
    awaiting_user: "ユーザー応答待ち",
    integration_error: "状態連携エラー",
  } as const;
  return `${provider} · ${session.agentState ? labels[session.agentState] : "連携準備中"}`;
}

function keySequence(
  event: KeyboardEvent,
  snapshot?: ScreenSnapshot,
): string | undefined {
  if (event.metaKey && event.key.toLowerCase() === "v") return undefined;
  if (snapshot?.applicationKeypad) {
    const keypad: Record<string, string> = {
      Numpad0: "\x1bOp",
      Numpad1: "\x1bOq",
      Numpad2: "\x1bOr",
      Numpad3: "\x1bOs",
      Numpad4: "\x1bOt",
      Numpad5: "\x1bOu",
      Numpad6: "\x1bOv",
      Numpad7: "\x1bOw",
      Numpad8: "\x1bOx",
      Numpad9: "\x1bOy",
      NumpadDecimal: "\x1bOn",
      NumpadDivide: "\x1bOo",
      NumpadMultiply: "\x1bOj",
      NumpadSubtract: "\x1bOm",
      NumpadAdd: "\x1bOk",
      NumpadEnter: "\x1bOM",
      NumpadEqual: "\x1bOX",
    };
    if (keypad[event.code]) return keypad[event.code];
  }
  if (event.ctrlKey && event.key.length === 1) {
    const code = event.key.toUpperCase().charCodeAt(0);
    if (code >= 64 && code <= 95) return String.fromCharCode(code - 64);
  }
  const cursorPrefix = snapshot?.applicationCursor ? "\x1bO" : "\x1b[";
  const sequences: Record<string, string> = {
    Enter: "\r",
    Backspace: "\x7f",
    Tab: "\t",
    Escape: "\x1b",
    ArrowUp: `${cursorPrefix}A`,
    ArrowDown: `${cursorPrefix}B`,
    ArrowRight: `${cursorPrefix}C`,
    ArrowLeft: `${cursorPrefix}D`,
    Home: "\x1b[H",
    End: "\x1b[F",
    Delete: "\x1b[3~",
    PageUp: "\x1b[5~",
    PageDown: "\x1b[6~",
  };
  return sequences[event.key];
}
