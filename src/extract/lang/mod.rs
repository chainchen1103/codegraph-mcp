//! 語言抽取器的註冊表。
//!
//! 新增一個語言只需新增一個模組並在 [`all`] 加入一項。

pub mod bindings;
pub mod common;
pub mod javascript;
pub mod python;
pub mod rust;
pub mod typescript;

/// 所有已註冊的抽取器。
fn all() -> &'static [&'static dyn super::Extractor] {
    &[
        &rust::RustExtractor,
        &typescript::TypeScriptExtractor,
        &python::PythonExtractor,
        &javascript::JavaScriptExtractor,
    ]
}

/// 依副檔名取得抽取器，副檔名不含點。
pub fn by_extension(ext: &str) -> Option<&'static dyn super::Extractor> {
    all()
        .iter()
        .copied()
        .find(|e| e.extensions().contains(&ext))
}

/// 已支援的語言名稱。
pub fn languages() -> Vec<&'static str> {
    all().iter().map(|e| e.language()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_map_to_exactly_one_extractor() {
        // 副檔名重複時，查詢結果會取決於註冊順序。
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
        assert!(by_extension("go").is_none(), "還沒註冊的語言不該有抽取器");
    }

    /// 每個語言各認自己的副檔名，查得到對的那一個。
    #[test]
    fn each_language_owns_its_extensions() {
        for (ext, language) in [
            ("rs", "rust"),
            ("ts", "typescript"),
            ("tsx", "typescript"),
            ("py", "python"),
            ("pyi", "python"),
            ("js", "javascript"),
            ("jsx", "javascript"),
            ("mjs", "javascript"),
        ] {
            let found = by_extension(ext).unwrap_or_else(|| panic!("{ext} 沒有抽取器"));
            assert_eq!(found.language(), language, "{ext}");
        }
    }

    #[test]
    fn language_names_are_listed() {
        assert_eq!(
            languages(),
            vec!["rust", "typescript", "python", "javascript"]
        );
    }
}
