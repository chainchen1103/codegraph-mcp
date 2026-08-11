//! 抽取層的整合測試——**拿本專案自己當索引對象**（自舉）。
//!
//! 用真實的、還在長大的原始碼測試，比 fixture 更能抓到「只有在真的
//! 程式碼裡才會出現」的形狀：巢狀模組、trait 實作、屬性、文件註解。

use std::path::{Path, PathBuf};

use code_graph::extract;
use code_graph::model::Kind;

/// 列出 `src/` 底下所有 `.rs` 檔（相對路徑、`/` 分隔）。
fn own_sources() -> Vec<String> {
    let mut out = Vec::new();
    collect(Path::new("src"), &mut out);
    out.sort();
    assert!(out.len() >= 10, "自己的原始碼檔案太少，路徑是不是錯了？");
    out
}

fn collect(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn parse_file(rel: &str) -> extract::FileParse {
    let source = std::fs::read_to_string(rel).unwrap();
    extract::extract(rel, &source).unwrap()
}

/// 我們自己的程式碼必須完全解析得動。這裡出現錯誤，代表 tree-sitter
/// 的文法版本跟不上實際在用的語法。
#[test]
fn every_source_file_in_this_project_parses_without_errors() {
    for rel in own_sources() {
        let parse = parse_file(&rel);
        assert!(
            parse.errors.is_empty(),
            "{rel} 解析出錯：{:?}",
            parse.errors
        );
        assert!(!parse.symbols.is_empty(), "{rel} 一個符號都沒抽到");
    }
}

/// 行號必須指向真正的宣告那一行。差一錯誤在這裡最容易發生，
/// 而且從輸出看不太出來——所以拿原始碼逐行比對。
#[test]
fn every_symbol_points_at_its_own_declaration() {
    for rel in own_sources() {
        let source = std::fs::read_to_string(&rel).unwrap();
        let lines: Vec<&str> = source.lines().collect();
        let parse = extract::extract(&rel, &source).unwrap();

        for sym in &parse.symbols {
            let idx = sym.start_line as usize - 1;
            let line = lines
                .get(idx)
                .unwrap_or_else(|| panic!("{rel}:{} 超出檔案範圍", sym.start_line));

            assert!(
                line.contains(&sym.name),
                "{rel}:{} 指到的是 `{line}`，但符號叫 `{}`",
                sym.start_line,
                sym.name
            );
            assert!(
                sym.end_line >= sym.start_line,
                "{rel} 的 {} 結束行在起始行之前",
                sym.qualified
            );
        }
    }
}

#[test]
fn monikers_are_unique_within_a_file() {
    for rel in own_sources() {
        let parse = parse_file(&rel);
        let mut seen: Vec<&str> = parse.symbols.iter().map(|s| s.moniker.as_str()).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            before,
            seen.len(),
            "{rel} 有重複的 moniker——兩個符號會在 intern 時撞成同一個 id"
        );
    }
}

/// 抽取層是純函數：整棵原始碼樹跑兩次，結果必須逐位元組相同。
/// 這是「用 content hash 跳過未變更檔案」與快取的前提。
#[test]
fn extracting_the_whole_tree_twice_gives_identical_results() {
    for rel in own_sources() {
        assert_eq!(parse_file(&rel), parse_file(&rel), "{rel} 的抽取結果不穩定");
    }
}

/// 對著自己的 `store` 模組，檢查幾個具體的抽取判斷。
/// 用「原始碼裡搜得到這一行」而不是寫死行號，才不會每次改檔案就壞。
#[test]
fn the_store_module_is_extracted_the_way_we_expect() {
    let rel = "src/store/mod.rs";
    let source = std::fs::read_to_string(rel).unwrap();
    let parse = extract::extract(rel, &source).unwrap();

    let open = parse
        .symbols
        .iter()
        .find(|s| s.qualified == "Store::open")
        .expect("沒有抽到 Store::open");

    assert_eq!(open.kind, Kind::Method, "impl 區塊裡的 fn 應該是方法");
    assert_eq!(open.name, "open");
    assert!(
        open.signature
            .as_deref()
            .unwrap()
            .starts_with("pub fn open"),
        "簽名不對：{:?}",
        open.signature
    );
    assert!(
        open.docstring.is_some(),
        "Store::open 有文件註解，卻沒有被抽到"
    );

    // `configure` 是模組層的自由函數，不是方法。
    let configure = parse
        .symbols
        .iter()
        .find(|s| s.qualified == "configure")
        .expect("沒有抽到 configure");
    assert_eq!(configure.kind, Kind::Function);

    // `mod tests` 底下的測試函數是函數，不是方法。
    let in_tests = parse
        .symbols
        .iter()
        .filter(|s| s.qualified.starts_with("tests::"))
        .collect::<Vec<_>>();
    assert!(!in_tests.is_empty());
    assert!(
        in_tests.iter().all(|s| s.kind == Kind::Function),
        "模組裡的函數被標成方法了"
    );
}

/// 一份刻意混合各種語法的 fixture。真實原始碼不一定同時出現這些形狀。
#[test]
fn a_mixed_fixture_covers_the_shapes_real_code_may_not_have() {
    let src = r#"
//! 模組層文件

use std::fmt;

/// 有屬性的型別
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Widget<T: Clone> {
    inner: T,
}

impl<T: Clone> Widget<T> {
    /// 建立
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
}

impl fmt::Display for Widget<u8> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "widget")
    }
}

pub trait Runnable {
    const NAME: &'static str;
    fn run(&self) -> u8;
    fn default_run(&self) -> u8 {
        0
    }
}

pub mod nested {
    pub mod deeper {
        pub fn buried() {}
    }
}

pub type Alias = Widget<u8>;
pub const LIMIT: usize = 10;
"#;

    let parse = extract::extract("src/fixture.rs", src).unwrap();
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let names: Vec<&str> = parse.symbols.iter().map(|s| s.qualified.as_str()).collect();
    for expected in [
        "Widget",
        "Widget::new",
        "Widget::fmt",
        "Runnable",
        "Runnable::run",
        "Runnable::default_run",
        "Runnable::NAME",
        "nested",
        "nested::deeper",
        "nested::deeper::buried",
        "Alias",
        "LIMIT",
    ] {
        assert!(
            names.contains(&expected),
            "少了 {expected}，有的是 {names:?}"
        );
    }

    let new = parse
        .symbols
        .iter()
        .find(|s| s.qualified == "Widget::new")
        .unwrap();
    assert_eq!(new.docstring.as_deref(), Some("建立"));

    let widget = parse
        .symbols
        .iter()
        .find(|s| s.qualified == "Widget")
        .unwrap();
    assert_eq!(
        widget.docstring.as_deref(),
        Some("有屬性的型別"),
        "屬性擋住了文件註解"
    );
    assert!(
        widget
            .signature
            .as_deref()
            .unwrap()
            .contains("Widget<T: Clone>"),
        "泛型參數不見了：{:?}",
        widget.signature
    );
}

/// 非 Rust 檔案不歸抽取層管，這不是錯誤。
#[test]
fn non_rust_files_are_skipped_rather_than_failing() {
    for path in ["README.md", "Cargo.toml", "src/store/schema.sql"] {
        assert!(
            extract::extract(path, "whatever").is_none(),
            "{path} 不該被當成原始碼"
        );
    }
}

/// 路徑分隔符不同的同一個檔案，必須產出相同的 moniker——
/// 否則同一份索引在 Windows 與 Linux 上對不起來。
#[test]
fn windows_and_posix_paths_agree_on_monikers() {
    let src = "fn f() {}\n";
    let posix = extract::extract("src/a/b.rs", src).unwrap();
    let windows = extract::extract(r"src\a\b.rs", src).unwrap();
    assert_eq!(posix, windows);
    assert_eq!(posix.symbols[0].moniker, "src/a/b.rs:function:f:1");
}

/// 一個空目錄樹不該讓收集器爆掉（`own_sources` 自己的守衛）。
#[test]
fn collecting_from_a_missing_directory_is_harmless() {
    let mut out: Vec<String> = Vec::new();
    collect(&PathBuf::from("這個目錄不存在-codegraph"), &mut out);
    assert!(out.is_empty());
}
