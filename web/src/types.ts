export type AgentIntegration = "codex" | "claude";
export type ProcessState = "starting" | "running" | "exited" | "killed";
export type AgentState =
  | "initializing"
  | "working"
  | "waiting_approval"
  | "awaiting_user"
  | "integration_error";

export type Session = {
  id: string;
  projectId?: string;
  name: string;
  command: string;
  args: string[];
  launchCwd: string;
  currentCwd?: string;
  agentIntegration?: AgentIntegration;
  agentState?: AgentState;
  currentActionId?: string;
  processState: ProcessState;
  pid?: number;
  cols: number;
  rows: number;
  daemonEpoch: string;
  sessionGeneration: string;
  historyEnabled: boolean;
  createdAt: string;
  lastActivityAt?: string;
  exitedAt?: string;
  exitCode?: number;
  exitReason?: string;
};

export type ScreenSnapshot = {
  sessionId: string;
  daemonEpoch: string;
  sessionGeneration: string;
  snapshotEpoch: string;
  sequenceNumber: number;
  cols: number;
  rows: number;
  cursorRow: number;
  cursorCol: number;
  contents: string;
  styledCells: Array<
    [
      row: number,
      col: number,
      text: string,
      width: number,
      foreground: number | null,
      background: number | null,
      attributes: number,
    ]
  >;
  alternateScreen: boolean;
  applicationCursor: boolean;
  applicationKeypad: boolean;
  bracketedPaste: boolean;
  mouseReporting: boolean;
  focusReporting: boolean;
  mouseEncoding: "sgr" | "utf8" | "default" | "";
  hideCursor: boolean;
};

export type CreateSessionRequest = {
  name: string;
  projectId?: string;
  command: string;
  args: string[];
  cwd: string;
  agentIntegration?: AgentIntegration;
  cols?: number;
  rows?: number;
  historyEnabled: boolean;
};

export type Project = {
  id: string;
  name: string;
  rootPath: string;
  color?: string;
  tags: string[];
  repositoryUrl?: string;
  createdAt: string;
  updatedAt: string;
};

export type ActionContext = {
  id: string;
  schemaVersion: 1;
  sessionId: string;
  taskSummary: string;
  repositoryPath: string;
  gitBranch?: string;
  gitWorkingTreeRoot?: string;
  acceptedAt: string;
};

export type GitStatus = {
  workingTreeRoot: string;
  actualBranch?: string;
  staged: number;
  unstaged: number;
  untracked: number;
  conflicts: number;
  changedFiles: number;
  additions: number;
  deletions: number;
  ahead?: number;
  behind?: number;
  stale: boolean;
  error?: string;
  updatedAt: string;
};

export type HistoryPage = {
  sessionId: string;
  chunks: Array<{ sequence: number; text: string }>;
  nextBefore?: number;
  truncated: boolean;
};

export type HistorySearchResult = {
  sessionId: string;
  query: string;
  matches: string[];
  truncated: boolean;
};
