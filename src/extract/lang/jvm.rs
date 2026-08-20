//! JVM 家族共用的零件。
//!
//! Java、Kotlin、Scala 的原始碼佈局遵循同一套慣例：套件名對應目錄，而
//! 目錄前面墊著建置工具規定的來源根（`src/main/java` 之類）。三個語言
//! 各寫一份剝除邏輯沒有意義。
//!
//! 套件名其實寫在檔案裡（`package com.example.app`），比路徑可靠。但
//! [`Extractor::module_path`] 只拿得到路徑——那是刻意的，它要在還沒解析
//! 的情況下就能回答。慣例佈局涵蓋絕大多數專案，不合慣例的專案退化成
//! 「模組路徑對不上」，而不是接出錯的邊。
//!
//! [`Extractor::module_path`]: crate::extract::Extractor::module_path

/// 建置工具規定的來源根，由長到短比對。
///
/// 順序要緊：`src/main/java` 必須排在 `src` 前面，否則只會被剝掉一層。
const SOURCE_ROOTS: &[&str] = &[
    "src/main/java",
    "src/test/java",
    "src/main/kotlin",
    "src/test/kotlin",
    "src/main/scala",
    "src/test/scala",
    "src/main/resources",
    "app/src/main/java",
    "app/src/main/kotlin",
    "src",
];

/// 從檔案路徑推出套件路徑，以 `::` 連接。
///
/// `suffix` 是該語言的副檔名（含點）。不是那個副檔名就回空字串。
pub fn package_path(rel_path: &str, suffixes: &[&str]) -> String {
    let normalized = rel_path.replace('\\', "/");
    if !suffixes.iter().any(|s| normalized.ends_with(s)) {
        return String::new();
    }

    let mut without_file = match normalized.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        // 根目錄底下的檔案沒有套件。
        None => return String::new(),
    };

    for root in SOURCE_ROOTS {
        if without_file == *root {
            return String::new();
        }
        if let Some(rest) = without_file.strip_prefix(&format!("{root}/")) {
            without_file = rest.to_string();
            break;
        }
    }

    without_file.replace('/', "::")
}

/// 這個名字看起來是型別而不是變數。
///
/// JVM 三個語言的命名慣例一致到可以當判斷依據：型別大寫開頭，變數小寫。
/// `Helper.compute()` 是對型別的靜態呼叫，`box.area()` 是對值的方法呼叫，
/// 兩者在語法樹上長得一模一樣，只有這個線索分得開。
pub fn looks_like_type(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JAVA: &[&str] = &[".java"];

    #[test]
    fn a_maven_layout_strips_its_source_root() {
        assert_eq!(
            package_path("src/main/java/com/example/app/Box.java", JAVA),
            "com::example::app"
        );
        assert_eq!(
            package_path("src/test/java/com/example/BoxTest.java", JAVA),
            "com::example"
        );
    }

    /// 長的來源根要先比對，否則 `src/main/java` 只會被剝掉 `src`。
    #[test]
    fn the_longest_source_root_wins() {
        assert_eq!(package_path("src/main/java/App.java", JAVA), "");
        assert_ne!(
            package_path("src/main/java/a/App.java", JAVA),
            "main::java::a"
        );
    }

    #[test]
    fn a_plain_src_layout_also_works() {
        assert_eq!(
            package_path("src/com/example/Box.java", JAVA),
            "com::example"
        );
    }

    #[test]
    fn a_file_at_the_root_has_no_package() {
        assert_eq!(package_path("Box.java", JAVA), "");
    }

    #[test]
    fn another_extension_has_no_package() {
        assert_eq!(package_path("src/main/java/com/App.kt", JAVA), "");
    }

    #[test]
    fn package_paths_do_not_depend_on_the_path_separator() {
        assert_eq!(
            package_path(r"src\main\java\com\example\Box.java", JAVA),
            package_path("src/main/java/com/example/Box.java", JAVA)
        );
    }

    #[test]
    fn the_naming_convention_separates_types_from_values() {
        assert!(looks_like_type("Helper"));
        assert!(!looks_like_type("box"));
        assert!(!looks_like_type(""));
    }
}
