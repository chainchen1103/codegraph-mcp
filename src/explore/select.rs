//! 從索引挑出要回傳的符號。

use rusqlite::Connection;

use super::query::Query;
use crate::error::Result;
use crate::graph::path::{self, Path as CallPath};
use crate::model::{Kind, SymbolId};

/// 全文檢索最多回傳的符號數。
const TEXT_SEARCH_LIMIT: usize = 20;

/// 查無結果時建議的候選數。
const SUGGESTION_LIMIT: usize = 8;

/// 最多回報的呼叫路徑數。
const PATH_LIMIT: usize = 4;

/// 單一查詢名最多拿幾個符號去試路徑。
///
/// 名字有大量同名定義時，兩兩配對的次數會平方成長。路徑的價值在指名的
/// 那幾個符號之間，多試沒有回報。
const ENDPOINTS_PER_NAME: usize = 4;

/// 符號被選中的原因。決定輸出額度的分配順序。
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// 位於某條呼叫路徑上。
    Flow,
    /// 使用者直接指名的符號。
    Named,
    /// 使用者指定的檔案裡的符號。
    Path,
    /// 全文檢索找到的符號。
    Text,
}

/// 一個被選中的符號。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub id: SymbolId,
    pub name: String,
    pub qualified: String,
    pub kind: Kind,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: Option<String>,
    pub docstring: Option<String>,
    pub origin: Origin,
}

/// 一次挑選的結果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub hits: Vec<Hit>,
    /// 被指名的符號之間的呼叫路徑。
    pub flows: Vec<CallPath>,
    /// 查無結果的詞。
    pub unmatched: Vec<String>,
    /// 給查無結果的詞的候選名稱。
    pub suggestions: Vec<String>,
}

/// 依查詢挑出符號。
///
/// 名字先精確比對，沒有結果才忽略大小寫再試一次。命中的那一階會回傳
/// 全部符合的符號，同名的多個定義不會被截掉。
pub fn select(conn: &Connection, query: &Query) -> Result<Selection> {
    let mut selection = Selection::default();
    // 每個查詢名各自的命中，用來配對呼叫路徑的兩端。
    let mut endpoints: Vec<Vec<SymbolId>> = Vec::new();

    for name in &query.names {
        let hits = by_name(conn, name, Origin::Named)?;
        if hits.is_empty() {
            selection.unmatched.push(name.clone());
        } else {
            endpoints.push(hits.iter().take(ENDPOINTS_PER_NAME).map(|h| h.id).collect());
            selection.hits.extend(hits);
        }
    }

    selection.flows = flows(conn, &endpoints)?;
    let on_flows: Vec<SymbolId> = selection
        .flows
        .iter()
        .flat_map(|p| p.hops.iter().map(|h| h.id))
        .collect();
    selection
        .hits
        .extend(by_ids(conn, &on_flows, Origin::Flow)?);

    for path in &query.paths {
        let hits = by_path(conn, path, Origin::Path)?;
        if hits.is_empty() {
            selection.unmatched.push(path.clone());
        } else {
            selection.hits.extend(hits);
        }
    }

    if !query.text.is_empty() {
        let hits = by_text(conn, &query.text, Origin::Text)?;
        if hits.is_empty() {
            selection.unmatched.extend(query.text.iter().cloned());
        } else {
            selection.hits.extend(hits);
        }
    }

    // 沒有比對到的詞若剛好是唯一的查詢內容，補上相近的名稱讓呼叫端
    // 有下一步可走。
    if selection.hits.is_empty() && !selection.unmatched.is_empty() {
        selection.suggestions = suggest(conn, &selection.unmatched)?;
    }

    dedup_and_sort(&mut selection.hits);
    Ok(selection)
}

/// 依名字比對，先精確再忽略大小寫。
///
/// 精確這一階同時比對限定名與裸名，兩者不分先後。查 `open` 時
/// `Store::open` 與模組層的 `open` 都要回傳，只回其中一個等於逼呼叫端
/// 自己去翻檔案挑。查 `Store::open` 時裸名不可能相等，自然只會命中
/// 那一個方法。
fn by_name(conn: &Connection, name: &str, origin: Origin) -> Result<Vec<Hit>> {
    for condition in [
        "s.qualified = ?1 OR s.name = ?1",
        "lower(s.qualified) = lower(?1) OR lower(s.name) = lower(?1)",
    ] {
        let hits = collect(
            conn,
            &symbol_query(condition),
            rusqlite::params![name],
            origin,
        )?;
        if !hits.is_empty() {
            return Ok(hits);
        }
    }
    Ok(Vec::new())
}

fn by_path(conn: &Connection, path: &str, origin: Origin) -> Result<Vec<Hit>> {
    let normalized = crate::extract::moniker::normalize_path(path);

    for condition in [
        "f.path = ?1",
        // 只給了檔名時，比對路徑結尾。
        "f.path = ?1 OR f.path LIKE '%/' || ?1",
    ] {
        let hits = collect(
            conn,
            &symbol_query(condition),
            rusqlite::params![normalized],
            origin,
        )?;
        if !hits.is_empty() {
            return Ok(hits);
        }
    }
    Ok(Vec::new())
}

/// 依識別碼取出符號。
fn by_ids(conn: &Connection, ids: &[SymbolId], origin: Origin) -> Result<Vec<Hit>> {
    let sql = symbol_query("s.id = ?1");
    let mut out = Vec::new();

    for id in ids {
        out.extend(collect(conn, &sql, rusqlite::params![id.0], origin)?);
    }
    Ok(out)
}

/// 找出被指名的符號之間的呼叫路徑。
///
/// 只配對來自不同查詢名的兩端：同一個名字的多個定義之間即使連得起來，
/// 也不是使用者問的那條路徑。
fn flows(conn: &Connection, endpoints: &[Vec<SymbolId>]) -> Result<Vec<CallPath>> {
    let mut out = Vec::new();

    for (index, left) in endpoints.iter().enumerate() {
        for right in endpoints.iter().skip(index + 1) {
            if out.len() >= PATH_LIMIT {
                return Ok(out);
            }
            if let Some(found) = between(conn, left, right)? {
                out.push(found);
            }
        }
    }

    Ok(out)
}

/// 兩組符號之間的第一條路徑。
///
/// 兩個方向都試：使用者打兩個名字時不會特意按呼叫方向排列。
fn between(conn: &Connection, left: &[SymbolId], right: &[SymbolId]) -> Result<Option<CallPath>> {
    for a in left {
        for b in right {
            if let Some(found) = path::shortest(conn, *a, *b)? {
                return Ok(Some(found));
            }
            if let Some(found) = path::shortest(conn, *b, *a)? {
                return Ok(Some(found));
            }
        }
    }
    Ok(None)
}

/// 取出組成 [`Hit`] 所需欄位的查詢，條件由呼叫端給。
///
/// 條件只來自本檔案內的字面值，不含任何外部輸入。
fn symbol_query(condition: &str) -> String {
    format!("{SYMBOL_COLUMNS} FROM symbols s JOIN files f ON f.id = s.file_id WHERE {condition}")
}

/// [`Hit`] 需要的欄位，順序與 [`collect`] 的讀取順序一致。
const SYMBOL_COLUMNS: &str = "SELECT s.id, s.name, s.qualified, s.kind, f.path,
                                     s.start_line, s.end_line, s.signature, s.docstring";

fn by_text(conn: &Connection, words: &[String], origin: Origin) -> Result<Vec<Hit>> {
    let expression = words
        .iter()
        .map(|w| format!("\"{}\"", w.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" OR ");

    let sql = format!(
        "{SYMBOL_COLUMNS}
         FROM symbols_fts
         JOIN symbols s ON s.id = symbols_fts.rowid
         JOIN files f ON f.id = s.file_id
         WHERE symbols_fts MATCH ?1
         ORDER BY bm25(symbols_fts)
         LIMIT ?2"
    );

    collect(
        conn,
        &sql,
        rusqlite::params![expression, TEXT_SEARCH_LIMIT as i64],
        origin,
    )
}

/// 找出與查無結果的詞相近的名稱。
///
/// 同時用兩種方式比對：查詢詞是名稱的一部分，以及名稱與查詢詞開頭
/// 相同。後者用來接住打錯字的情況，`opend` 這種輸入不是任何名稱的
/// 子字串，但與 `opened` 共用前綴。
fn suggest(conn: &Connection, missing: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();

    for token in missing {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT qualified FROM symbols
             WHERE lower(name) LIKE '%' || lower(?1) || '%'
                OR lower(qualified) LIKE '%' || lower(?1) || '%'
                OR lower(name) LIKE lower(?2) || '%'
             ORDER BY length(qualified), qualified
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![token, shared_prefix(token), SUGGESTION_LIMIT as i64],
            |r| r.get::<_, String>(0),
        )?;
        for row in rows {
            let name = row?;
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }

    out.truncate(SUGGESTION_LIMIT);
    Ok(out)
}

/// 用來比對相近名稱的前綴。
///
/// 取查詢詞的前三分之二，太短的詞不做前綴比對，否則幾乎整個索引都會
/// 被列成候選。
fn shared_prefix(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    let len = chars.len() * 2 / 3;
    if len < 3 {
        // 不可能出現在名稱裡的字串，等於停用這一項條件。
        return "\u{0}".to_string();
    }
    chars[..len].iter().collect()
}

fn collect(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
    origin: Origin,
) -> Result<Vec<Hit>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params, |r| {
        let raw_kind: u8 = r.get(3)?;
        Ok(Hit {
            id: SymbolId(r.get(0)?),
            name: r.get(1)?,
            qualified: r.get(2)?,
            kind: Kind::from_u8(raw_kind).unwrap_or(Kind::Function),
            file: r.get(4)?,
            start_line: r.get(5)?,
            end_line: r.get(6)?,
            signature: r.get(7)?,
            docstring: r.get(8)?,
            origin,
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// 去除重複並排序。
///
/// 排序鍵固定為檔案、起始行、名稱，讓同一個查詢每次的輸出完全相同。
/// 同一個符號被多種方式選中時，保留優先度最高的來源。
fn dedup_and_sort(hits: &mut Vec<Hit>) {
    hits.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.start_line.cmp(&b.start_line))
            .then(a.name.cmp(&b.name))
            .then(a.origin.cmp(&b.origin))
    });
    hits.dedup_by_key(|h| h.id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explore::query;
    use crate::store::Store;
    use crate::testing::indexed;

    const UTIL_RS: &str = "\
/// 開啟資料庫
pub fn open() -> u8 {
    1
}
";

    const STORE_RS: &str = "\
pub struct Store;

impl Store {
    /// 開啟索引
    pub fn open(path: &str) -> u8 {
        2
    }

    pub fn close(&self) {}
}
";

    fn sample_store() -> Store {
        indexed(&[("src/util.rs", UTIL_RS), ("src/store.rs", STORE_RS)])
    }

    fn run(store: &Store, input: &str) -> Selection {
        select(store.conn(), &query::parse(input)).unwrap()
    }

    fn qualified(selection: &Selection) -> Vec<&str> {
        selection
            .hits
            .iter()
            .map(|h| h.qualified.as_str())
            .collect()
    }

    #[test]
    fn a_qualified_name_matches_exactly_one_symbol() {
        let store = sample_store();
        let selection = run(&store, "Store::open");

        assert_eq!(qualified(&selection), vec!["Store::open"]);
        assert_eq!(selection.hits[0].kind, Kind::Method);
        assert_eq!(selection.hits[0].file, "src/store.rs");
        assert!(selection.unmatched.is_empty());
    }

    /// 同名的符號要一次全部回傳，呼叫端才不必自己去翻檔案挑。
    #[test]
    fn a_bare_name_returns_every_symbol_with_that_name() {
        let store = sample_store();
        let selection = run(&store, "open");

        assert_eq!(qualified(&selection), vec!["Store::open", "open"]);
    }

    #[test]
    fn matching_falls_back_to_case_insensitive() {
        let store = sample_store();
        let selection = run(&store, "STORE::OPEN");

        assert_eq!(qualified(&selection), vec!["Store::open"]);
    }

    #[test]
    fn a_path_returns_every_symbol_in_that_file() {
        let store = sample_store();
        let selection = run(&store, "src/store.rs");

        assert_eq!(
            qualified(&selection),
            vec!["Store", "Store::open", "Store::close"]
        );
    }

    #[test]
    fn a_bare_file_name_matches_by_suffix() {
        let store = sample_store();
        let selection = run(&store, "util.rs");

        assert_eq!(qualified(&selection), vec!["open"]);
    }

    #[test]
    fn natural_language_falls_back_to_full_text_search() {
        let store = sample_store();
        let selection = run(&store, "開啟索引");

        assert!(
            qualified(&selection).contains(&"Store::open"),
            "{:?}",
            qualified(&selection)
        );
    }

    #[test]
    fn results_are_sorted_by_file_and_line() {
        let store = sample_store();
        let selection = run(&store, "src/store.rs src/util.rs");

        let order: Vec<(String, u32)> = selection
            .hits
            .iter()
            .map(|h| (h.file.clone(), h.start_line))
            .collect();
        let mut expected = order.clone();
        expected.sort();
        assert_eq!(order, expected);
    }

    #[test]
    fn the_same_symbol_matched_twice_appears_once() {
        let store = sample_store();
        let selection = run(&store, "Store::open src/store.rs");

        let count = selection
            .hits
            .iter()
            .filter(|h| h.qualified == "Store::open")
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn an_unknown_name_is_reported_with_suggestions() {
        let store = sample_store();
        let selection = run(&store, "opne");

        assert!(selection.hits.is_empty());
        assert_eq!(selection.unmatched, vec!["opne"]);

        let selection = run(&store, "clos");
        assert_eq!(selection.suggestions, vec!["Store::close"]);
    }

    #[test]
    fn a_partly_matched_query_still_returns_what_it_found() {
        let store = sample_store();
        let selection = run(&store, "Store::open nonexistent_symbol");

        assert_eq!(qualified(&selection), vec!["Store::open"]);
        assert_eq!(selection.unmatched, vec!["nonexistent_symbol"]);
        assert!(selection.suggestions.is_empty(), "有命中時不需要再給候選");
    }

    #[test]
    fn quotes_in_a_text_query_do_not_break_the_search() {
        let store = sample_store();
        let selection = run(&store, "\"開啟索引\"");
        assert!(!selection.hits.is_empty());
    }

    /// 前綴比對只在查詢詞夠長時啟用。
    #[test]
    fn short_tokens_do_not_trigger_prefix_suggestions() {
        assert_eq!(shared_prefix("opend"), "ope");
        assert_eq!(shared_prefix("abcdef"), "abcd");
        assert_ne!(shared_prefix("ab"), "a");
        assert_ne!(shared_prefix("x"), "");
    }

    /// 中間隔了一層的兩個符號，路徑要把中間那一層帶出來。
    #[test]
    fn two_named_symbols_get_the_call_path_between_them() {
        let store = crate::testing::resolved(&[(
            "src/a.rs",
            "pub fn sink() {}\n\
             pub fn middle() {\n    sink();\n}\n\
             pub fn entry() {\n    middle();\n}\n",
        )]);
        let selection = run(&store, "entry sink");

        assert_eq!(selection.flows.len(), 1);
        let hops: Vec<&str> = selection.flows[0]
            .hops
            .iter()
            .map(|h| h.qualified.as_str())
            .collect();
        assert_eq!(hops, vec!["entry", "middle", "sink"]);
    }

    /// 橋接節點沒有被指名，但它的原始碼是看懂這條路徑的必要條件。
    #[test]
    fn symbols_on_a_path_are_selected_even_though_nobody_named_them() {
        let store = crate::testing::resolved(&[(
            "src/a.rs",
            "pub fn sink() {}\n\
             pub fn middle() {\n    sink();\n}\n\
             pub fn entry() {\n    middle();\n}\n",
        )]);
        let selection = run(&store, "entry sink");

        let bridge = selection
            .hits
            .iter()
            .find(|h| h.qualified == "middle")
            .expect("橋接節點沒有進到結果裡");
        assert_eq!(bridge.origin, Origin::Flow);
    }

    /// 路徑上的符號優先度最高，即使它同時是被指名的那一個。
    #[test]
    fn being_on_a_path_outranks_being_named() {
        let store = crate::testing::resolved(&[(
            "src/a.rs",
            "pub fn sink() {}\npub fn entry() {\n    sink();\n}\n",
        )]);
        let selection = run(&store, "entry sink");

        assert!(selection.hits.iter().all(|h| h.origin == Origin::Flow));
    }

    /// 使用者不會特意按呼叫方向排列兩個名字。
    #[test]
    fn the_two_names_can_be_given_in_either_order() {
        let store = crate::testing::resolved(&[(
            "src/a.rs",
            "pub fn sink() {}\npub fn entry() {\n    sink();\n}\n",
        )]);

        assert_eq!(
            run(&store, "sink entry").flows,
            run(&store, "entry sink").flows
        );
    }

    #[test]
    fn a_single_name_never_produces_a_path() {
        let store = sample_store();
        assert!(run(&store, "Store::open").flows.is_empty());
    }

    #[test]
    fn unrelated_names_produce_no_path() {
        let store = crate::testing::resolved(&[("src/a.rs", "pub fn one() {}\npub fn two() {}\n")]);
        assert!(run(&store, "one two").flows.is_empty());
    }

    /// 檔案路徑與自然語言不是路徑的端點，只有指名的符號是。
    #[test]
    fn only_named_symbols_are_used_as_endpoints() {
        let store = crate::testing::resolved(&[(
            "src/a.rs",
            "pub fn sink() {}\npub fn entry() {\n    sink();\n}\n",
        )]);
        assert!(run(&store, "src/a.rs sink").flows.is_empty());
    }

    /// 指名的符號一多，兩兩配對的路徑數會失控，只回報前幾條。
    #[test]
    fn the_number_of_reported_paths_is_capped() {
        let mut source = String::new();
        for i in 0..6 {
            match i {
                5 => source.push_str("pub fn f5() {}\n"),
                _ => source.push_str(&format!("pub fn f{i}() {{\n    f{}();\n}}\n", i + 1)),
            }
        }
        let store = crate::testing::resolved(&[("src/a.rs", &source)]);

        let selection = run(&store, "f0 f1 f2 f3 f4 f5");
        assert_eq!(selection.flows.len(), PATH_LIMIT);
    }
}
