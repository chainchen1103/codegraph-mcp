# CodeGraph

給 AI Agent 用的程式碼結構索引。

Claude Code、Cursor 這類 agent 要找一個函數，常常得打開好幾個上千行的檔案，
一次吞掉大量 token。CodeGraph 先用 tree-sitter 把整個專案解析成符號與關係，
存進本機的 SQLite；agent 透過 MCP 問一次，就拿到相關符號的**逐字原始碼**、
它們之間的**呼叫路徑**，以及改動會波及的**範圍**——不必自己一個一個檔案讀。

索引留在專案裡的 `.codegraph/`，不上傳任何東西。

## 安裝

從 [Releases](https://github.com/chainchen1103/codegraph-mcp/releases) 下載對應平台的壓縮檔，
解開後把 `codegraph`（Windows 是 `codegraph.exe`）放進 `PATH`。

| 平台 | 檔案 |
| ---- | ---- |
| Linux x86_64 | `codegraph-<版本>-x86_64-unknown-linux-musl.tar.gz`（靜態連結，不挑發行版） |
| Windows x86_64 | `codegraph-<版本>-x86_64-pc-windows-msvc.zip` |
| macOS Apple Silicon | `codegraph-<版本>-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `codegraph-<版本>-x86_64-apple-darwin.tar.gz` |

每個 Release 都附 `SHA256SUMS`，下載後可以驗證：

```bash
sha256sum -c SHA256SUMS --ignore-missing
```

或從原始碼建置（需要 Rust 1.88 以上與 C 編譯器）：

```bash
cargo install --git https://github.com/chainchen1103/codegraph-mcp --locked
```

## 快速開始

```bash
cd your-project
codegraph init        # 建立 .codegraph/，不會開始索引
codegraph index       # 全量索引
codegraph explore "Store::open"
```

`explore` 接受符號名、限定名（`Type::method`）、檔案路徑，或自然語言問句。
指名兩個以上的符號時，會先印出它們之間的呼叫路徑。

之後改了程式碼，用增量同步更新，只重新解析變過的檔案：

```bash
codegraph sync          # 同步一次
codegraph sync --watch  # 常駐監看，存檔就更新
```

## 接上 Claude Code / Cursor

```bash
codegraph serve --print-config
```

會印出一段設定，貼進 MCP 設定檔：Claude Code 是專案根目錄的 `.mcp.json`，
Cursor 是 `.cursor/mcp.json`。重啟之後 agent 就看得到三個工具：

| 工具 | 用途 |
| ---- | ---- |
| `explore` | 主入口。回傳相關符號的逐字原始碼（帶行號，可直接據以編輯）、被指名符號之間的呼叫路徑、受影響範圍 |
| `node` | 深挖單一符號：完整本體（不裁切）與它的呼叫者、被呼叫者。同名的多個定義一次全部回傳 |
| `status` | 索引的規模與新鮮度 |

工具刻意只有三個。實測 agent 只會可靠地呼叫一個工具，新能力的做法是讓
`explore` 的答案更完整，而不是再開一個工具。

當前目錄沒有索引時工具照樣存在，回應會說明下一步，而不是回錯誤——
monorepo 只有部分子專案建了索引是常見情況，可以用 `projectPath` 參數指向
任何一個已索引的目錄。**工具不會自行建立索引**，那是使用者的決定。

## 支援的語言

| 語言 | 副檔名 |
| ---- | ---- |
| Rust | `.rs` |
| TypeScript | `.ts` `.tsx` `.mts` `.cts` |
| JavaScript | `.js` `.jsx` `.mjs` `.cjs` |
| Python | `.py` `.pyi` |
| Go | `.go` |
| Java | `.java` |
| Kotlin | `.kt` `.kts` |
| Scala | `.scala` `.sc` |
| C | `.c` |
| C++ | `.cpp` `.cc` `.cxx` `.hpp` `.hh` `.h` 等 |
| CUDA | `.cu` `.cuh` |

同一個 repo 裡混用多個語言沒有問題，各語言的符號與關係不會互相串接——除了
本來就共用程式碼的那幾組：C / C++ / CUDA 共用 header，TypeScript 與
JavaScript 互相 import，Java / Kotlin / Scala 在同一個 JVM 上。

`.h` 用 C++ 的文法解析：副檔名分不出這個 header 是給誰用的，而 C++ 的文法
涵蓋 C 的宣告。C/C++ 的宣告與定義分居兩個檔案時，查詢會落在有本體的那一
個，並且記著它對應的宣告在哪裡。

## 指令一覽

| 指令 | 說明 |
| ---- | ---- |
| `codegraph init [PATH]` | 建立索引目錄 |
| `codegraph index [PATH]` | 全量索引 |
| `codegraph sync [PATH] [--watch]` | 增量同步 |
| `codegraph status [PATH]` | 索引狀態 |
| `codegraph explore <查詢> [--path PATH]` | 查詢符號的原始碼與呼叫路徑 |
| `codegraph callers <符號> [--path PATH]` | 誰呼叫了它 |
| `codegraph callees <符號> [--path PATH]` | 它呼叫了誰 |
| `codegraph outline <檔案>` | 單一檔案的結構骨架 |
| `codegraph serve [--print-config]` | 以 stdio 提供 MCP 服務 |

`PATH` 省略時從工作目錄往上找 `.codegraph/`，遇到 repo 邊界（`.git`）就停。

## 現況

早期版本。索引格式還在演進，升級後若 `status` 或查詢回報 schema 版本不相容，
刪掉 `.codegraph/` 重新 `codegraph index` 即可——索引隨時可以從原始碼重建。

規劃中但尚未提供的：PHP / Swift / Dart / Vue 等更多語言、CI 上預先建好索引供
開發端下載、PR 審查時列出 `git diff` 看不見的受影響呼叫端。
