//! 符號識別碼。
//!
//! **穩定性要求（DESIGN.md §8.3）：改 function body 而不改簽名與起始行時，
//! moniker 必須不變。** 滿足這條，單檔重解析後該符號的所有既有邊自動存活，
//! 不需要任何 remap 機制。
//!
//! 組成：`{相對路徑}:{kind}:{name}:{起始行}`
//!
//! 為什麼含起始行：同一個檔案裡可以有兩個同名的東西（不同 impl 區塊的
//! 同名方法、`#[cfg]` 分支的兩份實作）。行號是最便宜的消歧義鍵。
//! 代價是「在檔案上方插入一行」會讓下方所有符號換 moniker——這是刻意的
//! 取捨：那種編輯本來就會讓所有行號失效，重建比誤指更安全。

use crate::model::Kind;

/// 組出一個符號的 moniker。
///
/// `rel_path` 必須是相對專案根目錄、且用 `/` 當分隔符的路徑——
/// Windows 的 `\` 會讓同一份 artifact 在不同平台產出不同的識別碼。
pub fn build(rel_path: &str, kind: Kind, name: &str, start_line: u32) -> String {
    format!(
        "{}:{}:{}:{}",
        normalize_path(rel_path),
        kind.as_str(),
        name,
        start_line
    )
}

/// 路徑正規化：反斜線換成斜線，去掉開頭的 `./`。
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

    /// 這是整個增量索引策略的地基：body 改了、簽名與行號沒改，
    /// 識別碼就不變，該符號的所有 caller 邊自動存活。
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

    /// 同一份 artifact 要能在 Windows 與 Linux 上讀出同樣的圖。
    /// 路徑分隔符沒有正規化的話，兩邊的識別碼完全對不上。
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
