//! 函數本體內的變數型別追蹤。
//!
//! 方法呼叫只寫得出方法名，接收者的型別要另外判斷。宣告在原始碼裡是
//! 明擺著的：參數有型別標註、`let` 綁定有標註或以建構式初始化、`self`
//! 就是所屬的型別。抽取階段沿著語法樹走一遍就能建出對照表，比在解析
//! 階段用文字回頭找精確，也不必為每個引用重掃一次檔案。
//!
//! 追蹤的是**這個檔案裡看得出來的**型別。看不出來的接收者不記，由解析
//! 階段決定要怎麼處理。

use std::collections::HashMap;

/// 由內而外的區塊堆疊，每層記錄該層宣告的變數與其型別。
///
/// 內層的同名綁定遮蔽外層，查詢時由內往外找。
#[derive(Debug, Default)]
pub struct Bindings {
    frames: Vec<HashMap<String, String>>,
}

impl Bindings {
    /// 建立一個只有最外層的堆疊。
    pub fn new() -> Self {
        Self {
            frames: vec![HashMap::new()],
        }
    }

    /// 進入一個新的區塊。
    pub fn enter(&mut self) {
        self.frames.push(HashMap::new());
    }

    /// 離開目前的區塊。最外層永遠保留。
    pub fn leave(&mut self) {
        if self.frames.len() > 1 {
            self.frames.pop();
        }
    }

    /// 在目前的區塊記下一個綁定。
    ///
    /// 同一層重複宣告同名變數時後者勝出，這正是 Rust 的遮蔽語意。
    pub fn insert(&mut self, name: &str, type_name: &str) {
        if name.is_empty() || type_name.is_empty() {
            return;
        }
        if let Some(frame) = self.frames.last_mut() {
            frame.insert(name.to_string(), type_name.to_string());
        }
    }

    /// 查詢變數的型別，由內層往外層找。
    pub fn get(&self, name: &str) -> Option<&str> {
        self.frames
            .iter()
            .rev()
            .find_map(|frame| frame.get(name))
            .map(String::as_str)
    }

    /// 目前的巢狀深度，測試用。
    #[cfg(test)]
    fn depth(&self) -> usize {
        self.frames.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binding_is_visible_after_it_is_declared() {
        let mut b = Bindings::new();
        assert_eq!(b.get("store"), None);

        b.insert("store", "Store");
        assert_eq!(b.get("store"), Some("Store"));
    }

    #[test]
    fn an_inner_block_sees_the_outer_bindings() {
        let mut b = Bindings::new();
        b.insert("store", "Store");

        b.enter();
        assert_eq!(b.get("store"), Some("Store"));
        b.leave();
    }

    /// 內層的同名綁定遮蔽外層，離開區塊之後外層的綁定回來。
    #[test]
    fn an_inner_binding_shadows_the_outer_one() {
        let mut b = Bindings::new();
        b.insert("value", "Store");

        b.enter();
        b.insert("value", "Writer");
        assert_eq!(b.get("value"), Some("Writer"));
        b.leave();

        assert_eq!(b.get("value"), Some("Store"));
    }

    /// 區塊裡宣告的變數不會洩漏到外面。
    #[test]
    fn a_binding_does_not_outlive_its_block() {
        let mut b = Bindings::new();

        b.enter();
        b.insert("temp", "Writer");
        b.leave();

        assert_eq!(b.get("temp"), None);
    }

    /// 同一層重複宣告是 Rust 的遮蔽寫法，後者勝出。
    #[test]
    fn redeclaring_in_the_same_block_replaces_the_binding() {
        let mut b = Bindings::new();
        b.insert("value", "Store");
        b.insert("value", "Writer");
        assert_eq!(b.get("value"), Some("Writer"));
    }

    #[test]
    fn the_outermost_frame_is_never_popped() {
        let mut b = Bindings::new();
        b.insert("store", "Store");

        for _ in 0..5 {
            b.leave();
        }

        assert_eq!(b.depth(), 1);
        assert_eq!(b.get("store"), Some("Store"));
    }

    #[test]
    fn empty_names_and_types_are_ignored() {
        let mut b = Bindings::new();
        b.insert("", "Store");
        b.insert("store", "");
        assert_eq!(b.get(""), None);
        assert_eq!(b.get("store"), None);
    }
}
