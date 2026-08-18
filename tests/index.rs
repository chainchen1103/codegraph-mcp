//! 全量索引的整合測試：走訪、抽取、寫入三層串起來的行為。

mod common;

use code_graph::store::Store;
use common::{Fixture, query_one};

#[test]
fn a_small_project_is_indexed_end_to_end() {
    let f = Fixture::new("index-small", &[]);
    f.write(
        "src/store.rs",
        "/// 開啟資料庫\npub struct Store;\n\nimpl Store {\n    pub fn open() -> Store {\n        Store\n    }\n}\n",
    );
    f.write("src/util.rs", "pub fn helper() {}\n");
    f.write("README.md", "# 說明\n");

    let (report, store) = f.index();

    assert_eq!(report.files, 2, "只有原始碼檔案該被索引");
    assert_eq!(report.symbols, 3);
    assert!(report.warnings.is_empty());

    // 限定名與種類都要寫進去。
    let kind: i64 = query_one(&store, "SELECT kind FROM symbols WHERE name = 'open'");
    assert_eq!(kind, code_graph::Kind::Method as i64);

    // 文件註解跟著符號一起存。
    let doc: String = query_one(&store, "SELECT docstring FROM symbols WHERE name = 'Store'");
    assert_eq!(doc, "開啟資料庫");
}

#[test]
fn every_symbol_is_reachable_through_full_text_search() {
    let f = Fixture::new("index-fts", &[]);
    f.write(
        "src/a.rs",
        "/// 建立索引\npub fn build_index(root: &str) -> usize {\n    0\n}\n",
    );

    let (_, store) = f.index();

    for query in ["build_index", "建立索引", "usize"] {
        let hits: i64 = query_one(
            &store,
            &format!("SELECT count(*) FROM symbols_fts WHERE symbols_fts MATCH '{query}'"),
        );
        assert!(hits >= 1, "FTS 查不到 `{query}`");
    }
}

#[test]
fn file_rows_carry_the_path_and_content_hash() {
    let f = Fixture::new("index-files", &[]);
    f.write("src/a.rs", "fn a() {}\n");

    let (_, store) = f.index();

    let path: String = query_one(&store, "SELECT path FROM files");
    assert_eq!(path, "src/a.rs", "路徑要相對專案根目錄且以斜線分隔");

    let hash: String = query_one(&store, "SELECT content_hash FROM files");
    assert_eq!(hash.len(), 32);

    let indexed_at: i64 = query_one(&store, "SELECT indexed_at FROM files");
    assert!(indexed_at > 1_577_836_800_000);
}

#[test]
fn every_symbol_has_a_unique_handle() {
    let f = Fixture::new("index-handles", &[]);
    for i in 0..40 {
        f.write(&format!("src/m{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }

    let (report, store) = f.index();
    assert_eq!(report.symbols, 40);

    let distinct: i64 = query_one(&store, "SELECT count(DISTINCT handle) FROM monikers");
    assert_eq!(distinct, 40, "短碼出現重複");

    let total: i64 = query_one(&store, "SELECT count(*) FROM monikers");
    assert_eq!(total, 40);
}

#[test]
fn editing_a_file_updates_its_symbols() {
    let f = Fixture::new("index-edit", &[]);
    f.write("src/a.rs", "fn before() {}\n");

    {
        let (_, store) = f.index();
        let name: String = query_one(&store, "SELECT name FROM symbols");
        assert_eq!(name, "before");
    }

    f.write("src/a.rs", "fn after() {}\nfn extra() {}\n");
    let (report, store) = f.index();

    assert_eq!(report.symbols, 2);
    let names: i64 = query_one(&store, "SELECT count(*) FROM symbols WHERE name = 'before'");
    assert_eq!(names, 0, "舊符號沒有被清掉");
}

#[test]
fn gitignored_directories_never_reach_the_index() {
    let f = Fixture::new("index-ignored", &[]);
    f.write(".gitignore", "vendor/\n");
    f.write("src/a.rs", "fn mine() {}\n");
    f.write("vendor/lib.rs", "fn theirs() {}\n");

    let (report, store) = f.index();

    assert_eq!(report.files, 1);
    let theirs: i64 = query_one(&store, "SELECT count(*) FROM symbols WHERE name = 'theirs'");
    assert_eq!(theirs, 0);
}

#[test]
fn test_files_are_flagged_so_they_can_be_filtered_later() {
    let f = Fixture::new("index-testflag", &[]);
    f.write("src/a.rs", "fn production() {}\n");
    f.write("tests/a.rs", "fn checks() {}\n");

    let (_, store) = f.index();

    let flagged: String = query_one(&store, "SELECT path FROM files WHERE is_test = 1");
    assert_eq!(flagged, "tests/a.rs");
}

/// 索引結果與檔案系統的列舉順序無關，兩次索引的識別碼必須一致。
#[test]
fn two_runs_produce_the_same_identifiers() {
    let f = Fixture::new("index-stable", &[]);
    for name in ["c", "a", "b"] {
        f.write(&format!("src/{name}.rs"), &format!("fn {name}() {{}}\n"));
    }

    let collect = |store: &Store| -> Vec<(i64, String)> {
        let mut stmt = store
            .conn()
            .prepare("SELECT id, moniker FROM monikers ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };

    let (_, store) = f.index();
    let first = collect(&store);
    drop(store);

    let (_, store) = f.index();
    assert_eq!(first, collect(&store));

    // 依路徑排序，與建立檔案的順序無關。
    assert_eq!(
        first
            .iter()
            .map(|(_, m)| m.as_str())
            .collect::<Vec<_>>()
            .first()
            .copied(),
        Some("src/a.rs:function:a:1")
    );
}

#[test]
fn symbols_never_outlive_the_file_they_came_from() {
    let f = Fixture::new("index-cascade", &[]);
    f.write("src/a.rs", "fn a() {}\n");
    f.write("src/b.rs", "fn b() {}\n");

    let (_, store) = f.index();

    store
        .conn()
        .execute("DELETE FROM files WHERE path = 'src/b.rs'", [])
        .unwrap();

    let left: i64 = query_one(&store, "SELECT count(*) FROM symbols");
    assert_eq!(left, 1, "刪除檔案沒有連帶清掉它的符號");
}

/// 混語言 repo：TS 前端 + Python 後端 + Rust 工具，索引不互相污染。
///
/// 同名符號散在三個語言裡，各自的邊只能連到自己那一邊。
#[test]
fn a_mixed_language_repo_keeps_each_language_separate() {
    let f = Fixture::indexed(
        "mixed",
        &[
            (
                "web/api.ts",
                "export function handler(): void {\n  render();\n}\n\
                 export function render(): void {}\n",
            ),
            (
                "api/views.py",
                "def handler():\n    render()\n\ndef render():\n    pass\n",
            ),
            (
                "src/lib.rs",
                "pub fn handler() {\n    render();\n}\npub fn render() {}\n",
            ),
        ],
    );
    let store = f.store();

    // 每個檔案都被認出語言。
    let mut languages: Vec<String> = store
        .conn()
        .prepare("SELECT DISTINCT language FROM files ORDER BY language")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    languages.sort();
    assert_eq!(languages, ["python", "rust", "typescript"]);

    // 三個 handler 各自呼叫自己檔案裡的 render，沒有跨語言接錯。
    let crossed: i64 = store
        .conn()
        .query_row(
            "SELECT count(*) FROM relations r
             JOIN symbols src ON src.id = r.src
             JOIN symbols dst ON dst.id = r.dst
             JOIN files sf ON sf.id = src.file_id
             JOIN files df ON df.id = dst.file_id
             WHERE sf.language != df.language",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(crossed, 0, "有邊跨過了語言邊界");

    let edges: i64 = store
        .conn()
        .query_row("SELECT count(*) FROM relations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(edges, 3, "每個語言各該有一條 handler → render");
}

/// Python 的 `a.b.thing()`：點號在解析階段本來代表「對某個值呼叫方法」，
/// 靠名字比對永遠接不上。import 表指名了 `a.b` 是哪個檔案，就接得上了。
#[test]
fn a_dotted_python_call_resolves_through_its_import() {
    let f = Fixture::indexed(
        "py-import",
        &[
            ("api/util.py", "def thing():\n    return 1\n"),
            (
                "api/views.py",
                "import api.util\n\ndef handler():\n    return api.util.thing()\n",
            ),
        ],
    );
    let store = f.store();

    let edges = edge_names(&store);
    assert_eq!(
        edges,
        [("handler".to_string(), "thing".to_string())],
        "{edges:?}"
    );
}

/// TypeScript 的相對 import：`./utils` 要接到同目錄的那個檔案。
#[test]
fn a_typescript_call_resolves_through_a_relative_import() {
    let f = Fixture::indexed(
        "ts-import",
        &[
            ("web/utils.ts", "export function greet(): void {}\n"),
            (
                "web/app.ts",
                "import { greet } from './utils';\nexport function main(): void {\n  greet();\n}\n",
            ),
        ],
    );
    let store = f.store();

    let edges = edge_names(&store);
    assert_eq!(
        edges,
        [("main".to_string(), "greet".to_string())],
        "{edges:?}"
    );
}

/// 命名空間 import：`helpers.greet()` 的 `helpers` 是整個模組。
#[test]
fn a_namespace_import_resolves_its_members() {
    let f = Fixture::indexed(
        "ts-namespace",
        &[
            ("web/utils.ts", "export function greet(): void {}\n"),
            (
                "web/app.ts",
                "import * as helpers from './utils';\nexport function main(): void {\n  helpers.greet();\n}\n",
            ),
        ],
    );
    let store = f.store();

    let edges = edge_names(&store);
    assert_eq!(
        edges,
        [("main".to_string(), "greet".to_string())],
        "{edges:?}"
    );
}

/// 專案外部的 import 接不上，那不是錯誤——標準函式庫本來就不在索引裡。
#[test]
fn an_external_import_links_to_nothing() {
    let f = Fixture::indexed(
        "ts-external",
        &[(
            "web/app.ts",
            "import React from 'react';\nexport function main(): void {}\n",
        )],
    );
    let store = f.store();

    let linked: i64 = store
        .conn()
        .query_row(
            "SELECT count(*) FROM imports WHERE target_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(linked, 0);
}

/// 邊的兩端名稱。
fn edge_names(store: &Store) -> Vec<(String, String)> {
    let mut stmt = store
        .conn()
        .prepare(
            "SELECT a.name, b.name FROM relations r
             JOIN symbols a ON a.id = r.src
             JOIN symbols b ON b.id = r.dst
             ORDER BY a.name, b.name",
        )
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}
