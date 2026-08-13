//! 同一個 session 內的去重（DESIGN §5.6）。
//!
//! 同一段原始碼在一次對話裡重複送出是純浪費。**已經送過的那個符號**、
//! 而且它所在的檔案沒有變，這一次改成一行指標，省下的額度留給還沒看
//! 過的原始碼。
//!
//! 粒度是符號，不是檔案。一個檔案裡有十個符號，上一次送了三個，這一次
//! 問另外七個——若以檔案為單位，那七個從沒送出過的符號會被整批扣掉，
//! 指標還宣稱「你已經有了」。哪一邊錯得起是清楚的：重送一次已經有的東
//! 西是浪費幾百個字元，扣掉沒送過的東西是一次 Read，而一兩次 Read 就
//! 足以讓 agent 整段放棄這個工具。
//!
//! 記錄發生在排版**之後**，記的是實際完整送出去的符號。選中不等於送
//! 出：讀不到原始碼、或因額度被裁掉一半的，都還沒真的到對方手上。

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use crate::explore::render::Emitted;
use crate::explore::select::Selection;
use crate::model::SymbolId;
use crate::store::content_hash;

/// 一個檔案送出過的內容。
#[derive(Debug)]
struct Sent {
    /// 送出當時的檔案內容雜湊。檔案一改，這裡記的全部作廢。
    hash: String,
    symbols: HashSet<SymbolId>,
}

/// 一個檔案的指標，取代這一次不必重送的原始碼。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pointer {
    pub file: String,
    /// 這個檔案裡被省略的符號，依限定名。
    pub symbols: Vec<String>,
}

/// 一個對話的送出記錄。
#[derive(Debug, Default)]
pub struct Session {
    sent: HashMap<String, Sent>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    /// 把已經送過、而且檔案未變的符號從 `selection` 移除。
    ///
    /// 回傳被移除的符號，依檔案分組。讀不到內容的檔案一律當成沒送過：
    /// 無法證明內容相同時，重送比讓呼叫端拿著指標卻找不到內容好。
    pub fn dedup(&self, root: &Path, selection: &mut Selection) -> Vec<Pointer> {
        // 同一個檔案在一次查詢裡通常有好幾個符號，雜湊只算一次。
        let mut hashes: HashMap<String, Option<String>> = HashMap::new();
        let mut dropped: Vec<(String, String)> = Vec::new();

        selection.hits.retain(|hit| {
            let hash = hashes
                .entry(hit.file.clone())
                .or_insert_with(|| hash_of(root, &hit.file));

            let held = match (hash.as_deref(), self.sent.get(&hit.file)) {
                (Some(hash), Some(sent)) => sent.hash == hash && sent.symbols.contains(&hit.id),
                _ => false,
            };
            if held {
                dropped.push((hit.file.clone(), hit.qualified.clone()));
            }
            !held
        });

        group(dropped)
    }

    /// 記下這一次確實送出去的符號。
    ///
    /// 檔案內容變過時舊記錄整份作廢——同一個符號的行號與內容都可能已經
    /// 不是當初那一份了。
    pub fn record(&mut self, root: &Path, hits: &Selection, emitted: &[Emitted]) {
        let sent: HashSet<SymbolId> = emitted.iter().map(|e| e.id).collect();

        for hit in &hits.hits {
            if !sent.contains(&hit.id) {
                continue;
            }
            let Some(hash) = hash_of(root, &hit.file) else {
                continue;
            };

            let entry = self.sent.entry(hit.file.clone()).or_insert_with(|| Sent {
                hash: hash.clone(),
                symbols: HashSet::new(),
            });
            if entry.hash != hash {
                entry.hash = hash;
                entry.symbols.clear();
            }
            entry.symbols.insert(hit.id);
        }
    }
}

/// 依檔案彙整被省略的符號，順序固定。
fn group(dropped: Vec<(String, String)>) -> Vec<Pointer> {
    let mut by_file: HashMap<String, Vec<String>> = HashMap::new();
    for (file, symbol) in dropped {
        by_file.entry(file).or_default().push(symbol);
    }

    let mut out: Vec<Pointer> = by_file
        .into_iter()
        .map(|(file, mut symbols)| {
            symbols.sort();
            symbols.dedup();
            Pointer { file, symbols }
        })
        .collect();
    out.sort_by(|a, b| a.file.cmp(&b.file));
    out
}

fn hash_of(root: &Path, file: &str) -> Option<String> {
    std::fs::read(root.join(file))
        .ok()
        .map(|b| content_hash(&b))
}

/// 指標區塊。
///
/// 必須說清楚這是指標而不是缺漏，並且指名到符號——只說檔案的話，呼叫端
/// 沒辦法判斷自己手上是不是正好就是這幾個符號。
pub fn render_pointers(out: &mut String, pointers: &[Pointer]) {
    if pointers.is_empty() {
        return;
    }

    writeln!(out).ok();
    writeln!(
        out,
        "## 稍早已送出（內容未變，這是指標不是缺漏，不需要再讀一次）"
    )
    .ok();
    for pointer in pointers {
        writeln!(out, "  {}  {}", pointer.file, pointer.symbols.join("、")).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explore::{budget, query, render, select};
    use crate::store::Store;
    use crate::testing::{cleanup, indexed_project};

    /// 一次完整的查詢：挑選、去重、排版、記錄。
    fn ask(
        session: &mut Session,
        root: &Path,
        store: &Store,
        input: &str,
    ) -> (Vec<Pointer>, String) {
        let mut selection = select::select(store.conn(), &query::parse(input)).unwrap();
        let pointers = session.dedup(root, &mut selection);
        let (text, emitted) = render::reporting(root, &selection, budget::for_file_count(1));
        session.record(root, &selection, &emitted);
        (pointers, text)
    }

    #[test]
    fn asking_for_the_same_symbol_twice_collapses_to_a_pointer() {
        let p = indexed_project("session-repeat", &[("src/a.rs", "pub fn one() {}\n")]);
        let store = Store::open(&p.db_path()).unwrap();
        let mut session = Session::new();

        let (first, text) = ask(&mut session, p.root(), &store, "one");
        assert!(first.is_empty(), "第一次不該去重");
        assert!(text.contains("pub fn one()"), "{text}");

        let (second, text) = ask(&mut session, p.root(), &store, "one");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].file, "src/a.rs");
        assert_eq!(second[0].symbols, vec!["one".to_string()]);
        assert!(!text.contains("pub fn one()"), "第二次還在送原始碼：{text}");

        drop(store);
        cleanup(&p);
    }

    /// 這一條是檔案粒度做不到的：同一個檔案裡沒送過的符號必須照常送。
    #[test]
    fn another_symbol_in_a_seen_file_is_still_sent() {
        let p = indexed_project(
            "session-sibling",
            &[("src/a.rs", "pub fn one() {}\npub fn two() {}\n")],
        );
        let store = Store::open(&p.db_path()).unwrap();
        let mut session = Session::new();

        ask(&mut session, p.root(), &store, "one");
        let (pointers, text) = ask(&mut session, p.root(), &store, "two");

        assert!(pointers.is_empty(), "two 從沒送過，不該被扣掉");
        assert!(text.contains("pub fn two()"), "{text}");

        drop(store);
        cleanup(&p);
    }

    /// 一半送過一半沒有：送過的變指標，沒送過的照常出。
    #[test]
    fn a_partly_seen_file_sends_only_the_unseen_half() {
        let p = indexed_project(
            "session-partial",
            &[("src/a.rs", "pub fn one() {}\npub fn two() {}\n")],
        );
        let store = Store::open(&p.db_path()).unwrap();
        let mut session = Session::new();

        ask(&mut session, p.root(), &store, "one");
        let (pointers, text) = ask(&mut session, p.root(), &store, "one two");

        assert_eq!(pointers.len(), 1);
        assert_eq!(pointers[0].symbols, vec!["one".to_string()]);
        assert!(text.contains("pub fn two()"), "{text}");
        assert!(!text.contains("pub fn one()"), "{text}");

        drop(store);
        cleanup(&p);
    }

    /// 檔案改過，整份記錄作廢，連沒動到的符號也重送——行號可能已經位移。
    #[test]
    fn editing_the_file_invalidates_everything_it_had_sent() {
        let p = indexed_project(
            "session-changed",
            &[("src/a.rs", "pub fn one() {}\npub fn two() {}\n")],
        );
        let store = Store::open(&p.db_path()).unwrap();
        let mut session = Session::new();

        ask(&mut session, p.root(), &store, "one two");
        std::fs::write(
            p.root().join("src/a.rs"),
            "// 新的一行\npub fn one() {}\npub fn two() {}\n",
        )
        .unwrap();

        let (pointers, _) = ask(&mut session, p.root(), &store, "one");
        assert!(pointers.is_empty(), "檔案變了還在省略");

        drop(store);
        cleanup(&p);
    }

    /// 讀不到內容時無法證明相同，寧可重送。
    #[test]
    fn an_unreadable_file_is_never_deduped() {
        let p = indexed_project("session-gone", &[("src/a.rs", "pub fn one() {}\n")]);
        let store = Store::open(&p.db_path()).unwrap();
        let mut session = Session::new();

        ask(&mut session, p.root(), &store, "one");
        std::fs::remove_file(p.root().join("src/a.rs")).unwrap();

        let mut selection = select::select(store.conn(), &query::parse("one")).unwrap();
        assert!(session.dedup(p.root(), &mut selection).is_empty());
        assert_eq!(selection.hits.len(), 1);

        drop(store);
        cleanup(&p);
    }

    /// 選中不等於送出：沒真的排版出來的符號不能記成送過。
    #[test]
    fn a_symbol_that_was_never_rendered_is_not_recorded() {
        let p = indexed_project("session-unrendered", &[("src/a.rs", "pub fn one() {}\n")]);
        let store = Store::open(&p.db_path()).unwrap();
        let mut session = Session::new();

        let selection = select::select(store.conn(), &query::parse("one")).unwrap();
        session.record(p.root(), &selection, &[]);

        let mut again = select::select(store.conn(), &query::parse("one")).unwrap();
        assert!(session.dedup(p.root(), &mut again).is_empty());

        drop(store);
        cleanup(&p);
    }

    /// 每個 session 各自記錄，互不影響。
    #[test]
    fn sessions_do_not_share_what_they_have_sent() {
        let p = indexed_project("session-isolated", &[("src/a.rs", "pub fn one() {}\n")]);
        let store = Store::open(&p.db_path()).unwrap();

        let mut a = Session::new();
        ask(&mut a, p.root(), &store, "one");

        let mut b = Session::new();
        let (pointers, _) = ask(&mut b, p.root(), &store, "one");
        assert!(pointers.is_empty());

        drop(store);
        cleanup(&p);
    }

    #[test]
    fn the_pointer_block_names_the_symbols_and_says_it_is_not_a_gap() {
        let mut out = String::new();
        render_pointers(
            &mut out,
            &[Pointer {
                file: "src/a.rs".into(),
                symbols: vec!["one".into(), "two".into()],
            }],
        );

        assert!(out.contains("src/a.rs"), "{out}");
        assert!(out.contains("one、two"), "{out}");
        assert!(out.contains("不是缺漏"), "{out}");

        let mut empty = String::new();
        render_pointers(&mut empty, &[]);
        assert!(empty.is_empty(), "沒有指標時不該印區塊");
    }

    /// 指標的順序固定，同樣的查詢輸出要每次相同。
    #[test]
    fn pointers_are_grouped_by_file_in_a_fixed_order() {
        let grouped = group(vec![
            ("src/b.rs".into(), "two".into()),
            ("src/a.rs".into(), "zed".into()),
            ("src/a.rs".into(), "one".into()),
            ("src/a.rs".into(), "one".into()),
        ]);

        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].file, "src/a.rs");
        assert_eq!(
            grouped[0].symbols,
            vec!["one".to_string(), "zed".to_string()]
        );
        assert_eq!(grouped[1].file, "src/b.rs");
    }
}
