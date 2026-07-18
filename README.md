# Panestra

Panestra brings your terminals and coding agents together in one browser dashboard. See what needs your attention, jump into any session instantly, and keep parallel development moving without juggling windows.

Panestraは、複数のターミナルとコーディングエージェントをブラウザの一画面にまとめ、状況の確認から直接操作まで行えるツールです。対応が必要なセッションにすぐ気づき、ウィンドウを行き来せずに並行作業をスムーズに進められます。

> [!NOTE]
> Panestra is currently under development as an initial release. It is designed for single-user, local use on macOS and must not be exposed to an external network.

## Features

- Monitor up to 25 terminal sessions in a responsive grid
- Interact with any pane in place, or expand one when you need more room
- Resize PTYs automatically across small, medium, and large layouts
- Track Codex CLI and Claude Code through their official lifecycle hooks
  - Initializing
  - Working
  - Waiting for approval
  - Awaiting user input
  - Integration error
- Highlight, prioritize, and filter sessions that need attention
- Organize sessions by project
- Show agent-reported tasks, working directories, and optional Git branches
- Display changed files, additions, deletions, ahead/behind counts, and conflicts
- Save, search, and export terminal history
- Reconnect the browser while the same daemon keeps running
- Use IME input, copy and paste, text selection, scrolling, and mouse controls

## Requirements

- macOS
- [mise](https://mise.jdx.dev/)
- pnpm 10.15.1
- Google Chrome 121 or later, or Microsoft Edge 121 or later
  - WebGPU, Web Workers, binary WebSockets, the Clipboard API, and IME support are required
- To use agent state integration, one of the following:
  - Codex CLI 0.144.x
  - Claude Code 2.1.211 or later, but earlier than 2.2.0

mise installs Node.js 24.18.0 and Rust 1.97.1 for this project.

## Quick Start

Install the toolchains and project dependencies:

```sh
mise install
mise run install
```

Start the web app and daemon in development mode:

```sh
mise run dev
```

The daemon listens on `127.0.0.1:4317` and opens a one-time authentication URL in your browser. Select **＋ 新しいセッション** (New Session) in the top-right corner to choose a command, arguments, working directory, and optional agent integration.

Press `Ctrl+C` in the terminal running Panestra to stop the daemon. Stopping the daemon also terminates every PTY and process group managed by Panestra.

### Open another browser tab

If the daemon is already running, open a new authenticated tab with:

```sh
cargo run -p panestra-daemon -- open
```

If the daemon uses a custom data directory, pass the same path to `open`:

```sh
cargo run -p panestra-daemon -- open --data-dir /path/to/data
```

The `open` command reads an owner-only runtime credential and issues a new one-time bootstrap URL.

## Usage

### 1. Create sessions

Select **＋ 新しいセッション** (New Session), then choose one of the following session types:

- **Terminal** for shells, development servers, tests, logs, SSH, or any other local command
- **Codex CLI（状態連携付き）** (with state integration) to launch Codex with session-scoped Panestra hooks and MCP configuration
- **Claude Code（状態連携付き）** (with state integration) to launch Claude Code with session-scoped Panestra hooks and MCP configuration

You can assign the session to a project, give it a name, set its command and arguments, choose its working directory, and enable or disable history storage.

### 2. Monitor and organize work

Every running session remains visible in the grid. Use the project sidebar to narrow the dashboard, or select **注意が必要** (Needs Attention) to focus on approval requests, agents awaiting input, integration errors, and Git conflicts.

Choose a terminal size to change the grid density. Panestra keeps the font size stable and resizes each PTY to match its available area. Expand a pane when you want to inspect or operate one session in more detail.

### 3. Interact with a terminal

Select a pane and type directly into it. The active pane supports keyboard and IME input, copy and paste, text selection, scrolling, and mouse events. Other sessions remain visible so you can keep monitoring them while you work.

### 4. Use agent state integration

When you launch an integrated Codex CLI or Claude Code session, Panestra reads official lifecycle-hook events to display the agent state. It does not infer state from terminal text, colors, prompts, or ANSI output.

Panestra also provides a session-scoped MCP server that lets the agent report a task summary, working directory, and optional Git branch. MCP calls are controlled by the agent, so action details may not be available for every turn and do not affect hook-based state tracking.

If the CLI asks you to trust a hook, review it and approve it explicitly. Panestra never bypasses hook trust prompts. Unsupported agent versions cannot start with state integration, but you can still run them manually inside a regular Terminal session.

### 5. Review history and end sessions

Select an active session and open **履歴** (History) to browse, search, or export its saved terminal history. History is stored locally only when it was enabled for that session.

Reloading or closing the browser does not stop a PTY while the same daemon is running. Deleting a session terminates its managed process and removes it from the dashboard while preserving its stored record and history. PTYs cannot be reattached after the daemon restarts.

## Production Build

Build the web app and release daemon:

```sh
pnpm build
```

Run the daemon from the repository root:

```sh
target/release/panestra-daemon
```

Common daemon options:

| Option               | Description                                          | Default                                  |
| -------------------- | ---------------------------------------------------- | ---------------------------------------- |
| `--listen <ADDRESS>` | Listen address; must be an explicit loopback address | `127.0.0.1:4317`                         |
| `--data-dir <PATH>`  | Database, history, and runtime credential directory  | `~/Library/Application Support/Panestra` |
| `--web-dir <PATH>`   | Built web app directory                              | `web/dist`                               |
| `--no-open`          | Do not open the browser at startup                   | Disabled                                 |

Example:

```sh
target/release/panestra-daemon \
  --listen 127.0.0.1:8080 \
  --data-dir /path/to/data \
  --web-dir /path/to/web/dist \
  --no-open
```

## Development

### Commands

| Command           | Description                                      |
| ----------------- | ------------------------------------------------ |
| `mise run dev`    | Start the web app and daemon                     |
| `pnpm dev:web`    | Start only the Vite development server           |
| `pnpm dev:daemon` | Start only the Rust daemon                       |
| `mise run check`  | Check formatting, lint, and TypeScript types     |
| `mise run test`   | Run all web and Rust tests                       |
| `pnpm build`      | Build the web app and release daemon             |
| `pnpm format`     | Format the project with Prettier and rustfmt     |
| `pnpm lint:fix`   | Apply ESLint's automatic fixes to the web source |

Installing dependencies configures a Husky pre-commit hook. The hook runs `pnpm check` and `pnpm test`, and rejects commits with formatting, lint, type, or test failures. GitHub Actions runs the same checks for pull requests targeting `stg` or `main`.

### Project structure

```text
Panestra
├── crates/panestra-daemon/  Rust HTTP/WebSocket server, PTY management, and persistence
└── web/                     SolidJS, TypeScript, and WebGPU browser interface
```

The browser interface connects to the Rust daemon over HTTP APIs and a binary WebSocket. The daemon manages PTYs, terminal state, agent integrations, Git metadata, and history. A Web Worker processes screen data before the browser renders all terminal panes to a single WebGPU canvas.

## Security and Limitations

- The daemon only accepts an explicit loopback listen address
- Browser authentication, WebSocket tickets, hooks, and MCP use separate credentials
- Bootstrap URLs and WebSocket tickets are single-use and expire
- The data directory and runtime credentials are created with owner-only permissions
- Linux, Windows, multi-user operation, and remote access are not supported
- Emoji, image, and video rendering are not supported in terminals
- Panestra cannot guarantee termination of descendants that deliberately daemonize or detach from the managed process group

## License

Panestra is released under the [MIT License](LICENSE).
