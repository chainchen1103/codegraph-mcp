//! 字串池：把重複出現的字串換成整數識別碼。
//!
//! 索引中大量重複的字串只有兩種：符號的 moniker 與檔案路徑。兩者都
//! 以整數存放，邊表與索引才不會被字串撐大。

use std::collections::HashMap;

/// 識別碼的配發器。
///
/// 同一個字串永遠得到同一個識別碼；未見過的字串會取得新的識別碼，
/// 並記入待寫入清單。
#[derive(Debug, Default)]
pub struct Interner {
    map: HashMap<Box<str>, u32>,
    pending: Vec<(u32, String)>,
    next: u32,
}

impl Interner {
    /// 建立一個從 1 開始配發識別碼的字串池。
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            pending: Vec::new(),
            next: 1,
        }
    }

    /// 取得字串的識別碼，必要時配發新的。
    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(id) = self.map.get(s) {
            return *id;
        }
        let id = self.next;
        self.next += 1;
        self.map.insert(s.into(), id);
        self.pending.push((id, s.to_string()));
        id
    }

    /// 查詢既有的識別碼，不配發新的。
    pub fn get(&self, s: &str) -> Option<u32> {
        self.map.get(s).copied()
    }

    /// 取走尚未寫入資料庫的項目。
    pub fn take_pending(&mut self) -> Vec<(u32, String)> {
        std::mem::take(&mut self.pending)
    }

    /// 已配發的識別碼數量。
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_string_always_gets_the_same_id() {
        let mut i = Interner::new();
        let a = i.intern("src/a.rs");
        let b = i.intern("src/b.rs");
        assert_ne!(a, b);
        assert_eq!(i.intern("src/a.rs"), a);
        assert_eq!(i.len(), 2);
    }

    #[test]
    fn ids_start_at_one_so_zero_stays_available_as_a_sentinel() {
        let mut i = Interner::new();
        assert_eq!(i.intern("first"), 1);
        assert_eq!(i.intern("second"), 2);
    }

    #[test]
    fn only_new_strings_are_queued_for_writing() {
        let mut i = Interner::new();
        i.intern("a");
        i.intern("b");
        i.intern("a");

        let pending = i.take_pending();
        assert_eq!(
            pending,
            vec![(1, "a".to_string()), (2, "b".to_string())],
            "重複的字串不該被排入寫入佇列"
        );

        // 取走之後不會重複交出同一批。
        assert!(i.take_pending().is_empty());

        // 已配發的識別碼仍然查得到。
        assert_eq!(i.get("a"), Some(1));
    }

    #[test]
    fn lookup_does_not_allocate_new_ids() {
        let mut i = Interner::new();
        assert_eq!(i.get("missing"), None);
        assert!(i.is_empty());

        i.intern("present");
        assert_eq!(i.get("present"), Some(1));
        assert_eq!(i.len(), 1);
    }
}
