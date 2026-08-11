//! 符號識別碼的組成規則。
//!
//! 格式為 `路徑:kind:name:起始行`。
//!
//! 只修改函數內容而不動簽名與起始行時，識別碼保持不變，該符號既有的
//! 邊在重新解析後仍然有效。行號用於區分同一檔案中的同名符號，代價是
//! 在檔案上方插入內容會使下方符號的識別碼全部改變。

use crate::model::Kind;

/// 組出符號的 moniker。
///
/// `rel_path` 必須相對於專案根目錄。
pub fn build(rel_path: &str, kind: Kind, name: &str, start_line: u32) -> String {
    format!(
        "{}:{}:{}:{}",
        normalize_path(rel_path),
        kind.as_str(),
        name,
        start_line
    )
}

/// 正規化路徑：反斜線轉為斜線，並去除開頭的 `./`。
///
/// 不同平台的分隔符必須收斂成同一種，索引才能跨平台共用。
pub fn normalize_path(path: &str) -> String {
    let p = path.replace('\\', "/");
    p.strip_prefix("./").unwrap_or(&p).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moniker_has_the_documented_shape() {
        assert_eq!(
            build("src/store/mod.rs", Kind::Function, "open", 42),
            "src/store/mod.rs:function:open:42"
        );
    }

    #[test]
    fn moniker_is_stable_across_body_edits() {
        let before = build("src/a.rs", Kind::Function, "process", 10);
        let after = build("src/a.rs", Kind::Function, "process", 10);
        assert_eq!(before, after);
    }

    #[test]
    fn moniker_distinguishes_everything_that_can_collide() {
        let base = build("src/a.rs", Kind::Function, "run", 10);
        assert_ne!(base, build("src/b.rs", Kind::Function, "run", 10));
        assert_ne!(base, build("src/a.rs", Kind::Method, "run", 10));
        assert_ne!(base, build("src/a.rs", Kind::Function, "walk", 10));
        assert_ne!(base, build("src/a.rs", Kind::Function, "run", 11));
    }

    #[test]
    fn path_separators_are_normalised_across_platforms() {
        assert_eq!(
            build(r"src\store\mod.rs", Kind::Function, "open", 1),
            build("src/store/mod.rs", Kind::Function, "open", 1)
        );
        assert_eq!(normalize_path("./src/a.rs"), "src/a.rs");
        assert_eq!(normalize_path(r".\src\a.rs"), "src/a.rs");
    }
}
