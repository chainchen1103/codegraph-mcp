//! 查詢入口：輸入名字或問題，取得相關符號的逐字原始碼。

pub mod budget;
pub mod query;
pub mod render;
pub mod select;

use crate::error::Result;
use crate::project::Project;
use crate::store::Store;

/// 一次查詢的結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exploration {
    pub selection: select::Selection,
    /// 這次查詢所依據的額度。
    pub budget: budget::Budget,
    /// 已排版、可直接輸出的文字。
    pub text: String,
}

impl Exploration {
    /// 是否有找到任何符號。
    pub fn is_empty(&self) -> bool {
        self.selection.hits.is_empty()
    }
}

/// 依查詢字串取回相關符號。
///
/// 查無結果不是錯誤：回傳的文字會列出相近的名稱，讓呼叫端有下一步
/// 可走。
pub fn explore(project: &Project, store: &Store, input: &str) -> Result<Exploration> {
    let parsed = query::parse(input);
    let selection = select::select(store.conn(), &parsed)?;
    let budget = budget::for_file_count(store.stats()?.files.max(0) as usize);
    let text = render::render(project.root(), &selection, budget);

    Ok(Exploration {
        selection,
        budget,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{cleanup, indexed_project};

    struct Fixture {
        project: Project,
        store: Store,
    }

    impl Fixture {
        fn new(tag: &str, files: &[(&str, &str)]) -> Self {
            let project = indexed_project(&format!("explore-{tag}"), files);
            let store = Store::open(&project.db_path()).unwrap();
            Self { project, store }
        }

        fn explore(&self, input: &str) -> Exploration {
            explore(&self.project, &self.store, input).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            cleanup(&self.project);
        }
    }

    #[test]
    fn a_name_returns_its_source_verbatim() {
        let f = Fixture::new(
            "verbatim",
            &[(
                "src/store.rs",
                "pub struct Store;\n\nimpl Store {\n    pub fn open() -> u8 {\n        42\n    }\n}\n",
            )],
        );

        let out = f.explore("Store::open");
        assert!(!out.is_empty());
        assert!(
            out.text.contains("      4 |     pub fn open() -> u8 {"),
            "{}",
            out.text
        );
        assert!(out.text.contains("      5 |         42"), "{}", out.text);
    }

    #[test]
    fn every_symbol_sharing_a_name_comes_back_in_one_call() {
        let f = Fixture::new(
            "overloads",
            &[
                ("src/a.rs", "pub fn run() {}\n"),
                (
                    "src/b.rs",
                    "pub struct B;\nimpl B {\n    pub fn run() {}\n}\n",
                ),
            ],
        );

        let out = f.explore("run");
        assert_eq!(out.selection.hits.len(), 2, "{}", out.text);
        assert!(out.text.contains("src/a.rs"));
        assert!(out.text.contains("src/b.rs"));
    }

    #[test]
    fn a_file_path_returns_the_whole_file_structure() {
        let f = Fixture::new(
            "bypath",
            &[("src/a.rs", "pub fn one() {}\npub fn two() {}\n")],
        );

        let out = f.explore("src/a.rs");
        assert_eq!(out.selection.hits.len(), 2);
    }

    #[test]
    fn an_unknown_name_returns_guidance_not_an_error() {
        let f = Fixture::new("unknown", &[("src/a.rs", "pub fn opened() {}\n")]);

        let out = f.explore("opend");
        assert!(out.is_empty());
        assert!(out.text.contains("查無結果"), "{}", out.text);
        assert!(
            out.text.contains("opened"),
            "應該給出相近的名稱：{}",
            out.text
        );
    }

    #[test]
    fn the_same_query_always_produces_the_same_output() {
        let f = Fixture::new(
            "stable",
            &[
                ("src/a.rs", "pub fn run() {}\n"),
                ("src/b.rs", "pub fn run_twice() {}\n"),
            ],
        );

        assert_eq!(f.explore("run").text, f.explore("run").text);
    }

    #[test]
    fn an_empty_query_reports_that_nothing_was_asked() {
        let f = Fixture::new("empty", &[("src/a.rs", "pub fn one() {}\n")]);

        let out = f.explore("   ");
        assert!(out.is_empty());
        assert!(out.text.contains("查無結果"), "{}", out.text);
    }

    /// 原始碼一律從磁碟讀取，索引之後的修改要立刻反映出來。
    #[test]
    fn source_comes_from_disk_not_from_the_index() {
        let f = Fixture::new("fresh", &[("src/a.rs", "pub fn one() {\n    1\n}\n")]);

        std::fs::write(
            f.project.root().join("src/a.rs"),
            "pub fn one() {\n    999\n}\n",
        )
        .unwrap();

        let out = f.explore("one");
        assert!(
            out.text.contains("999"),
            "回傳的是索引時的舊內容：{}",
            out.text
        );
    }
}
