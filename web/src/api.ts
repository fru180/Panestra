import type {
  ActionContext,
  CreateSessionRequest,
  GitStatus,
  HistoryPage,
  HistorySearchResult,
  Project,
  ScreenSnapshot,
  Session,
} from "./types";

const CREDENTIAL_KEY = "panestra.browserSession";

export function getCredential(): string | null {
  return sessionStorage.getItem(CREDENTIAL_KEY);
}

export function saveCredential(credential: string): void {
  sessionStorage.setItem(CREDENTIAL_KEY, credential);
}

export function clearCredential(): void {
  sessionStorage.removeItem(CREDENTIAL_KEY);
}

export function readBootstrapToken(): string | null {
  const parameters = new URLSearchParams(location.hash.slice(1));
  const token = parameters.get("bootstrap");
  if (token) {
    history.replaceState(null, "", `${location.pathname}${location.search}`);
  }
  return token;
}

export async function exchangeBootstrap(
  token: string,
  capabilities: Record<string, boolean>,
): Promise<string> {
  const response = await fetch("/api/bootstrap", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token, capabilities }),
  });
  const body = await jsonBody<{ credential: string }>(response);
  saveCredential(body.credential);
  return body.credential;
}

export async function listSessions(): Promise<Session[]> {
  return authorizedJson<Session[]>("/api/sessions");
}

export async function getEnvironment(): Promise<{
  home: string;
  shell: string;
}> {
  return authorizedJson("/api/environment");
}

export async function listCurrentActions(): Promise<ActionContext[]> {
  return authorizedJson("/api/actions/current");
}

export async function getGitStatus(sessionId: string): Promise<GitStatus> {
  return authorizedJson(`/api/sessions/${sessionId}/git-status`);
}

export async function getHistory(
  sessionId: string,
  before?: number,
): Promise<HistoryPage> {
  const query = new URLSearchParams({ limit: "10" });
  if (before !== undefined) query.set("before", String(before));
  return authorizedJson(`/api/sessions/${sessionId}/history?${query}`);
}

export async function searchHistory(
  sessionId: string,
  query: string,
): Promise<HistorySearchResult> {
  return authorizedJson(
    `/api/sessions/${sessionId}/history/search?q=${encodeURIComponent(query)}`,
  );
}

export async function exportHistory(sessionId: string): Promise<void> {
  const credential = getCredential();
  if (!credential) throw new Error("ブラウザセッションがありません");
  const response = await fetch(`/api/sessions/${sessionId}/history/export`, {
    headers: { authorization: `Bearer ${credential}` },
  });
  if (!response.ok) await jsonBody<never>(response);
  const url = URL.createObjectURL(await response.blob());
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = `panestra-${sessionId}.txt`;
  anchor.click();
  URL.revokeObjectURL(url);
}

export async function listProjects(): Promise<Project[]> {
  return authorizedJson("/api/projects");
}

export async function createProject(input: {
  name: string;
  rootPath: string;
  color?: string;
  tags?: string[];
}): Promise<Project> {
  return authorizedJson("/api/projects", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(input),
  });
}

export async function createSession(
  request: CreateSessionRequest,
): Promise<Session> {
  return authorizedJson<Session>("/api/sessions", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(request),
  });
}

export async function terminateSession(
  id: string,
  force = false,
): Promise<void> {
  await authorizedJson(`/api/sessions/${id}?force=${String(force)}`, {
    method: "DELETE",
  });
}

export async function deleteSessionRecord(
  id: string,
  includeHistory: boolean,
): Promise<void> {
  await authorizedJson(
    `/api/sessions/${id}/record?includeHistory=${String(includeHistory)}`,
    { method: "DELETE" },
  );
}

export async function getSnapshot(id: string): Promise<ScreenSnapshot> {
  return authorizedJson<ScreenSnapshot>(`/api/sessions/${id}/snapshot`);
}

export async function issueWebSocketTicket(): Promise<{
  ticket: string;
  protocol: string;
}> {
  return authorizedJson("/api/ws-ticket", { method: "POST" });
}

async function authorizedJson<T>(
  path: string,
  init: RequestInit = {},
): Promise<T> {
  const credential = getCredential();
  if (!credential) throw new Error("ブラウザセッションがありません");
  const headers = new Headers(init.headers);
  headers.set("authorization", `Bearer ${credential}`);
  const response = await fetch(path, { ...init, headers });
  if (response.status === 401) clearCredential();
  return jsonBody<T>(response);
}

async function jsonBody<T>(response: Response): Promise<T> {
  if (!response.ok) {
    const error = (await response.json().catch(() => null)) as {
      error?: string;
    } | null;
    throw new Error(error?.error ?? `HTTP ${response.status}`);
  }
  if (response.status === 202 || response.status === 204) return undefined as T;
  return (await response.json()) as T;
}
