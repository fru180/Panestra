import { For, Show, createEffect, createSignal } from "solid-js";

import type { CreateSessionRequest, Project } from "./types";
import {
  logicalSizeForDisplay,
  type TerminalDisplaySize,
} from "./terminal-geometry";

type Props = {
  open: boolean;
  home: string;
  shell: string;
  projects: Project[];
  displaySize: TerminalDisplaySize;
  onClose: () => void;
  onCreate: (request: CreateSessionRequest) => Promise<void>;
};

export function NewSessionDialog(props: Props) {
  const [name, setName] = createSignal("Terminal");
  const [integration, setIntegration] = createSignal<
    "terminal" | "codex" | "claude"
  >("terminal");
  const [projectId, setProjectId] = createSignal("");
  const [command, setCommand] = createSignal("");
  const [args, setArgs] = createSignal("-l");
  const [cwd, setCwd] = createSignal("");
  const [historyEnabled, setHistoryEnabled] = createSignal(true);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");

  createEffect(() => {
    if (!cwd() && props.home) setCwd(props.home);
    if ((!command() || command() === "/bin/zsh") && props.shell)
      setCommand(props.shell);
  });

  function selectIntegration(value: "terminal" | "codex" | "claude"): void {
    setIntegration(value);
    if (value === "codex") {
      setName("Codex");
      setCommand("codex");
      setArgs("");
    } else if (value === "claude") {
      setName("Claude");
      setCommand("claude");
      setArgs("");
    } else {
      setName("Terminal");
      setCommand(props.shell || "/bin/zsh");
      setArgs("-l");
    }
  }

  async function submit(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    setBusy(true);
    setError("");
    try {
      const selectedIntegration = integration();
      const initialSize = logicalSizeForDisplay(props.displaySize);
      await props.onCreate({
        name: name(),
        projectId: projectId() || undefined,
        command: command(),
        args: splitArguments(args()),
        cwd: cwd(),
        cols: initialSize.cols,
        rows: initialSize.rows,
        historyEnabled: historyEnabled(),
        agentIntegration:
          selectedIntegration === "terminal" ? undefined : selectedIntegration,
      });
      props.onClose();
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Show when={props.open}>
      <div class="dialog-backdrop" onPointerDown={() => props.onClose()}>
        <form
          class="dialog"
          onPointerDown={(event) => event.stopPropagation()}
          onSubmit={(event) => void submit(event)}
        >
          <header>
            <div>
              <span class="eyebrow">NEW SESSION</span>
              <h2>新しいターミナル</h2>
            </div>
            <button
              type="button"
              class="icon-button"
              onClick={() => props.onClose()}
              aria-label="閉じる"
            >
              ×
            </button>
          </header>
          <label>
            セッション種別
            <select
              value={integration()}
              onChange={(event) =>
                selectIntegration(
                  event.currentTarget.value as "terminal" | "codex" | "claude",
                )
              }
            >
              <option value="terminal">Terminal</option>
              <option value="codex">Codex CLI（状態連携付き）</option>
              <option value="claude">Claude Code（状態連携付き）</option>
            </select>
          </label>
          <label>
            プロジェクト
            <select
              value={projectId()}
              onChange={(event) => setProjectId(event.currentTarget.value)}
            >
              <option value="">未分類</option>
              <For each={props.projects}>
                {(project) => (
                  <option value={project.id}>{project.name}</option>
                )}
              </For>
            </select>
          </label>
          <label>
            名前
            <input
              value={name()}
              onInput={(event) => setName(event.currentTarget.value)}
              required
              maxlength={120}
            />
          </label>
          <label>
            コマンド
            <input
              value={command()}
              onInput={(event) => setCommand(event.currentTarget.value)}
              required
            />
          </label>
          <label>
            引数 <span>空白区切り。引用符を利用できます</span>
            <input
              value={args()}
              onInput={(event) => setArgs(event.currentTarget.value)}
            />
          </label>
          <label>
            作業ディレクトリ
            <input
              value={cwd()}
              onInput={(event) => setCwd(event.currentTarget.value)}
              required
            />
          </label>
          <label class="checkbox-field">
            <input
              type="checkbox"
              checked={historyEnabled()}
              onChange={(event) =>
                setHistoryEnabled(event.currentTarget.checked)
              }
            />
            確定済み画面と履歴を保存する
          </label>
          <Show when={error()}>
            <p class="form-error">{error()}</p>
          </Show>
          <footer>
            <button
              type="button"
              class="secondary"
              onClick={() => props.onClose()}
            >
              キャンセル
            </button>
            <button type="submit" class="primary" disabled={busy()}>
              {busy() ? "起動中…" : "起動"}
            </button>
          </footer>
        </form>
      </div>
    </Show>
  );
}

function splitArguments(value: string): string[] {
  const result: string[] = [];
  let current = "";
  let quote: "'" | '"' | null = null;
  let escaped = false;
  for (const character of value.trim()) {
    if (escaped) {
      current += character;
      escaped = false;
      continue;
    }
    if (character === "\\" && quote !== "'") {
      escaped = true;
      continue;
    }
    if (
      (character === "'" || character === '"') &&
      (!quote || quote === character)
    ) {
      quote = quote ? null : character;
      continue;
    }
    if (/\s/.test(character) && !quote) {
      if (current) {
        result.push(current);
        current = "";
      }
      continue;
    }
    current += character;
  }
  if (current) result.push(current);
  return result;
}
