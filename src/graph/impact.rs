//! 受影響範圍：動一個符號會波及誰。
//!
//! 呼叫關係回答不了這個問題。把一個結構多加一個欄位，用到它的函數一個
//! 都沒被呼叫，照樣全部得跟著改；改一個 trait 的方法簽名，受害的是所有
//! 實作它的型別。這一層看的是**指向目標的所有邊**——呼叫、型別引用、
//! 實作——因為它們都是「改了目標就得回頭看」的理由。
//!
//! 輸出是摘要不是清單。專案裡光是 `Result` 就有近百個入邊，逐條列會把
//! 原始碼擠掉，而原始碼才是呼叫端真正要的東西。依檔案彙總之後，一個符
//! 號通常只佔幾行。

use rusqlite::Connection;

use crate::error::Result;
use crate::model::SymbolId;

/// 一個檔案裡引用目標的次數。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Users {
    pub file: String,
    pub count: usize,
}

/// 一個符號的受影響範圍。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Impact {
    /// 依引用次數遞減，同次數依路徑。
    pub files: Vec<Users>,
    /// 引用的總數，包含未列出的檔案。
    pub total: usize,
}

impl Impact {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// 統計指向 `target` 的引用，依檔案彙總。
///
/// 自我引用不算：遞迴函數指向自己，那不是「還有誰依賴它」。
pub fn of(conn: &Connection, target: SymbolId) -> Result<Impact> {
    let mut stmt = conn.prepare_cached(
        "SELECT f.path, count(*) FROM relations r
         JOIN files f ON f.id = r.file_id
         WHERE r.dst = ?1 AND r.src != ?1
         GROUP BY f.path
         ORDER BY count(*) DESC, f.path",
    )?;

    let rows = stmt.query_map([target.0], |r| {
        Ok(Users {
            file: r.get(0)?,
            count: r.get::<_, i64>(1)? as usize,
        })
    })?;

    let files: Vec<Users> = rows.collect::<rusqlite::Result<_>>()?;
    let total = files.iter().map(|u| u.count).sum();
    Ok(Impact { files, total })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explore::{query, select};
    use crate::store::Store;
    use crate::testing::resolved;

    /// 依名字找出符號的識別碼。
    fn id_of(store: &Store, name: &str) -> SymbolId {
        let selection = select::select(store.conn(), &query::parse(name)).unwrap();
        selection
            .hits
            .first()
            .unwrap_or_else(|| panic!("找不到 {name}"))
            .id
    }

    #[test]
    fn a_type_used_in_signatures_reports_every_file_that_names_it() {
        let store = resolved(&[
            ("src/a.rs", "pub struct Widget;\n"),
            (
                "src/b.rs",
                "use crate::a::Widget;\npub fn one(w: Widget) {}\npub fn two() -> Widget {\n    Widget\n}\n",
            ),
            (
                "src/c.rs",
                "use crate::a::Widget;\npub fn three(w: &Widget) {}\n",
            ),
        ]);

        let impact = of(store.conn(), id_of(&store, "Widget")).unwrap();
        assert_eq!(impact.files.len(), 2, "{:?}", impact.files);
        assert_eq!(impact.files[0].file, "src/b.rs");
        assert_eq!(impact.files[0].count, 2);
        assert_eq!(impact.files[1].file, "src/c.rs");
        assert_eq!(impact.total, 3);
    }

    /// 呼叫也算依賴：改了簽名，呼叫端就得跟著改。
    #[test]
    fn callers_count_as_impact_too() {
        let store = resolved(&[(
            "src/a.rs",
            "pub fn target() {}\npub fn one() {\n    target();\n}\npub fn two() {\n    target();\n}\n",
        )]);

        let impact = of(store.conn(), id_of(&store, "target")).unwrap();
        assert_eq!(impact.total, 2);
        assert_eq!(impact.files[0].file, "src/a.rs");
    }

    #[test]
    fn a_symbol_nobody_depends_on_is_empty() {
        let store = resolved(&[("src/a.rs", "pub fn lonely() {}\n")]);

        let impact = of(store.conn(), id_of(&store, "lonely")).unwrap();
        assert!(impact.is_empty());
        assert_eq!(impact.total, 0);
    }

    /// 遞迴不是依賴，自己指向自己不該讓一個沒人用的函數看起來有人用。
    #[test]
    fn a_recursive_call_is_not_counted() {
        let store = resolved(&[(
            "src/a.rs",
            "pub fn spin(n: u32) {\n    if n > 0 {\n        spin(n - 1);\n    }\n}\n",
        )]);

        let impact = of(store.conn(), id_of(&store, "spin")).unwrap();
        assert!(impact.is_empty(), "{:?}", impact.files);
    }

    /// 實作一個 trait 就是依賴它：trait 改簽名，實作全部要跟著改。
    #[test]
    fn implementing_a_trait_counts_as_depending_on_it() {
        let store = resolved(&[
            ("src/a.rs", "pub trait Shape {\n    fn area(&self);\n}\n"),
            (
                "src/b.rs",
                "use crate::a::Shape;\npub struct Square;\nimpl Shape for Square {\n    fn area(&self) {}\n}\n",
            ),
        ]);

        let impact = of(store.conn(), id_of(&store, "Shape")).unwrap();
        assert!(
            impact.files.iter().any(|u| u.file == "src/b.rs"),
            "{:?}",
            impact.files
        );
    }

    /// 引用最多的檔案排前面，同樣多則依路徑，順序每次相同。
    #[test]
    fn files_are_ordered_by_weight_then_path() {
        let store = resolved(&[
            ("src/a.rs", "pub struct Widget;\n"),
            (
                "src/z.rs",
                "use crate::a::Widget;\npub fn one(w: Widget) {}\npub fn two(w: Widget) {}\n",
            ),
            (
                "src/m.rs",
                "use crate::a::Widget;\npub fn three(w: Widget) {}\n",
            ),
            (
                "src/b.rs",
                "use crate::a::Widget;\npub fn four(w: Widget) {}\n",
            ),
        ]);

        let impact = of(store.conn(), id_of(&store, "Widget")).unwrap();
        let order: Vec<&str> = impact.files.iter().map(|u| u.file.as_str()).collect();
        assert_eq!(order, ["src/z.rs", "src/b.rs", "src/m.rs"]);
    }
}
