//! 查詢的整合測試：索引之後用名字、路徑與自然語言取回原始碼。

mod common;

use code_graph::explore;
use common::Fixture;

/// 索引好的專案加上查詢入口。
trait Ask {
    fn ask(&self, input: &str) -> explore::Exploration;
}

impl Ask for Fixture {
    fn ask(&self, input: &str) -> explore::Exploration {
        explore::explore(&self.project, &self.store(), input).unwrap()
    }
}

const STORE_RS: &str = "\
use std::path::Path;

/// 索引資料庫
pub struct Store {
    path: String,
}

impl Store {
    /// 開啟索引
    pub fn open(path: &Path) -> Store {
        Store {
            path: path.display().to_string(),
        }
    }

    pub fn close(self) {}
}
";

const UTIL_RS: &str = "\
pub fn open() -> u8 {
    0
}

pub fn helper() {}
";

fn fixture(tag: &str) -> Fixture {
    Fixture::indexed(tag, &[("src/store.rs", STORE_RS), ("src/util.rs", UTIL_RS)])
}

#[test]
fn a_qualified_name_returns_exactly_that_symbol() {
    let f = fixture("qualified");
    let out = f.ask("Store::open");

    assert_eq!(out.selection.hits.len(), 1);
    assert!(
        out.text.contains("pub fn open(path: &Path) -> Store"),
        "{}",
        out.text
    );
    assert!(!out.text.contains("pub fn helper"), "{}", out.text);
}

/// 同名的定義要在同一次查詢中全部回傳。
#[test]
fn a_bare_name_returns_every_definition() {
    let f = fixture("bare");
    let out = f.ask("open");

    let names: Vec<&str> = out
        .selection
        .hits
        .iter()
        .map(|h| h.qualified.as_str())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.contains(&"Store::open"));
    assert!(names.contains(&"open"));
}

#[test]
fn source_is_returned_verbatim_with_line_numbers() {
    let f = fixture("verbatim");
    let out = f.ask("Store::open");

    // 行號要對得上原始檔。
    let source = std::fs::read_to_string(f.project.root().join("src/store.rs")).unwrap();
    let expected_line = source
        .lines()
        .position(|l| l.contains("pub fn open"))
        .unwrap() as u32
        + 1;

    assert_eq!(out.selection.hits[0].start_line, expected_line);
    assert!(
        out.text.contains(&format!("{expected_line:>5} |")),
        "{}",
        out.text
    );
}

#[test]
fn a_file_path_returns_everything_declared_in_it() {
    let f = fixture("path");
    let out = f.ask("src/util.rs");

    let names: Vec<&str> = out
        .selection
        .hits
        .iter()
        .map(|h| h.qualified.as_str())
        .collect();
    assert_eq!(names, vec!["open", "helper"]);
}

#[test]
fn a_bare_file_name_is_enough() {
    let f = fixture("filename");
    assert_eq!(f.ask("util.rs").selection.hits.len(), 2);
}

#[test]
fn documentation_text_finds_the_symbol_it_describes() {
    let f = fixture("doc");
    let out = f.ask("索引資料庫");

    assert!(
        out.selection.hits.iter().any(|h| h.qualified == "Store"),
        "{}",
        out.text
    );
}

#[test]
fn several_tokens_are_answered_in_one_call() {
    let f = fixture("multi");
    let out = f.ask("Store::close helper");

    let names: Vec<&str> = out
        .selection
        .hits
        .iter()
        .map(|h| h.qualified.as_str())
        .collect();
    assert!(names.contains(&"Store::close"), "{names:?}");
    assert!(names.contains(&"helper"), "{names:?}");
}

#[test]
fn a_typo_gets_suggestions_instead_of_an_error() {
    let f = fixture("typo");
    let out = f.ask("helpr");

    assert!(out.is_empty());
    assert!(out.text.contains("查無結果"), "{}", out.text);
    assert!(out.text.contains("helper"), "{}", out.text);
}

#[test]
fn results_are_grouped_by_file_and_ordered_by_position() {
    let f = fixture("order");
    let out = f.ask("src/store.rs");

    let lines: Vec<u32> = out.selection.hits.iter().map(|h| h.start_line).collect();
    let mut sorted = lines.clone();
    sorted.sort_unstable();
    assert_eq!(lines, sorted);
}

#[test]
fn the_output_is_identical_across_runs() {
    let f = fixture("stable");
    assert_eq!(f.ask("open").text, f.ask("open").text);
}

/// 超大檔案裡的符號要能單獨取回，而不是被迫接受檔案開頭那一段。
///
/// 這是額度分配最容易寫錯的地方：以檔案為單位截斷的話，一次查詢只會
/// 回到整份檔案的極小一部分，呼叫端只能自己去翻檔案。
#[test]
fn a_symbol_deep_inside_a_huge_file_is_returned_on_its_own() {
    let filler: String = (0..12_000)
        .map(|i| format!("pub fn noise{i}() -> u32 {{\n    {i}\n}}\n\n"))
        .collect();
    assert!(filler.len() > 400_000, "測試檔案不夠大");

    let source = format!("{filler}pub fn needle() -> u32 {{\n    42\n}}\n");
    let f = Fixture::indexed("explore-huge", &[("src/huge.rs", &source)]);

    let out = f.ask("needle");

    assert_eq!(out.selection.hits.len(), 1);
    assert!(out.text.contains("pub fn needle() -> u32"), "{}", out.text);
    assert!(out.text.contains("42"), "{}", out.text);
    assert!(
        !out.text.contains("noise0"),
        "回傳的是檔案開頭而不是要的符號"
    );

    let hit = &out.selection.hits[0];
    assert!(hit.start_line > 10_000, "符號應該落在檔案深處");
}

/// 一個檔案裡命中太多符號時，額度用完的部分要回報數量。
#[test]
fn results_beyond_the_budget_are_reported_not_silently_dropped() {
    let body = "    let value = 1;\n".repeat(30);
    let source: String = (0..60)
        .map(|i| format!("pub fn wide{i}() {{\n{body}}}\n"))
        .collect();
    let f = Fixture::indexed("explore-budget", &[("src/wide.rs", &source)]);

    let out = f.ask("src/wide.rs");

    assert_eq!(out.selection.hits.len(), 60);
    assert!(out.text.contains("未列出"), "{}", out.text);
    assert!(out.text.contains("共 60"), "{}", out.text);
    assert!(
        out.text.chars().count() < out.budget.max_chars * 2,
        "輸出遠超過額度"
    );
}

/// 專案越大額度越寬，且不會反向縮小。
#[test]
fn the_budget_follows_the_size_of_the_project() {
    let f = fixture("budgettier");
    assert_eq!(
        f.ask("open").budget,
        code_graph::explore::budget::for_file_count(2)
    );
}

/// 索引之後才修改的檔案，查詢要回傳磁碟上的新內容。
#[test]
fn edits_made_after_indexing_show_up_immediately() {
    let f = fixture("stale");
    std::fs::write(
        f.project.root().join("src/util.rs"),
        "pub fn open() -> u8 {\n    123\n}\n\npub fn helper() {}\n",
    )
    .unwrap();

    let out = f.ask("src/util.rs");
    assert!(out.text.contains("123"), "{}", out.text);
}

/// 「X 怎麼走到 Y」要在一次查詢裡回答完：路徑、每一跳的位置，以及
/// 路徑上每個符號的原始碼。
#[test]
fn asking_how_one_symbol_reaches_another_answers_in_one_call() {
    let f = Fixture::indexed(
        "explore-flow",
        &[
            (
                "src/cli.rs",
                "use crate::indexer;\n\
                 pub fn run() {\n    indexer::index_project();\n}\n",
            ),
            (
                "src/indexer.rs",
                "use crate::store;\n\
                 pub fn index_project() {\n    store::write_file();\n}\n",
            ),
            ("src/store.rs", "pub fn write_file() {}\n"),
        ],
    );

    let out = f.ask("run write_file");

    let hops: Vec<&str> = out.selection.flows[0]
        .hops
        .iter()
        .map(|h| h.qualified.as_str())
        .collect();
    assert_eq!(
        hops,
        vec!["run", "index_project", "write_file"],
        "{}",
        out.text
    );

    // 路徑排在原始碼前面，每一跳帶著呼叫點。
    let flow_at = out.text.find("## Flow").expect(&out.text);
    assert!(
        flow_at < out.text.find("## Source").unwrap(),
        "{}",
        out.text
    );
    assert!(out.text.contains("src/cli.rs:3"), "{}", out.text);
    assert!(out.text.contains("src/indexer.rs:3"), "{}", out.text);

    // 沒有被指名的中間那一站，原始碼也要一起回來。
    assert!(out.text.contains("pub fn index_project()"), "{}", out.text);
}

/// 連不起來的兩個符號不編造路徑，但兩邊的原始碼照樣回。
#[test]
fn unrelated_symbols_get_source_without_an_invented_path() {
    let f = fixture("noflow");
    let out = f.ask("Store::close helper");

    assert!(out.selection.flows.is_empty());
    assert!(!out.text.contains("## Flow"), "{}", out.text);
    assert!(out.text.contains("pub fn helper"), "{}", out.text);
}

/// 改一個型別會波及誰，靠呼叫關係看不出來——沒有人呼叫「型別」。
#[test]
fn a_type_reports_the_files_that_depend_on_it() {
    let f = Fixture::indexed(
        "blast",
        &[
            ("src/model.rs", "pub struct Widget {\n    pub id: u32,\n}\n"),
            (
                "src/a.rs",
                "use crate::model::Widget;\n\
                 pub fn one(w: &Widget) {}\n\
                 pub fn two() -> Widget {\n    Widget { id: 0 }\n}\n",
            ),
            (
                "src/b.rs",
                "use crate::model::Widget;\npub struct Holder {\n    inner: Widget,\n}\n",
            ),
        ],
    );

    let out = f.ask("Widget");

    // 三個引用散在兩個檔案裡，全部來自宣告而不是呼叫。
    let blast = &out.selection.blast;
    assert_eq!(blast.len(), 1, "{:?}", blast);
    assert_eq!(blast[0].impact.total, 3, "{:?}", blast[0].impact);

    let files: Vec<&str> = blast[0]
        .impact
        .files
        .iter()
        .map(|u| u.file.as_str())
        .collect();
    assert_eq!(files, ["src/a.rs", "src/b.rs"]);

    // 摘要排在原始碼之後，並且指出各檔幾處。
    assert!(out.text.contains("## Blast radius"), "{}", out.text);
    assert!(
        out.text.find("## Source") < out.text.find("## Blast radius"),
        "{}",
        out.text
    );
    assert!(out.text.contains("struct Widget  3 處"), "{}", out.text);
}

/// 沒有人依賴的符號不印空區塊。
#[test]
fn an_unused_symbol_has_no_blast_radius() {
    let f = fixture("blast-none");
    let out = f.ask("Store::close");

    assert!(out.selection.blast.is_empty());
    assert!(!out.text.contains("## Blast radius"), "{}", out.text);
}
