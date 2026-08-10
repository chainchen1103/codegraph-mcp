-- CodeGraph schema。對應 DESIGN.md §8.2。
--
-- 設計要點：所有重複出現的字串（moniker、檔案路徑）都 intern 成 integer。
-- 若用 TEXT 主鍵，同一份字串會在 relations 的兩個欄位加上各自的索引裡
-- 出現 6 次以上，中型 repo 會從 ~15MB 膨脹到 100MB+，直接毀掉
-- 「幾 MB 壓縮索引」這個核心承諾（DESIGN.md §8.1）。

-- 版本追蹤。migrate.rs 依此決定要不要升級。
CREATE TABLE IF NOT EXISTS schema_versions (
    version    INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL,
    note       TEXT
);

-- =============================================================
-- 0. 字串池
-- =============================================================

-- moniker 是符號的穩定識別碼字串，只在寫入期用來 intern 成 id。
-- handle 是 blake3(moniker) 前 6 hex，輸出用的穩定錨點。
-- 兩者都 UNIQUE：intern 靠這個做 upsert。
CREATE TABLE IF NOT EXISTS monikers (
    id      INTEGER PRIMARY KEY,
    moniker TEXT NOT NULL UNIQUE,
    handle  TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS files (
    id           INTEGER PRIMARY KEY,
    path         TEXT NOT NULL UNIQUE,
    unit_id      INTEGER NOT NULL REFERENCES units(id),
    -- get_tests_for_symbol / diff_impact 的受影響測試清單依賴這個標記。
    is_test      INTEGER NOT NULL DEFAULT 0,
    -- 產生出來的檔案（*.pb.go 等）排名要降權，但不排除。
    is_generated INTEGER NOT NULL DEFAULT 0,
    -- 增量同步靠它跳過未變更的檔案。
    content_hash TEXT NOT NULL,
    indexed_at   INTEGER NOT NULL
);

-- 編譯單元：Cargo crate / tsconfig / go module。
-- 增量重建的最小單位，export_hash 不變則停止向下游級聯（DESIGN.md §3.4）。
CREATE TABLE IF NOT EXISTS units (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    export_hash TEXT NOT NULL DEFAULT ''
);

-- =============================================================
-- 1. 節點
-- =============================================================

-- id == monikers.id。symbols.id 是 INTEGER PRIMARY KEY，
-- 也就是 rowid 本身，FTS5 的 content_rowid 直接指它。
CREATE TABLE IF NOT EXISTS symbols (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    kind       INTEGER NOT NULL,
    file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    start_line INTEGER NOT NULL,
    end_line   INTEGER NOT NULL,
    signature  TEXT,
    docstring  TEXT
);

-- =============================================================
-- 2. 邊
-- =============================================================

-- line 用 NOT NULL DEFAULT -1（-1 代表「沒有座標」，例如合成邊），
-- 而不是可為 NULL。原因：主鍵含 line，而 SQLite 視每個 NULL 為相異值，
-- 兩次索引產生的同一條無座標邊會變成兩列重複資料。
--
-- 主鍵含 line 是刻意的：同一個 caller 在不同行呼叫同一個 callee
-- 要保留成不同的邊（呼叫點上下文需要），同時天然去重。
CREATE TABLE IF NOT EXISTS relations (
    src        INTEGER NOT NULL,
    dst        INTEGER NOT NULL,
    rel        INTEGER NOT NULL,
    line       INTEGER NOT NULL DEFAULT -1,
    file_id    INTEGER,
    provenance INTEGER NOT NULL DEFAULT 0,
    -- 僅 heuristic 邊使用：合成器名稱與註冊位置。
    meta       TEXT,
    PRIMARY KEY (src, dst, rel, line)
) WITHOUT ROWID;

-- =============================================================
-- 3. 未解析引用
-- =============================================================

-- 生命週期（DESIGN.md §4.2）：抽取時以 status=0 (pending) 寫入；
-- 解析 pass 結束後解出來的刪列，解不出的標 status=1 (failed) 保留。
-- 之後某次同步引入新符號時，用 failed 列重試——這是修復
-- 「先寫 caller、後寫 callee」這種編輯順序的唯一辦法。
--
-- name_tail 是 reference_name 的最後一段（'util.greet' → 'greet'），
-- 讓重試能用新符號的名字反查。
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

-- =============================================================
-- 4. PR 增量專用（只有 pr.db 會用到）
-- =============================================================

CREATE TABLE IF NOT EXISTS tombstones (
    kind INTEGER NOT NULL,     -- 1=symbol 2=relation
    src  INTEGER NOT NULL,
    dst  INTEGER NOT NULL DEFAULT -1,
    rel  INTEGER NOT NULL DEFAULT -1,
    PRIMARY KEY (kind, src, dst, rel)
) WITHOUT ROWID;

-- =============================================================
-- 5. 全文檢索
-- =============================================================

-- external-content 表**不會自動更新**。
-- 全量索引結尾必須跑：INSERT INTO symbols_fts(symbols_fts) VALUES('rebuild');
-- 增量路徑則靠下面的 trigger。漏掉任一種，搜尋結果會靜默過時
-- 且沒有任何錯誤訊息（DESIGN.md §8.4）。
CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
    name, signature, docstring,
    content='symbols', content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS symbols_ai AFTER INSERT ON symbols BEGIN
    INSERT INTO symbols_fts(rowid, name, signature, docstring)
    VALUES (new.id, new.name, new.signature, new.docstring);
END;

CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, name, signature, docstring)
    VALUES ('delete', old.id, old.name, old.signature, old.docstring);
END;

CREATE TRIGGER IF NOT EXISTS symbols_au AFTER UPDATE ON symbols BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, name, signature, docstring)
    VALUES ('delete', old.id, old.name, old.signature, old.docstring);
    INSERT INTO symbols_fts(rowid, name, signature, docstring)
    VALUES (new.id, new.name, new.signature, new.docstring);
END;

-- =============================================================
-- 6. 索引
-- =============================================================

-- 反向查 caller 的主力路徑。
CREATE INDEX IF NOT EXISTS idx_relations_dst  ON relations(dst, rel);
CREATE INDEX IF NOT EXISTS idx_symbols_file   ON symbols(file_id);
CREATE INDEX IF NOT EXISTS idx_symbols_name   ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_files_unit     ON files(unit_id);
-- 部分索引：只有 failed 的列會被重試掃到，生成的索引小很多。
CREATE INDEX IF NOT EXISTS idx_unresolved_tail
    ON unresolved_refs(name_tail) WHERE status = 1;

-- =============================================================
-- 7. 專案中繼資料
-- =============================================================

-- base_commit / indexed_at / handle_len 等。
CREATE TABLE IF NOT EXISTS project_metadata (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
