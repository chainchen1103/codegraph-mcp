//! 同一個 session 內的去重（DESIGN §5.6）。
//!
//! 同一段原始碼在一次對話裡重複送出是純浪費。已經送過而且檔案沒有變
//! 的檔案，這一次改成一行指標，省下的額度留給還沒看過的原始碼。
//!
//! 去重發生在排版**之前**：被去重的符號根本不會進到排版，因此省下的
//! 額度會自動流向其他符號，不需要再分配一次。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::explore::select::Selection;
use crate::store::content_hash;

/// 一個對話的送出記錄。
#[derive(Debug, Default)]
pub struct Session {
    /// 已送出的檔案，對應送出當時的內容雜湊。
    sent: HashMap<String, String>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    /// 把已經送過且未變更的檔案從 `selection` 移除，回傳這些檔案的路徑。
    ///
    /// 讀不到內容的檔案一律當成沒送過：無法證明內容相同時，重送一次比
    /// 讓呼叫端看到指標卻拿不到內容好。
    pub fn dedup(&mut self, root: &Path, selection: &mut Selection) -> Vec<String> {
        let mut pointers = Vec::new();

        let mut files: Vec<String> = selection.hits.iter().map(|h| h.file.clone()).collect();
        files.sort();
        files.dedup();

        for file in files {
            let Some(hash) = hash_of(root, &file) else {
                continue;
            };
            if self.sent.get(&file) == Some(&hash) {
                pointers.push(file);
            } else {
                self.sent.insert(file, hash);
            }
        }

        selection.hits.retain(|h| !pointers.contains(&h.file));
        pointers
    }
}

fn hash_of(root: &Path, file: &str) -> Option<String> {
    std::fs::read(root.join(file))
        .ok()
        .map(|b| content_hash(&b))
}

/// 指標區塊。
///
/// 必須說清楚這是指標而不是缺漏，否則呼叫端會以為漏了而去讀檔案，去重
/// 省下的成本就白費了。
pub fn render_pointers(out: &mut String, pointers: &[String]) {
    if pointers.is_empty() {
        return;
    }

    writeln!(out).ok();
    writeln!(
        out,
        "## 稍早已送出（內容未變，這是指標不是缺漏，不需要再讀一次）"
    )
    .ok();
    for file in pointers {
        writeln!(out, "  {file}").ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explore::{query, select};
    use crate::store::Store;
    use crate::testing::{cleanup, indexed_project};

    fn selection(store: &Store, input: &str) -> Selection {
        select::select(store.conn(), &query::parse(input)).unwrap()
    }

    #[test]
    fn the_second_call_gets_a_pointer_instead_of_the_source() {
        let p = indexed_project("session-repeat", &[("src/a.rs", "pub fn one() {}\n")]);
        let store = Store::open(&p.db_path()).unwrap();
        let mut session = Session::new();

        let mut first = selection(&store, "one");
        assert!(session.dedup(p.root(), &mut first).is_empty());
        assert_eq!(first.hits.len(), 1, "第一次不該去重");

        let mut second = selection(&store, "one");
        let pointers = session.dedup(p.root(), &mut second);
        assert_eq!(pointers, vec!["src/a.rs".to_string()]);
        assert!(second.hits.is_empty(), "第二次還在送原始碼");

        drop(store);
        cleanup(&p);
    }

    /// 檔案改過就要重送，指標指向的是舊內容。
    #[test]
    fn a_changed_file_is_sent_again() {
        let p = indexed_project("session-changed", &[("src/a.rs", "pub fn one() {}\n")]);
        let store = Store::open(&p.db_path()).unwrap();
        let mut session = Session::new();

        let mut first = selection(&store, "one");
        session.dedup(p.root(), &mut first);

        std::fs::write(p.root().join("src/a.rs"), "pub fn one() {\n    1;\n}\n").unwrap();

        let mut second = selection(&store, "one");
        assert!(session.dedup(p.root(), &mut second).is_empty());
        assert_eq!(second.hits.len(), 1);

        drop(store);
        cleanup(&p);
    }

    /// 讀不到內容時無法證明相同，寧可重送。
    #[test]
    fn an_unreadable_file_is_never_deduped() {
        let p = indexed_project("session-gone", &[("src/a.rs", "pub fn one() {}\n")]);
        let store = Store::open(&p.db_path()).unwrap();
        let mut session = Session::new();

        let mut first = selection(&store, "one");
        session.dedup(p.root(), &mut first);

        std::fs::remove_file(p.root().join("src/a.rs")).unwrap();

        let mut second = selection(&store, "one");
        assert!(session.dedup(p.root(), &mut second).is_empty());
        assert_eq!(second.hits.len(), 1);

        drop(store);
        cleanup(&p);
    }

    /// 每個 session 各自記錄，互不影響。
    #[test]
    fn sessions_do_not_share_what_they_have_sent() {
        let p = indexed_project("session-isolated", &[("src/a.rs", "pub fn one() {}\n")]);
        let store = Store::open(&p.db_path()).unwrap();

        let mut a = Session::new();
        let mut first = selection(&store, "one");
        a.dedup(p.root(), &mut first);

        let mut b = Session::new();
        let mut second = selection(&store, "one");
        assert!(b.dedup(p.root(), &mut second).is_empty());

        drop(store);
        cleanup(&p);
    }

    #[test]
    fn the_pointer_block_says_it_is_not_a_gap() {
        let mut out = String::new();
        render_pointers(&mut out, &["src/a.rs".to_string()]);
        assert!(out.contains("src/a.rs"), "{out}");
        assert!(out.contains("不是缺漏"), "{out}");

        let mut empty = String::new();
        render_pointers(&mut empty, &[]);
        assert!(empty.is_empty(), "沒有指標時不該印區塊");
    }
}
