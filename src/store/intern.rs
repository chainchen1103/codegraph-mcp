//! 字串池：把重複出現的字串換成整數識別碼。
//!
//! 索引中大量重複的字串只有兩種：符號的 moniker 與檔案路徑。兩者都
//! 以整數存放，邊表與索引才不會被字串撐大。

use std::collections::HashMap;

/// 識別碼的配發器。
///
/// 同一個字串永遠得到同一個識別碼。呼叫端要知道自己拿到的是不是新配
/// 發的，才能決定要不要寫一列進資料庫。
#[derive(Debug, Default)]
pub struct Interner {
    map: HashMap<Box<str>, u32>,
    next: u32,
}

/// 一次 intern 的結果。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Interned {
    pub id: u32,
    /// 這次才配發的識別碼，對應的列還沒寫進資料庫。
    pub is_new: bool,
}

impl Interner {
    /// 建立一個從 1 開始配發識別碼的字串池。
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            next: 1,
        }
    }

    /// 取得字串的識別碼，必要時配發新的。
    pub fn intern(&mut self, s: &str) -> Interned {
        if let Some(id) = self.map.get(s) {
            return Interned {
                id: *id,
                is_new: false,
            };
        }
        let id = self.next;
        self.next += 1;
        self.map.insert(s.into(), id);
        Interned { id, is_new: true }
    }

    /// 查詢既有的識別碼，不配發新的。
    pub fn get(&self, s: &str) -> Option<u32> {
        self.map.get(s).copied()
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
        assert_ne!(a.id, b.id);
        assert_eq!(i.intern("src/a.rs").id, a.id);
    }

    #[test]
    fn ids_start_at_one_so_zero_stays_available_as_a_sentinel() {
        let mut i = Interner::new();
        assert_eq!(i.intern("first").id, 1);
        assert_eq!(i.intern("second").id, 2);
    }

    /// 呼叫端靠這個旗標決定要不要寫一列進資料庫。
    #[test]
    fn only_the_first_sighting_is_reported_as_new() {
        let mut i = Interner::new();
        assert!(i.intern("a").is_new);
        assert!(i.intern("b").is_new);
        assert!(!i.intern("a").is_new);
    }

    #[test]
    fn lookup_does_not_allocate_new_ids() {
        let mut i = Interner::new();
        assert_eq!(i.get("missing"), None);

        i.intern("present");
        assert_eq!(i.get("present"), Some(1));
        assert_eq!(i.get("missing"), None);
    }
}
