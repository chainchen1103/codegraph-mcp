//! 編譯單元的偵測。
//!
//! 一個 repo 常常不只一個編譯單元：monorepo 裡 TS 前端與 Python 後端
//! 並存，Rust workspace 底下每個 crate 各自獨立。單元邊界決定 Stage 16
//! 的級聯範圍——改了 A 單元的對外簽名，只有依賴它的單元要重建。
//!
//! 判斷方式是找**離檔案最近的上層 manifest**。這個規則對每個語言都一
//! 樣，新增語言只需在 [`MANIFESTS`] 加一列，不必動任何邏輯。
//!
//! 找不到任何 manifest 的檔案歸到 [`ROOT`]。這不是錯誤：不是每個 repo
//! 都有 manifest，而沒有單元邊界只表示級聯範圍是整個 repo。

use std::path::Path;

/// 沒有任何 manifest 認領時所屬的單元。
pub const ROOT: &str = "root";

/// manifest 檔名，與它代表的生態系。
///
/// 順序決定同一層有多份 manifest 時誰勝出。實務上這很常見——TS 專案裡
/// 常有 `package.json` 與 `tsconfig.json` 並存——取哪一個不影響正確性，
/// 只要每次取同一個，單元邊界就是穩定的。
const MANIFESTS: &[(&str, &str)] = &[
    ("Cargo.toml", "cargo"),
    ("go.mod", "go"),
    ("tsconfig.json", "ts"),
    ("package.json", "npm"),
    ("pyproject.toml", "python"),
    ("setup.py", "python"),
];

/// `rel_path` 所屬的編譯單元名稱。
///
/// `root` 是專案根目錄的絕對路徑，`rel_path` 相對於它。名稱的形狀是
/// `{生態系}:{相對於專案根的目錄}`，根目錄本身省略後半段。
pub fn of(root: &Path, rel_path: &str) -> String {
    let normalized = rel_path.replace('\\', "/");
    let mut segments: Vec<&str> = normalized.split('/').collect();
    // 最後一段是檔名，從它所在的目錄開始往上找。
    segments.pop();

    loop {
        let dir = segments.join("/");
        if let Some(ecosystem) = manifest_in(&root.join(&dir)) {
            return if dir.is_empty() {
                ecosystem.to_string()
            } else {
                format!("{ecosystem}:{dir}")
            };
        }
        if segments.pop().is_none() {
            return ROOT.to_string();
        }
    }
}

/// 這個目錄裡有沒有 manifest。
fn manifest_in(dir: &Path) -> Option<&'static str> {
    MANIFESTS
        .iter()
        .find(|(file, _)| dir.join(file).is_file())
        .map(|(_, ecosystem)| *ecosystem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tmpdir;

    /// 在暫存目錄裡擺出給定的檔案。
    fn layout(tag: &str, files: &[&str]) -> std::path::PathBuf {
        let root = tmpdir(&format!("unit-{tag}"));
        for file in files {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        root
    }

    #[test]
    fn a_file_belongs_to_the_nearest_manifest_above_it() {
        let root = layout("nearest", &["Cargo.toml"]);

        assert_eq!(of(&root, "src/lib.rs"), "cargo");
        assert_eq!(of(&root, "src/deep/nested/thing.rs"), "cargo");

        std::fs::remove_dir_all(&root).ok();
    }

    /// monorepo：前端與後端各自成一個單元，互不相干。
    #[test]
    fn a_monorepo_splits_into_one_unit_per_manifest() {
        let root = layout(
            "monorepo",
            &["web/tsconfig.json", "api/pyproject.toml", "Cargo.toml"],
        );

        assert_eq!(of(&root, "web/src/app.ts"), "ts:web");
        assert_eq!(of(&root, "api/views.py"), "python:api");
        assert_eq!(of(&root, "src/main.rs"), "cargo");

        std::fs::remove_dir_all(&root).ok();
    }

    /// 巢狀 manifest：取最近的那一個，不是最外層。
    #[test]
    fn a_nested_manifest_wins_over_an_outer_one() {
        let root = layout("nested", &["Cargo.toml", "crates/inner/Cargo.toml"]);

        assert_eq!(of(&root, "crates/inner/src/lib.rs"), "cargo:crates/inner");
        assert_eq!(of(&root, "src/lib.rs"), "cargo");

        std::fs::remove_dir_all(&root).ok();
    }

    /// 沒有 manifest 不是錯誤，整個 repo 就是一個單元。
    #[test]
    fn a_repo_without_manifests_is_a_single_unit() {
        let root = layout("bare", &["notes.txt"]);

        assert_eq!(of(&root, "src/a.rs"), ROOT);

        std::fs::remove_dir_all(&root).ok();
    }

    /// 同一層有多份 manifest 時，取哪一個不重要，但每次要取同一個。
    #[test]
    fn the_choice_among_sibling_manifests_is_stable() {
        let root = layout("siblings", &["web/package.json", "web/tsconfig.json"]);

        let first = of(&root, "web/a.ts");
        assert_eq!(first, of(&root, "web/b.ts"));
        assert_eq!(first, "ts:web");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn units_do_not_depend_on_the_path_separator() {
        let root = layout("separator", &["web/tsconfig.json"]);

        assert_eq!(of(&root, r"web\src\app.ts"), of(&root, "web/src/app.ts"));

        std::fs::remove_dir_all(&root).ok();
    }

    /// 目錄底下有同名的目錄而不是檔案時不算數。
    #[test]
    fn a_directory_named_like_a_manifest_is_not_one() {
        let root = tmpdir("unit-dirname");
        std::fs::create_dir_all(root.join("web/package.json")).unwrap();

        assert_eq!(of(&root, "web/a.ts"), ROOT);

        std::fs::remove_dir_all(&root).ok();
    }
}
