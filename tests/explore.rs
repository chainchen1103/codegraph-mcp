//! 查詢的整合測試：索引之後用名字、路徑與自然語言取回原始碼。

use code_graph::explore;
use code_graph::indexer;
use code_graph::project::Project;
use code_graph::store::Store;

struct Fixture {
    project: Project,
    store: Store,
}

impl Fixture {
    fn new(tag: &str, files: &[(&str, &str)]) -> Self {
        let dir =
            std::env::temp_dir().join(format!("codegraph-it-explore-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let project = Project::create(&dir).unwrap();

        for (rel, body) in files {
            let path = project.root().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }

        let mut store = Store::open(&project.db_path()).unwrap();
        indexer::index_project(&project, &mut store).unwrap();

        Self { project, store }
    }

    fn ask(&self, input: &str) -> explore::Exploration {
        explore::explore(&self.project, &self.store, input).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.project.root()).ok();
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
    Fixture::new(tag, &[("src/store.rs", STORE_RS), ("src/util.rs", UTIL_RS)])
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
