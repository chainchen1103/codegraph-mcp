//! 語言註冊表。
//!
//! 加一個語言 = **新增一個檔案 + 在這裡加一行**。若某次新增語言需要
//! 改動 `extract/` 以外的地方，代表抽象漏了，該回頭修 `Extractor` trait
//! 而不是把特例塞進呼叫端（IMPLEMENTATION.md Stage 10）。

pub mod rust;

/// 所有已註冊的抽取器。
fn all() -> &'static [&'static dyn super::Extractor] {
    &[&rust::RustExtractor]
}

/// 依副檔名（不含點）找抽取器。
pub fn by_extension(ext: &str) -> Option<&'static dyn super::Extractor> {
    all()
        .iter()
        .copied()
        .find(|e| e.extensions().contains(&ext))
}

/// 已支援的語言名稱，`status` 與文件用。
pub fn languages() -> Vec<&'static str> {
    all().iter().map(|e| e.language()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_map_to_exactly_one_extractor() {
        // 兩個抽取器搶同一個副檔名的話，`by_extension` 的結果會取決於
        // 註冊順序——這種 bug 只會在加第三、第四個語言時冒出來。
        let mut seen: Vec<&str> = Vec::new();
        for e in all() {
            for ext in e.extensions() {
                assert!(!seen.contains(ext), "副檔名 {ext} 被兩個抽取器認領");
                seen.push(ext);
            }
        }
        assert!(!seen.is_empty());
    }

    #[test]
    fn lookup_is_by_bare_extension() {
        assert!(by_extension("rs").is_some());
        assert!(by_extension(".rs").is_none(), "副檔名不應該帶點");
        assert!(by_extension("py").is_none(), "Python 還沒實作");
    }

    #[test]
    fn language_names_are_listed() {
        assert_eq!(languages(), vec!["rust"]);
    }
}
