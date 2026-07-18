import { For, Show, createEffect, createSignal } from "solid-js";

import { exportHistory, getHistory, searchHistory } from "./api";
import type { HistoryPage, Session } from "./types";

type Props = {
  session?: Session;
  open: boolean;
  onClose: () => void;
};

export function HistoryDrawer(props: Props) {
  const [page, setPage] = createSignal<HistoryPage>();
  const [query, setQuery] = createSignal("");
  const [matches, setMatches] = createSignal<string[]>();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");

  createEffect(() => {
    if (props.open && props.session) void loadFirst(props.session.id);
  });

  async function loadFirst(id: string): Promise<void> {
    setBusy(true);
    setError("");
    setMatches(undefined);
    try {
      setPage(await getHistory(id));
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  }

  async function loadOlder(): Promise<void> {
    const current = page();
    const session = props.session;
    if (!current?.nextBefore || !session) return;
    setBusy(true);
    try {
      const older = await getHistory(session.id, current.nextBefore);
      setPage({
        ...older,
        chunks: [...current.chunks, ...older.chunks],
      });
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  }

  async function search(): Promise<void> {
    const session = props.session;
    if (!session || !query().trim()) return;
    setBusy(true);
    try {
      setMatches((await searchHistory(session.id, query().trim())).matches);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Show when={props.open && props.session}>
      {(session) => (
        <aside class="history-drawer">
          <header>
            <div>
              <span class="eyebrow">TERMINAL HISTORY</span>
              <h2>{session().name}</h2>
            </div>
            <button type="button" onClick={props.onClose}>
              ×
            </button>
          </header>
          <div class="history-tools">
            <input
              value={query()}
              placeholder="確定済み履歴を検索"
              maxlength={256}
              onInput={(event) => setQuery(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") void search();
              }}
            />
            <button
              class="secondary"
              type="button"
              onClick={() => void search()}
            >
              検索
            </button>
            <button
              class="secondary"
              type="button"
              onClick={() => void exportHistory(session().id)}
            >
              書き出し
            </button>
          </div>
          <Show when={error()}>
            <p class="form-error">{error()}</p>
          </Show>
          <div class="history-content">
            <Show
              when={matches()}
              fallback={
                <For each={page()?.chunks}>
                  {(chunk) => (
                    <pre data-sequence={chunk.sequence}>{chunk.text}</pre>
                  )}
                </For>
              }
            >
              {(results) => (
                <For each={results()}>{(line) => <pre>{line}</pre>}</For>
              )}
            </Show>
            <Show
              when={!busy() && (page()?.chunks.length ?? 0) === 0 && !matches()}
            >
              <p class="history-empty">確定済み履歴はまだありません。</p>
            </Show>
          </div>
          <Show when={!matches() && page()?.truncated}>
            <button
              class="history-more"
              type="button"
              disabled={busy()}
              onClick={() => void loadOlder()}
            >
              さらに古い履歴を取得
            </button>
          </Show>
        </aside>
      )}
    </Show>
  );
}
