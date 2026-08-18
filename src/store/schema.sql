-- CodeGraph 索引 schema。
--
-- 重複出現的字串（moniker、檔案路徑）一律 intern 成整數。若以字串當
-- 主鍵，同一份字串會在 relations 的兩個欄位與各自的索引中重複儲存，
-- 中型專案的索引檔會膨脹數倍。

-- 版本追蹤，migrate 依此決定是否需要升級。
CREATE TABLE IF NOT EXISTS schema_versions (
    version    INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL,
    note       TEXT
);

-- ============================================================
-- 字串池
-- ============================================================

-- moniker 是符號的穩定識別碼，handle 是輸出時使用的短碼。
-- 兩者都必須唯一，intern 依此判斷是新增還是重用。
CREATE TABLE IF NOT EXISTS monikers (
    id      INTEGER PRIMARY KEY,
    moniker TEXT NOT NULL UNIQUE,
    handle  TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS files (
    id           INTEGER PRIMARY KEY,
    path         TEXT NOT NULL UNIQUE,
    unit_id      INTEGER NOT NULL REFERENCES units(id),
    -- 測試檔案。查詢受影響的測試時使用。
    is_test      INTEGER NOT NULL DEFAULT 0,
    -- 產生出來的檔案，排名時降權。
    is_generated INTEGER NOT NULL DEFAULT 0,
    -- 增量同步依此跳過未變更的檔案。
    content_hash TEXT NOT NULL,
    -- 檔案在模組樹中的位置，例如 src/extract/ts.rs 是 extract::ts。
    -- 符號的限定名只記錄檔案內部的巢狀結構，模組層級靠這個欄位補回來。
    module_path  TEXT NOT NULL DEFAULT '',
    -- 認領這個檔案的抽取器，例如 rust / typescript / python。
    language     TEXT NOT NULL DEFAULT '',
    indexed_at   INTEGER NOT NULL
);

-- 編譯單元，例如 Cargo crate、tsconfig 專案、Go module。
-- export_hash 未變更時不需重建下游單元。
CREATE TABLE IF NOT EXISTS units (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    export_hash TEXT NOT NULL DEFAULT ''
);

-- 一條 import 在某個檔案裡引入的名字。
--
-- 解析階段最強的一階線索：作者明寫了「這個名字來自哪個檔案」，比用名字
-- 去猜可靠得多。target_id 由解析階段填上，填不出來表示指向專案外部。
CREATE TABLE IF NOT EXISTS imports (
    file_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    -- 這個檔案裡看得到的寫法。
    local     TEXT NOT NULL,
    -- 目標的種類：0 相對路徑、1 從專案根算起、2 專案外部。
    kind      INTEGER NOT NULL,
    -- 目標的原文，種類為外部時是空字串。
    spec      TEXT NOT NULL,
    -- 解析出來的目標檔案，還沒解析或指向專案外部時為 NULL。
    target_id INTEGER REFERENCES files(id) ON DELETE SET NULL,
    line      INTEGER NOT NULL,
    PRIMARY KEY (file_id, local)
) WITHOUT ROWID;

-- 依名字回查是解析階段的熱路徑。
CREATE INDEX IF NOT EXISTS idx_imports_local ON imports(local);

-- ============================================================
-- 節點
-- ============================================================

-- id 與 monikers.id 相同，同時是 rowid，供 FTS 的 content_rowid 引用。
CREATE TABLE IF NOT EXISTS symbols (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    -- 含容器的名字，例如 Store::open。查詢與輸出都以它為主。
    qualified  TEXT NOT NULL DEFAULT '',
    kind       INTEGER NOT NULL,
    file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    start_line INTEGER NOT NULL,
    end_line   INTEGER NOT NULL,
    signature  TEXT,
    docstring  TEXT
);

-- ============================================================
-- 邊
-- ============================================================

-- line 以 -1 表示沒有位置，例如合成的邊，而不使用 NULL：主鍵包含
-- line，而 SQLite 視每個 NULL 為相異值，會讓同一條邊重複寫入。
--
-- 主鍵包含 line，使同一組 src/dst 在不同行的呼叫各自保留一列。
CREATE TABLE IF NOT EXISTS relations (
    src        INTEGER NOT NULL,
    dst        INTEGER NOT NULL,
    rel        INTEGER NOT NULL,
    line       INTEGER NOT NULL DEFAULT -1,
    file_id    INTEGER,
    provenance INTEGER NOT NULL DEFAULT 0,
    -- 合成器名稱與註冊位置，僅合成的邊使用。
    meta       TEXT,
    PRIMARY KEY (src, dst, rel, line)
) WITHOUT ROWID;

-- ============================================================
-- 未解析引用
-- ============================================================

-- status 0 待解析，1 有多個候選無法確定，2 索引裡沒有這個名字。
--
-- 1 與 2 都保留下來：增量同步只寫一個檔案，現在找不到的目標可能只是
-- 還沒輪到它被寫進來，新符號出現時要能重試。全量索引則直接丟棄 2，
-- 那時所有符號都已寫入，找不到就是專案外部的東西。
--
-- name_tail 是 ref_name 的最後一段，例如 util.greet 取 greet，供重試
-- 時以新符號的名字反查。
CREATE TABLE IF NOT EXISTS unresolved_refs (
    id        INTEGER PRIMARY KEY,
    from_id   INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    ref_name  TEXT NOT NULL,
    name_tail TEXT NOT NULL DEFAULT '',
    rel       INTEGER NOT NULL,
    file_id   INTEGER NOT NULL,
    line      INTEGER NOT NULL,
    status    INTEGER NOT NULL DEFAULT 0
);

-- ============================================================
-- 增量索引的刪除標記
-- ============================================================

-- kind 1 表示符號，2 表示邊。不適用的欄位以 -1 填入，理由同 relations。
CREATE TABLE IF NOT EXISTS tombstones (
    kind INTEGER NOT NULL,
    src  INTEGER NOT NULL,
    dst  INTEGER NOT NULL DEFAULT -1,
    rel  INTEGER NOT NULL DEFAULT -1,
    PRIMARY KEY (kind, src, dst, rel)
) WITHOUT ROWID;

-- ============================================================
-- 全文檢索
-- ============================================================

-- external content 表不會自動同步。批次索引結束時執行
-- INSERT INTO symbols_fts(symbols_fts) VALUES('rebuild')，增量寫入則
-- 依賴下列 trigger。兩者皆缺時搜尋結果會過時且不會報錯。
CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
    name, qualified, signature, docstring,
    content='symbols', content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS symbols_ai AFTER INSERT ON symbols BEGIN
    INSERT INTO symbols_fts(rowid, name, qualified, signature, docstring)
    VALUES (new.id, new.name, new.qualified, new.signature, new.docstring);
END;

CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, name, qualified, signature, docstring)
    VALUES ('delete', old.id, old.name, old.qualified, old.signature, old.docstring);
END;

CREATE TRIGGER IF NOT EXISTS symbols_au AFTER UPDATE ON symbols BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, name, qualified, signature, docstring)
    VALUES ('delete', old.id, old.name, old.qualified, old.signature, old.docstring);
    INSERT INTO symbols_fts(rowid, name, qualified, signature, docstring)
    VALUES (new.id, new.name, new.qualified, new.signature, new.docstring);
END;

-- ============================================================
-- 索引
-- ============================================================

-- 反查 caller 的主要路徑。
CREATE INDEX IF NOT EXISTS idx_relations_dst  ON relations(dst, rel);
CREATE INDEX IF NOT EXISTS idx_symbols_file   ON symbols(file_id);
CREATE INDEX IF NOT EXISTS idx_symbols_name   ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_qualified ON symbols(qualified);
CREATE INDEX IF NOT EXISTS idx_files_unit     ON files(unit_id);
CREATE INDEX IF NOT EXISTS idx_files_module   ON files(module_path);
-- 部分索引：只有待重試的列會被掃到。status 0 的列正要被解析，不需要
-- 靠名字反查。
CREATE INDEX IF NOT EXISTS idx_unresolved_tail
    ON unresolved_refs(name_tail) WHERE status > 0;

-- ============================================================
-- 專案中繼資料
-- ============================================================

CREATE TABLE IF NOT EXISTS project_metadata (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
