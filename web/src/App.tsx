import {
  For,
  Match,
  Show,
  Switch,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
  untrack,
} from "solid-js";
import type { JSX } from "solid-js";

import {
  createProject as createProjectApi,
  createSession,
  deleteSessionRecord,
  exchangeBootstrap,
  getCredential,
  getEnvironment,
  getGitStatus,
  listCurrentActions,
  listProjects,
  listSessions,
  readBootstrapToken,
  terminateSession,
} from "./api";
import { detectBrowserSupport } from "./browser-support";
import { PanestraConnection, type ConnectionState } from "./connection";
import { NewSessionDialog } from "./NewSessionDialog";
import { HistoryDrawer } from "./HistoryDrawer";
import { TerminalGrid } from "./TerminalGrid";
import type { ServerMessage } from "./protocol";
import type {
  ActionContext,
  CreateSessionRequest,
  GitStatus,
  Project,
  ScreenSnapshot,
  Session,
} from "./types";
import {
  isTerminalDisplaySize,
  type TerminalDisplaySize,
} from "./terminal-geometry";
import { requestWebGpuDevice } from "./webgpu-renderer";
import { isVisibleTerminal } from "./session-visibility";

type StartupState =
  "checking" | "unsupported" | "authentication_required" | "ready" | "failed";

const TERMINAL_DISPLAY_SIZE_KEY = "panestra.terminalDisplaySize";

export default function App() {
  const [startup, setStartup] = createSignal<StartupState>("checking");
  const [startupReasons, setStartupReasons] = createSignal<string[]>([]);
  const [error, setError] = createSignal("");
  const [sessions, setSessions] = createSignal<Session[]>([]);
  const [snapshots, setSnapshots] = createSignal(
    new Map<string, ScreenSnapshot>(),
  );
  const [actionContexts, setActionContexts] = createSignal(
    new Map<string, ActionContext>(),
  );
  const [gitStatuses, setGitStatuses] = createSignal(
    new Map<string, GitStatus>(),
  );
  const [projects, setProjects] = createSignal<Project[]>([]);
  const [selectedProjectId, setSelectedProjectId] = createSignal<string>();
  const [device, setDevice] = createSignal<GPUDevice>();
  const [gpuRecovering, setGpuRecovering] = createSignal(false);
  const [connectionState, setConnectionState] =
    createSignal<ConnectionState>("disconnected");
  const [activeId, setActiveId] = createSignal<string>();
  const [expandedId, setExpandedId] = createSignal<string>();
  const [leaseSessionId, setLeaseSessionId] = createSignal<string>();
  const [resizeLeaseSessionIds, setResizeLeaseSessionIds] = createSignal(
    new Set<string>(),
  );
  const [terminalDisplaySize, setTerminalDisplaySize] =
    createSignal<TerminalDisplaySize>(readTerminalDisplaySize());
  const [attentionOnly, setAttentionOnly] = createSignal(false);
  const [dialogOpen, setDialogOpen] = createSignal(false);
  const [historyOpen, setHistoryOpen] = createSignal(false);
  const [environment, setEnvironment] = createSignal({
    home: "",
    shell: "/bin/zsh",
  });
  let connection: PanestraConnection | undefined;
  let gitRefreshTimer: number | undefined;

  const visibleSessions = createMemo(() => {
    const statuses = gitStatuses();
    let source = attentionOnly()
      ? sessions().filter((session) =>
          needsAttention(session, statuses.get(session.id)),
        )
      : sessions();
    if (selectedProjectId()) {
      source = source.filter(
        (session) => session.projectId === selectedProjectId(),
      );
    }
    return [...source].sort((left, right) => {
      return (
        attentionRank(right, statuses.get(right.id)) -
        attentionRank(left, statuses.get(left.id))
      );
    });
  });

  onMount(() => void initialize());
  onCleanup(() => {
    connection?.close();
    if (gitRefreshTimer !== undefined) window.clearInterval(gitRefreshTimer);
  });

  async function initialize(): Promise<void> {
    const support = detectBrowserSupport();
    const bootstrapToken = readBootstrapToken();
    if (!support.supported) {
      setStartupReasons(support.reasons);
      setStartup("unsupported");
      return;
    }
    try {
      const gpuDevice = await requestWebGpuDevice();
      setDevice(gpuDevice);
      if (!getCredential()) {
        if (!bootstrapToken) {
          setStartup("authentication_required");
          return;
        }
        await exchangeBootstrap(bootstrapToken, support.capabilities);
      }
      const [
        initialSessions,
        initialEnvironment,
        initialActions,
        initialProjects,
      ] = await Promise.all([
        listSessions(),
        getEnvironment(),
        listCurrentActions(),
        listProjects(),
      ]);
      const visibleInitialSessions = initialSessions.filter(isVisibleTerminal);
      const visibleSessionIds = new Set(
        visibleInitialSessions.map((session) => session.id),
      );
      const visibleInitialActions = initialActions.filter((action) =>
        visibleSessionIds.has(action.sessionId),
      );
      setSessions(visibleInitialSessions);
      setEnvironment(initialEnvironment);
      setActionContexts(
        new Map(
          visibleInitialActions.map((action) => [action.sessionId, action]),
        ),
      );
      setProjects(initialProjects);
      startConnection(visibleInitialSessions);
      setStartup("ready");
      void refreshGitStatuses(visibleInitialActions);
      gitRefreshTimer = window.setInterval(
        () => void refreshGitStatuses([...untrack(actionContexts).values()]),
        5_000,
      );
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
      setStartup("failed");
    }
  }

  function startConnection(initialSessions: Session[]): void {
    connection = new PanestraConnection();
    connection.onState = setConnectionState;
    connection.onError = setError;
    connection.onMessage = handleServerMessage;
    connection.subscribe(
      initialSessions.slice(0, 25).map((session) => session.id),
    );
    void connection.connect();
  }

  function handleServerMessage(message: ServerMessage): void {
    if (typeof message === "string") return;
    if ("SessionList" in message) {
      const visible = message.SessionList.filter(isVisibleTerminal);
      setSessions(visible);
      connection?.subscribe(visible.slice(0, 25).map((session) => session.id));
    } else if ("SessionState" in message) {
      mergeSession(message.SessionState);
    } else if ("SessionDeleted" in message) {
      removeSession(message.SessionDeleted.session_id);
    } else if ("AgentState" in message) {
      const { session_id, state } = message.AgentState;
      setSessions((current) =>
        current.map((session) =>
          session.id === session_id
            ? { ...session, agentState: state }
            : session,
        ),
      );
    } else if ("FullSnapshot" in message) {
      const snapshot = message.FullSnapshot;
      setSnapshots((current) => {
        const previous = current.get(snapshot.sessionId);
        if (
          previous &&
          previous.sessionGeneration === snapshot.sessionGeneration &&
          previous.sequenceNumber > snapshot.sequenceNumber
        )
          return current;
        const next = new Map(current);
        next.set(snapshot.sessionId, snapshot);
        return next;
      });
      connection?.snapshotAck(snapshot);
    } else if ("ActionContext" in message) {
      setActionContexts((current) =>
        new Map(current).set(
          message.ActionContext.sessionId,
          message.ActionContext,
        ),
      );
      void refreshGitStatuses([message.ActionContext]);
    } else if ("ActionContextCleared" in message) {
      const sessionId = message.ActionContextCleared.session_id;
      setActionContexts((current) => {
        const next = new Map(current);
        next.delete(sessionId);
        return next;
      });
      setGitStatuses((current) => {
        const next = new Map(current);
        next.delete(sessionId);
        return next;
      });
    } else if ("InputLeaseState" in message) {
      const lease = message.InputLeaseState;
      if (lease.owned) {
        if (activeId() === lease.session_id)
          setLeaseSessionId(lease.session_id);
      } else if (leaseSessionId() === lease.session_id) {
        setLeaseSessionId(undefined);
      }
    } else if ("ResizeLeaseState" in message) {
      const lease = message.ResizeLeaseState;
      setResizeLeaseSessionIds((current) => {
        const next = new Set(current);
        if (lease.owned) next.add(lease.session_id);
        else next.delete(lease.session_id);
        return next;
      });
      if (!lease.owned && lease.available)
        connection?.requestResizeLease(lease.session_id);
    } else if ("ProtocolError" in message) {
      setError(message.ProtocolError.message);
    }
  }

  async function refreshGitStatuses(actions: ActionContext[]): Promise<void> {
    const gitActions = actions.filter((action) => action.gitWorkingTreeRoot);
    const results = await Promise.allSettled(
      gitActions.map(async (action) => ({
        sessionId: action.sessionId,
        status: await getGitStatus(action.sessionId),
      })),
    );
    setGitStatuses((current) => {
      const next = new Map(current);
      for (const result of results) {
        if (result.status === "fulfilled")
          next.set(result.value.sessionId, result.value.status);
      }
      return next;
    });
  }

  function mergeSession(updated: Session): void {
    if (!isVisibleTerminal(updated)) {
      removeSession(updated.id);
      return;
    }
    setSessions((current) => {
      const found = current.some((session) => session.id === updated.id);
      const next = found
        ? current.map((session) =>
            session.id === updated.id ? updated : session,
          )
        : [...current, updated];
      connection?.subscribe(next.slice(0, 25).map((session) => session.id));
      return next;
    });
  }

  function activate(id: string): void {
    setActiveId(id);
    setLeaseSessionId(undefined);
    connection?.requestInputLease(id);
  }

  async function create(request: CreateSessionRequest): Promise<void> {
    const session = await createSession(request);
    mergeSession(session);
    activate(session.id);
  }

  async function stop(id: string): Promise<void> {
    await terminateSession(id);
    window.setTimeout(() => {
      const session = untrack(sessions).find(
        (candidate) => candidate.id === id,
      );
      if (
        session?.processState === "running" &&
        window.confirm(
          `${session.name} は終了していません。プロセスグループを強制終了しますか？`,
        )
      )
        void terminateSession(id, true).catch((caught) =>
          setError(caught instanceof Error ? caught.message : String(caught)),
        );
    }, 2_500);
  }

  function removeSession(id: string): void {
    setSessions((current) => {
      const next = current.filter((session) => session.id !== id);
      connection?.subscribe(next.slice(0, 25).map((session) => session.id));
      return next;
    });
    setSnapshots((current) => {
      const next = new Map(current);
      next.delete(id);
      return next;
    });
    setActionContexts((current) => {
      const next = new Map(current);
      next.delete(id);
      return next;
    });
    setResizeLeaseSessionIds((current) => {
      const next = new Set(current);
      next.delete(id);
      return next;
    });
    if (activeId() === id) setActiveId(undefined);
    if (expandedId() === id) setExpandedId(undefined);
  }

  async function removeRecord(id: string): Promise<void> {
    const choice = window.prompt(
      "削除方法を入力してください: metadata（履歴ファイルを残す） / all（履歴も完全削除）",
      "metadata",
    );
    if (choice !== "metadata" && choice !== "all") return;
    const includeHistory = choice === "all";
    if (
      includeHistory &&
      !window.confirm(
        "確定済み画面と履歴を完全に削除します。この操作は復元できません。",
      )
    )
      return;
    try {
      await deleteSessionRecord(id, includeHistory);
      removeSession(id);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function addProject(): Promise<void> {
    const name = window.prompt("プロジェクト名");
    if (!name) return;
    const rootPath = window.prompt(
      "プロジェクトのルートディレクトリ",
      environment().home,
    );
    if (!rootPath) return;
    try {
      const project = await createProjectApi({ name, rootPath, tags: [] });
      setProjects((current) => [...current, project]);
      setSelectedProjectId(project.id);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  function selectTerminalDisplaySize(value: string): void {
    if (!isTerminalDisplaySize(value)) return;
    setTerminalDisplaySize(value);
    window.localStorage.setItem(TERMINAL_DISPLAY_SIZE_KEY, value);
  }

  async function recoverWebGpu(reason: string): Promise<void> {
    if (gpuRecovering()) return;
    setGpuRecovering(true);
    setError(`WebGPU device lost: ${reason}。描画を再構築しています。`);
    try {
      setDevice(await requestWebGpuDevice());
      setError("");
    } catch (caught) {
      setError(
        `${caught instanceof Error ? caught.message : String(caught)}。PTYは継続しています。ブラウザを再読み込みしてください。`,
      );
    } finally {
      setGpuRecovering(false);
    }
  }

  return (
    <Switch>
      <Match when={startup() === "checking"}>
        <StartupCard
          title="起動環境を確認しています"
          detail="WebGPU deviceとローカルデーモンを確認中です。"
        />
      </Match>
      <Match when={startup() === "unsupported"}>
        <StartupCard
          title="このブラウザでは起動できません"
          detail="Panestraは必要機能が揃った対応ブラウザでのみ起動します。PTYは作成されていません。"
        >
          <ul>
            <For each={startupReasons()}>{(reason) => <li>{reason}</li>}</For>
          </ul>
        </StartupCard>
      </Match>
      <Match when={startup() === "authentication_required"}>
        <StartupCard
          title="ランチャーから開いてください"
          detail="このタブにはブラウザセッションがありません。Panestraを再度起動するか、open操作で新しい1回限りのリンクを発行してください。"
        />
      </Match>
      <Match when={startup() === "failed"}>
        <StartupCard title="Panestraを起動できませんでした" detail={error()} />
      </Match>
      <Match when={startup() === "ready" && device() && connection}>
        <main class="app-shell">
          <aside class="sidebar">
            <div class="brand">
              <span class="brand-mark">P</span>
              <strong>Panestra</strong>
            </div>
            <nav>
              <button
                classList={{ selected: !attentionOnly() }}
                onClick={() => setAttentionOnly(false)}
              >
                <span>すべてのセッション</span>
                <b>{sessions().length}</b>
              </button>
              <button
                classList={{ selected: attentionOnly() }}
                onClick={() => setAttentionOnly(true)}
              >
                <span>注意が必要</span>
                <b>
                  {
                    sessions().filter((session) =>
                      needsAttention(session, gitStatuses().get(session.id)),
                    ).length
                  }
                </b>
              </button>
            </nav>
            <div class="project-list">
              <div class="section-heading">
                <span class="eyebrow">PROJECTS</span>
                <button
                  type="button"
                  title="プロジェクトを追加"
                  onClick={() => void addProject()}
                >
                  ＋
                </button>
              </div>
              <button
                classList={{ selected: !selectedProjectId() }}
                onClick={() => setSelectedProjectId(undefined)}
              >
                すべてのプロジェクト
              </button>
              <For each={projects()}>
                {(project) => (
                  <button
                    classList={{ selected: project.id === selectedProjectId() }}
                    onClick={() => setSelectedProjectId(project.id)}
                    title={project.rootPath}
                  >
                    <span
                      class="project-color"
                      style={{ background: project.color ?? "#43c7ac" }}
                    />
                    <span>{project.name}</span>
                  </button>
                )}
              </For>
            </div>
            <div class="session-list">
              <span class="eyebrow">SESSIONS</span>
              <For each={visibleSessions()}>
                {(session) => (
                  <button
                    classList={{ active: session.id === activeId() }}
                    onClick={() => activate(session.id)}
                  >
                    <span
                      class={`state-dot state-${session.agentState ?? session.processState}`}
                    />
                    <span>
                      <strong>{session.name}</strong>
                      <small>{session.launchCwd.split("/").at(-1)}</small>
                    </span>
                    <Show when={session.processState === "running"}>
                      <i
                        onClick={(event) => {
                          event.stopPropagation();
                          void stop(session.id);
                        }}
                        title="終了"
                      >
                        ■
                      </i>
                    </Show>
                    <Show
                      when={
                        session.processState !== "running" &&
                        session.processState !== "starting"
                      }
                    >
                      <i
                        onClick={(event) => {
                          event.stopPropagation();
                          void removeRecord(session.id);
                        }}
                        title="記録を削除"
                      >
                        ×
                      </i>
                    </Show>
                  </button>
                )}
              </For>
            </div>
            <div class="connection-status">
              <span classList={{ online: connectionState() === "connected" }} />
              {connectionLabel(connectionState())}
            </div>
          </aside>
          <section class="workspace">
            <header class="topbar">
              <div>
                <span class="eyebrow">COMMAND CENTER</span>
                <h1>ターミナル管制盤</h1>
              </div>
              <div class="toolbar">
                <button
                  class="secondary"
                  type="button"
                  disabled={!activeId()}
                  onClick={() => setHistoryOpen(true)}
                >
                  履歴
                </button>
                <label>
                  ターミナルサイズ
                  <select
                    value={terminalDisplaySize()}
                    onChange={(event) =>
                      selectTerminalDisplaySize(event.currentTarget.value)
                    }
                  >
                    <option value="small">小</option>
                    <option value="medium">中</option>
                    <option value="large">大</option>
                  </select>
                </label>
                <button class="primary" onClick={() => setDialogOpen(true)}>
                  ＋ 新しいセッション
                </button>
              </div>
            </header>
            <Show when={error()}>
              <button class="error-banner" onClick={() => setError("")}>
                {error()} <span>×</span>
              </button>
            </Show>
            <TerminalGrid
              sessions={visibleSessions()}
              snapshots={snapshots()}
              actionContexts={actionContexts()}
              gitStatuses={gitStatuses()}
              projects={projects()}
              device={device()!}
              connection={connection!}
              activeId={activeId()}
              expandedId={expandedId()}
              displaySize={terminalDisplaySize()}
              leaseOwned={Boolean(
                activeId() &&
                leaseSessionId() === activeId() &&
                !gpuRecovering(),
              )}
              resizeLeaseIds={resizeLeaseSessionIds()}
              onActivate={activate}
              onToggleExpanded={(id) =>
                setExpandedId((current) => (current === id ? undefined : id))
              }
              onBrowseHistory={(id) => {
                setActiveId(id);
                setHistoryOpen(true);
              }}
              onDeviceLost={(reason) => void recoverWebGpu(reason)}
            />
          </section>
          <NewSessionDialog
            open={dialogOpen()}
            home={environment().home}
            shell={environment().shell}
            projects={projects()}
            displaySize={terminalDisplaySize()}
            onClose={() => setDialogOpen(false)}
            onCreate={create}
          />
          <HistoryDrawer
            open={historyOpen()}
            session={sessions().find((session) => session.id === activeId())}
            onClose={() => setHistoryOpen(false)}
          />
        </main>
      </Match>
    </Switch>
  );
}

function readTerminalDisplaySize(): TerminalDisplaySize {
  const value = window.localStorage.getItem(TERMINAL_DISPLAY_SIZE_KEY);
  return isTerminalDisplaySize(value) ? value : "medium";
}

function StartupCard(props: {
  title: string;
  detail: string;
  children?: JSX.Element;
}) {
  return (
    <main class="startup">
      <div class="startup-card">
        <span class="brand-mark">P</span>
        <span class="eyebrow">PANESTRA</span>
        <h1>{props.title}</h1>
        <p>{props.detail}</p>
        {props.children}
      </div>
    </main>
  );
}

function needsAttention(session: Session, git?: GitStatus): boolean {
  return (
    Boolean(git?.conflicts) ||
    ["waiting_approval", "awaiting_user", "integration_error"].includes(
      session.agentState ?? "",
    )
  );
}

function attentionRank(session: Session, git?: GitStatus): number {
  if (session.agentState === "waiting_approval") return 5;
  if (session.agentState === "awaiting_user") return 4;
  if (session.agentState === "integration_error") return 3;
  if (git?.conflicts) return 3;
  return 1;
}

function connectionLabel(state: ConnectionState): string {
  return {
    connecting: "接続中",
    connected: "デーモン接続済み",
    resyncing: "再同期中",
    disconnected: "切断",
  }[state];
}
