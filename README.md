# Panestra

Panestraは、最大25個のローカルターミナルとコーディングエージェントを一画面で監視・操作する、macOS向けのブラウザ管制盤です。

製品要件は[`docs/concept.md`](docs/concept.md)、実装上の判断は[`docs/技術仕様.md`](docs/技術仕様.md)を参照してください。

## 必要環境

- macOS
- [mise](https://mise.jdx.dev/)
- pnpm
- macOS版 Google Chrome 121以降、またはMicrosoft Edge 121以降（WebGPU必須）

## セットアップ

```sh
mise install
mise run install
```

## 開発

```sh
mise run dev
```

デーモンが起動済みの状態で新しいタブを認証して開くには、次を実行します。

```sh
cargo run -p panestra-daemon -- open
```

データディレクトリを変更して起動した場合は、`open`にも同じ`--data-dir`を指定してください。`open`は所有者だけが読める実行時credentialから1回限りのbootstrap URLを発行します。

個別に起動する場合:

```sh
pnpm dev:web
pnpm dev:daemon
```

## ローカル起動

```sh
pnpm build
target/release/panestra-daemon
```

デーモンはloopbackだけで待ち受け、対応ブラウザを1回限りのbootstrap URLで開きます。ブラウザを再読み込みしても同じデーモン上のPTYは継続しますが、デーモンを終了すると管理対象の全PTYとプロセスグループも終了します。

新しいセッションでは任意のコマンド、引数配列、作業ディレクトリを指定できます。文字サイズは一定のまま、小、中、大の表示サイズと拡大領域からPTYの列数・行数を自動算出します。既に表示済みの通常出力は再折り返ししませんが、resize後の出力とSIGWINCH対応アプリは新しい寸法へ追従します。

状態連携付き起動の動作確認範囲は次のとおりです。

- Codex CLI 0.144.x
- Claude Code 2.1.211以上、2.2.0未満

新規セッション画面で「Codex CLI（状態連携付き）」または「Claude Code（状態連携付き）」を選ぶと、そのセッションだけに公式lifecycle hookとPanestra MCPを注入します。CLIがhookの信頼確認を表示した場合は内容を確認して明示的に許可してください。Panestraは信頼確認を迂回しません。対応外バージョンは状態連携付きでは起動できませんが、通常のTerminal内でユーザー自身が起動することは制限しません。

## 検証

```sh
mise run check
mise run test
```

個別にlint、フォーマットを実行する場合:

```sh
pnpm lint
pnpm lint:fix
pnpm format
pnpm format:check
```

`lint:fix`はWebコードに対してESLintの安全な自動修正を適用します。`format`はWebコードをPrettier、Rustコードをrustfmtで整形します。

依存関係のインストール時にHuskyのpre-commit hookが設定されます。コミット前には`pnpm check`と`pnpm test`が実行され、lint、フォーマット、型、テストのいずれかに問題がある場合はコミットを中止します。

`stg`または`main`を対象とするPull Requestでは、同じ検証をGitHub Actionsでも実行します。
