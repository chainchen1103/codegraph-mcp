//! 把引用的名字對應到索引裡的符號。

use std::sync::OnceLock;

use rusqlite::Connection;

use crate::error::Result;
use crate::model::{Kind, Provenance, Rel, SymbolId};

/// 一次比對的結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Match {
    /// 找到唯一的目標，以及這個結論有多可靠。
    One(SymbolId, Provenance),
    /// 有多個同樣合理的目標。不猜，交給呼叫端記錄下來。
    Ambiguous,
    /// 索引裡沒有這個名字，通常是專案外部的函數。
    None,
}

/// 引用名字的最後一段。
///
/// `a::b::greet` 與 `obj.greet` 都取 `greet`。
pub fn tail(name: &str) -> &str {
    let after_path = name.rsplit("::").next().unwrap_or(name);
    after_path.rsplit('.').next().unwrap_or(after_path)
}

/// 把引用對應到符號。
///
/// 步驟由可靠到勉強：
///
/// 1. 由長到短嘗試限定名。呼叫寫成什麼樣子取決於檔案頂端 import 了哪
///    一層，`crate::store::Store::open` 與 `Store::open` 指的是同一個
///    東西，逐段去掉前綴就能對上，不必另外維護 import 表。
/// 2. 原始碼裡寫了容器卻對不到任何符號，表示那個容器不屬於這個專案，
///    例如 `Vec::new`。直接判為外部，不再往下猜。
/// 3. 只有名字的引用退到比對最後一段。接收者的型別查不到時，這一步
///    只在同一個檔案裡進行。
pub fn resolve(conn: &Connection, ref_name: &str, from_file: i64, rel: Rel) -> Result<Match> {
    let style = Style::of(ref_name);

    for suffix in suffixes(ref_name) {
        match lookup(conn, Field::Qualified, &suffix, from_file, rel, style)? {
            Match::None => continue,
            found => return Ok(found),
        }
    }

    // 作者已經指明容器，而那個容器不在索引裡。
    if ref_name.contains("::") {
        return Ok(Match::None);
    }

    lookup(conn, Field::Name, tail(ref_name), from_file, rel, style)
}

/// 由長到短的後綴，只剝掉模組路徑。
///
/// 要剝的是 import 層級的差異：`crate::store::Store::open` 與
/// `Store::open` 是同一個東西。但剝到型別就得停 —— `Vec::push` 再剝一
/// 段會變成 `push`，然後落在專案裡剛好叫 `push` 的自由函數上。
///
/// 用命名慣例區分：模組是小寫，型別是大寫開頭。這是 Rust 一致遵守的
/// 慣例，而位置本身分不出這兩者。
fn suffixes(name: &str) -> Vec<String> {
    let segments: Vec<&str> = name.split("::").collect();
    let mut out = vec![name.to_string()];

    for i in 0..segments.len().saturating_sub(1) {
        if !is_module_like(segments[i]) {
            break;
        }
        out.push(segments[i + 1..].join("::"));
    }

    out
}

/// 這一段看起來是模組而不是型別。
fn is_module_like(segment: &str) -> bool {
    segment.chars().next().is_some_and(|c| !c.is_uppercase())
}

/// 呼叫的寫法。
///
/// 只靠名字無從得知接收者的型別，但寫法本身已經排除了一部分候選：
/// `warnings.push(..)` 不可能落在專案裡的自由函數 `push` 上，
/// `helper()` 也不會是某個型別的方法。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Style {
    /// `foo()` 或 `a::b::foo()`。
    Direct,
    /// `receiver.method()`。
    Receiver,
}

impl Style {
    fn of(ref_name: &str) -> Self {
        if ref_name.contains('.') {
            Style::Receiver
        } else {
            Style::Direct
        }
    }
}

/// 任何呼叫都不可能落在的種類。
const BASE_EXCLUDES: [Kind; 3] = [Kind::Trait, Kind::TypeAlias, Kind::Module];

/// 只寫了名字的直接呼叫，額外排除方法。
///
/// 方法要嘛寫出限定名，要嘛透過接收者呼叫；`helper()` 兩者都不是。
const DIRECT_EXCLUDES: [Kind; 4] = [Kind::Trait, Kind::TypeAlias, Kind::Module, Kind::Method];

/// 透過接收者呼叫時，只有方法可能是目標。
const RECEIVER_EXCLUDES: [Kind; 6] = [
    Kind::Trait,
    Kind::TypeAlias,
    Kind::Module,
    Kind::Function,
    Kind::Struct,
    Kind::Enum,
];

/// 比對的欄位。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Field {
    Qualified,
    Name,
}

impl Field {
    fn column(self) -> &'static str {
        match self {
            Field::Qualified => "qualified",
            Field::Name => "name",
        }
    }
}

/// 先在同一個檔案裡找，再擴大到整個專案。
///
/// 同名符號散落在多個檔案時，發出引用的那個檔案裡的符號最可能是目標。
/// 測試檔案裡到處都有同名的輔助函數，沒有這一層就會全部變成有歧義。
///
/// 同檔比對還多一種寫法：符號的限定名比引用長。在 `mod tests` 裡呼叫
/// `Fixture::new`，符號的限定名是 `tests::Fixture::new`，多出來的前綴
/// 在符號那一側，逐段縮短引用永遠對不上。這種比對只在同一個檔案裡做，
/// 放到全專案會鬆到開始猜。
fn lookup(
    conn: &Connection,
    field: Field,
    value: &str,
    from_file: i64,
    rel: Rel,
    style: Style,
) -> Result<Match> {
    match unique_in_file(conn, field, value, from_file, rel, style)? {
        Match::None => {}
        found => return Ok(found),
    }

    // 只有帶容器的寫法才做結尾比對。單獨一個名字去比對任何容器的結尾，
    // 等於把 `new` 接到專案裡隨便一個建構函數上。
    if field == Field::Qualified && value.contains("::") {
        match suffix_in_file(conn, value, from_file, rel, style)? {
            Match::None => {}
            found => return Ok(found),
        }
    }

    // 接收者的型別查不到，就不拿方法名去比對整個專案。
    //
    // 抽取階段查得到型別的呼叫已經寫成 `Type::method`，還帶著句點就
    // 表示查不到。此時「全專案剛好只有一個同名方法」不是證據：
    // `r.get(0)` 的 `r` 是外部函式庫的型別，專案裡碰巧也有 `get`，
    // 接上去就是一條錯的邊。同檔案裡有同名方法仍然算數，那是弱但
    // 真實的上下文。
    if field == Field::Name && style == Style::Receiver {
        return Ok(Match::None);
    }

    unique_anywhere(conn, field, value, rel, style)
}

/// 同一個檔案裡，限定名以 `value` 結尾的符號。
fn suffix_in_file(
    conn: &Connection,
    value: &str,
    file_id: i64,
    rel: Rel,
    style: Style,
) -> Result<Match> {
    let sql = format!(
        "SELECT id FROM symbols WHERE qualified LIKE '%::' || ?1 AND file_id = ?2{} LIMIT 2",
        kind_filter(rel, Field::Qualified, style)
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    first_or_ambiguous(&mut stmt, rusqlite::params![value, file_id])
}

fn unique_in_file(
    conn: &Connection,
    field: Field,
    value: &str,
    file_id: i64,
    rel: Rel,
    style: Style,
) -> Result<Match> {
    let sql = format!(
        "SELECT id FROM symbols WHERE {} = ?1 AND file_id = ?2{} LIMIT 2",
        field.column(),
        kind_filter(rel, field, style)
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    first_or_ambiguous(&mut stmt, rusqlite::params![value, file_id])
}

fn unique_anywhere(
    conn: &Connection,
    field: Field,
    value: &str,
    rel: Rel,
    style: Style,
) -> Result<Match> {
    let sql = format!(
        "SELECT id FROM symbols WHERE {} = ?1{} LIMIT 2",
        field.column(),
        kind_filter(rel, field, style)
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    first_or_ambiguous(&mut stmt, rusqlite::params![value])
}

/// 依關係種類、比對欄位與呼叫寫法排除不可能的目標。
///
/// 比對限定名時只排除語法上不可能的種類：`Store::open` 就是呼叫方法的
/// 正常寫法。退到比對裸名時，寫法才進一步限制候選。
///
/// 回傳的 SQL 片段由排除清單產生，不含任何外部輸入。
fn kind_filter(rel: Rel, field: Field, style: Style) -> &'static str {
    if rel != Rel::Calls {
        return "";
    }
    match (field, style) {
        (Field::Qualified, _) => base_clause(),
        (Field::Name, Style::Direct) => direct_clause(),
        (Field::Name, Style::Receiver) => receiver_clause(),
    }
}

fn base_clause() -> &'static str {
    static CLAUSE: OnceLock<String> = OnceLock::new();
    CLAUSE
        .get_or_init(|| exclude_clause(&BASE_EXCLUDES))
        .as_str()
}

fn direct_clause() -> &'static str {
    static CLAUSE: OnceLock<String> = OnceLock::new();
    CLAUSE
        .get_or_init(|| exclude_clause(&DIRECT_EXCLUDES))
        .as_str()
}

fn receiver_clause() -> &'static str {
    static CLAUSE: OnceLock<String> = OnceLock::new();
    CLAUSE
        .get_or_init(|| exclude_clause(&RECEIVER_EXCLUDES))
        .as_str()
}

fn exclude_clause(kinds: &[Kind]) -> String {
    let ids: Vec<String> = kinds.iter().map(|k| (*k as u8).to_string()).collect();
    format!(" AND kind NOT IN ({})", ids.join(", "))
}

/// 取最多兩列就夠了：有沒有第二列決定唯一或有歧義。
fn first_or_ambiguous(
    stmt: &mut rusqlite::CachedStatement<'_>,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Match> {
    let mut rows = stmt.query(params)?;
    let first = match rows.next()? {
        Some(row) => SymbolId(row.get(0)?),
        None => return Ok(Match::None),
    };
    match rows.next()? {
        Some(_) => Ok(Match::Ambiguous),
        None => Ok(Match::One(first, Provenance::Static)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn store_with(symbols: &[(&str, &str, Kind, i64)]) -> Store {
        let store = Store::in_memory().unwrap();
        store
            .conn()
            .execute_batch(
                "INSERT INTO units(id, name) VALUES (1, 'root');
                 INSERT INTO files(id, path, unit_id, content_hash, indexed_at)
                     VALUES (1, 'src/a.rs', 1, 'h', 0), (2, 'src/b.rs', 1, 'h', 0);",
            )
            .unwrap();

        for (i, (name, qualified, kind, file)) in symbols.iter().enumerate() {
            store
                .conn()
                .execute(
                    "INSERT INTO symbols(id, name, qualified, kind, file_id, start_line, end_line)
                     VALUES (?1, ?2, ?3, ?4, ?5, 1, 2)",
                    rusqlite::params![i as i64 + 1, name, qualified, *kind as u8, file],
                )
                .unwrap();
        }
        store
    }

    fn call(store: &Store, name: &str, from_file: i64) -> Match {
        resolve(store.conn(), name, from_file, Rel::Calls).unwrap()
    }

    fn certain(id: u32) -> Match {
        Match::One(SymbolId(id), Provenance::Static)
    }

    #[test]
    fn the_tail_is_the_last_segment() {
        assert_eq!(tail("greet"), "greet");
        assert_eq!(tail("a::b::greet"), "greet");
        assert_eq!(tail("obj.greet"), "greet");
        assert_eq!(tail("crate::a::obj.greet"), "greet");
    }

    #[test]
    fn module_prefixes_are_stripped_but_types_are_kept() {
        assert_eq!(
            suffixes("crate::store::Store::open"),
            vec![
                "crate::store::Store::open",
                "store::Store::open",
                "Store::open"
            ]
        );

        // 模組底下的自由函數：剝到只剩函數名是對的。
        assert_eq!(
            suffixes("walk::source_files"),
            vec!["walk::source_files", "source_files"]
        );

        // 型別不剝。
        assert_eq!(suffixes("Vec::push"), vec!["Vec::push"]);
        assert_eq!(suffixes("Store::open"), vec!["Store::open"]);
        assert_eq!(suffixes("open"), vec!["open"]);
    }

    #[test]
    fn the_module_convention_is_by_first_letter() {
        assert!(is_module_like("store"));
        assert!(is_module_like("_private"));
        assert!(!is_module_like("Store"));
        assert!(!is_module_like(""));
    }

    /// 容器不在索引裡的呼叫不能退到裸名比對，否則 `Vec::push` 會落在
    /// 專案裡剛好叫 `push` 的自由函數上。
    #[test]
    fn an_external_container_never_falls_through_to_a_bare_function() {
        let s = store_with(&[("push", "push", Kind::Function, 1)]);
        assert_eq!(call(&s, "Vec::push", 1), Match::None);
    }

    #[test]
    fn a_qualified_reference_matches_the_qualified_name() {
        let s = store_with(&[
            ("open", "Store::open", Kind::Method, 1),
            ("open", "Cache::open", Kind::Method, 2),
        ]);
        assert_eq!(call(&s, "Store::open", 2), certain(1));
    }

    /// import 的層級不同，寫法就不同，但指的是同一個符號。
    #[test]
    fn a_longer_path_falls_back_to_a_shorter_one() {
        let s = store_with(&[("open", "Store::open", Kind::Method, 1)]);
        assert_eq!(call(&s, "crate::store::Store::open", 1), certain(1));
    }

    #[test]
    fn a_bare_name_matches_when_it_is_unique() {
        let s = store_with(&[("helper", "helper", Kind::Function, 1)]);
        assert_eq!(call(&s, "helper", 2), certain(1));
    }

    /// 猜錯的邊會讓查詢結果指向不相干的程式碼，寧可留著不解析。
    #[test]
    fn several_equally_good_candidates_are_left_unresolved() {
        let s = store_with(&[
            ("open", "A::open", Kind::Method, 1),
            ("open", "B::open", Kind::Method, 1),
        ]);
        assert_eq!(call(&s, "x.open", 1), Match::Ambiguous);
    }

    /// 同名符號分散在多個檔案時，同一個檔案裡的那一個優先。
    #[test]
    fn a_symbol_in_the_same_file_wins_over_one_elsewhere() {
        let s = store_with(&[
            ("helper", "A::helper", Kind::Method, 1),
            ("helper", "B::helper", Kind::Method, 2),
        ]);
        assert_eq!(call(&s, "x.helper", 2), certain(2));
    }

    /// 呼叫寫在容器內部時，符號的限定名會比引用長。
    #[test]
    fn a_symbol_nested_deeper_than_the_reference_is_found_in_the_same_file() {
        let s = store_with(&[
            ("new", "tests::Fixture::new", Kind::Method, 1),
            ("new", "Fixture::new", Kind::Method, 2),
        ]);
        assert_eq!(call(&s, "Fixture::new", 1), certain(1));
    }

    /// 這種比對只在同一個檔案裡做。跨檔案放寬會開始猜。
    #[test]
    fn a_deeper_symbol_in_another_file_is_not_matched_by_suffix() {
        let s = store_with(&[("new", "tests::Fixture::new", Kind::Method, 1)]);
        assert_eq!(call(&s, "Fixture::new", 2), Match::None);
    }

    /// 限定名也適用同檔優先：測試檔案裡常有同名的輔助型別。
    #[test]
    fn a_qualified_name_also_prefers_the_current_file() {
        let s = store_with(&[
            ("new", "Fixture::new", Kind::Method, 1),
            ("new", "Fixture::new", Kind::Method, 2),
        ]);
        assert_eq!(call(&s, "Fixture::new", 2), certain(2));
    }

    #[test]
    fn an_unknown_name_matches_nothing() {
        let s = store_with(&[("helper", "helper", Kind::Function, 1)]);
        assert_eq!(call(&s, "unwrap", 1), Match::None);
    }

    /// 原始碼寫出了容器，而容器不在索引裡，那就是外部的東西。
    /// 退回去比對最後一段的話，`Vec::new` 會被接到專案自己的建構函數上。
    #[test]
    fn an_explicit_container_outside_the_project_is_not_guessed_by_name() {
        let s = store_with(&[
            ("new", "Writer::new", Kind::Method, 1),
            ("new", "Interner::new", Kind::Method, 2),
        ]);
        assert_eq!(call(&s, "Vec::new", 1), Match::None);
        assert_eq!(call(&s, "String::new", 1), Match::None);
    }

    /// 模組與函數同名很常見，但呼叫的一定是函數。
    #[test]
    fn a_call_never_targets_a_module() {
        let s = store_with(&[
            ("render", "render", Kind::Module, 1),
            ("render", "render", Kind::Function, 2),
        ]);
        assert_eq!(call(&s, "render", 1), certain(2));
    }

    #[test]
    fn traits_and_type_aliases_are_not_call_targets_either() {
        let s = store_with(&[
            ("Runner", "Runner", Kind::Trait, 1),
            ("Alias", "Alias", Kind::TypeAlias, 1),
        ]);
        assert_eq!(call(&s, "Runner", 1), Match::None);
        assert_eq!(call(&s, "Alias", 1), Match::None);
    }

    /// `warnings.push(..)` 呼叫的是標準函式庫的方法，不是專案裡剛好
    /// 同名的自由函數。寫法本身就排除了這個候選。
    #[test]
    fn a_receiver_call_never_lands_on_a_free_function() {
        let s = store_with(&[("push", "push", Kind::Function, 1)]);
        assert_eq!(call(&s, "warnings.push", 1), Match::None);
    }

    /// 反過來也一樣：直接呼叫不會落在某個型別的方法上。
    #[test]
    fn a_direct_call_never_lands_on_a_method() {
        let s = store_with(&[("stats", "Store::stats", Kind::Method, 1)]);
        assert_eq!(call(&s, "stats", 1), Match::None);
    }

    /// 接收者的型別查不到時，不拿方法名去比對整個專案。
    ///
    /// 「全專案剛好只有一個同名方法」不構成證據：`r.get(0)` 的 `r` 是
    /// 外部函式庫的型別，接到專案裡碰巧同名的 `get` 上就是一條錯的邊。
    #[test]
    fn an_unknown_receiver_does_not_reach_across_files() {
        let s = store_with(&[("stats", "Store::stats", Kind::Method, 1)]);
        assert_eq!(call(&s, "store.stats", 2), Match::None);
    }

    /// 同一個檔案裡的方法有上下文支撐，不算推測。
    #[test]
    fn a_method_found_in_the_same_file_is_not_a_guess() {
        let s = store_with(&[("stats", "Store::stats", Kind::Method, 1)]);
        assert_eq!(call(&s, "store.stats", 1), certain(1));
    }

    /// 寫出限定名的呼叫沒有不確定性。
    #[test]
    fn a_qualified_method_call_is_certain() {
        let s = store_with(&[("stats", "Store::stats", Kind::Method, 2)]);
        assert_eq!(call(&s, "Store::stats", 1), certain(1));
    }

    #[test]
    fn the_filter_is_derived_from_the_exclude_lists() {
        assert_eq!(
            kind_filter(Rel::Calls, Field::Name, Style::Direct),
            exclude_clause(&DIRECT_EXCLUDES)
        );
        assert_eq!(
            kind_filter(Rel::Calls, Field::Name, Style::Receiver),
            exclude_clause(&RECEIVER_EXCLUDES)
        );
        assert!(!DIRECT_EXCLUDES.contains(&Kind::Function));
        assert!(!RECEIVER_EXCLUDES.contains(&Kind::Method));
    }

    /// 限定名是呼叫方法的正常寫法，比對限定名時不能排除方法。
    #[test]
    fn matching_a_qualified_name_does_not_exclude_methods() {
        assert_eq!(
            kind_filter(Rel::Calls, Field::Qualified, Style::Direct),
            exclude_clause(&BASE_EXCLUDES)
        );
        assert!(!BASE_EXCLUDES.contains(&Kind::Method));
    }

    /// 只有呼叫需要依寫法排除種類，其他關係沒有這個限制。
    #[test]
    fn other_relations_do_not_filter_by_kind() {
        assert_eq!(kind_filter(Rel::UsesType, Field::Name, Style::Direct), "");
        assert_eq!(
            kind_filter(Rel::Implements, Field::Qualified, Style::Receiver),
            ""
        );
    }

    #[test]
    fn the_call_style_comes_from_the_written_form() {
        assert_eq!(Style::of("helper"), Style::Direct);
        assert_eq!(Style::of("a::b::helper"), Style::Direct);
        assert_eq!(Style::of("obj.helper"), Style::Receiver);
        assert_eq!(Style::of("self.inner.helper"), Style::Receiver);
    }
}
