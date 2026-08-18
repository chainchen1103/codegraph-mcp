//! 從原始碼抽取符號與引用。
//!
//! 抽取是純函數：相同的路徑與內容永遠得到相同的結果，過程中不存取
//! 資料庫、也不做跨檔推論。跨檔的解析由 resolve 層負責。

pub mod lang;
pub mod moniker;
pub mod ts;

use std::path::Path;

use crate::model::{RawRef, RawSymbol};

/// import 指向的位置，翻成與語言無關的形式。
///
/// 每個語言的 import 語法都不同，但問的是同一件事：**這個名字是從哪個
/// 檔案來的**。抽取器負責把自己語言的寫法翻成這三種之一，解析階段就只
/// 需要做與語言無關的路徑比對。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportTarget {
    /// 相對於發出 import 的那個檔案。TypeScript 的 `./utils`。
    Relative(String),
    /// 相對於專案根目錄的路徑段。Python 的 `a.b`、Rust 的 `crate::a::b`。
    Rooted(Vec<String>),
    /// 專案外部，不必再找。標準函式庫與第三方套件都算。
    External,
}

/// 一條 import 在這個檔案裡引入的名字。
///
/// `local` 是這個檔案裡看得到的寫法。`import * as utils from './u'` 的
/// `local` 是 `utils`，`from a.b import thing` 的是 `thing`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    pub local: String,
    pub target: ImportTarget,
    pub line: u32,
}

/// 單一檔案的抽取結果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileParse {
    pub symbols: Vec<RawSymbol>,
    pub refs: Vec<RawRef>,
    /// 這個檔案 import 了什麼。解析階段最強的一階線索。
    pub imports: Vec<Import>,
    /// 語法錯誤等非致命問題。有錯誤時仍會回傳已抽到的符號。
    pub errors: Vec<String>,
}

impl FileParse {
    /// 沒有抽到任何符號或引用。
    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty() && self.refs.is_empty()
    }
}

/// 單一語言的抽取器。
pub trait Extractor: Send + Sync {
    /// 語言名稱，寫入 `files.language`。
    fn language(&self) -> &'static str;

    /// 認得的副檔名，不含點。
    fn extensions(&self) -> &'static [&'static str];

    /// 抽取單一檔案。
    ///
    /// `rel_path` 必須相對於專案根目錄，它是 moniker 的組成部分。
    fn extract(&self, rel_path: &str, source: &str) -> FileParse;

    /// 這個檔案在該語言模組樹中的位置。
    ///
    /// 符號的限定名只記錄檔案內部的巢狀結構，`src/extract/ts.rs` 裡的
    /// `parse` 限定名就是 `parse`。引用寫成 `ts::parse` 時前面那一段
    /// 指的是檔案位置，解析階段要靠這個值才對得上。
    ///
    /// 每個語言的規則都不一樣，所以是抽取器的責任而不是專案佈局的：
    /// Rust 的 `mod.rs` 代表所在目錄，Python 的是 `__init__.py`，
    /// TypeScript 根本不用這種形式。沒有這一層的語言回空字串。
    fn module_path(&self, rel_path: &str) -> String;

    /// import 指向一個目錄時，代表那個目錄的檔名。
    ///
    /// `./components` 指的是 `components/index.ts`，`import a.b` 指的可能
    /// 是 `a/b/__init__.py`。沒有這種慣例的語言回空陣列。
    fn directory_modules(&self) -> &'static [&'static str] {
        &[]
    }
}

/// 依副檔名取得抽取器，不支援的副檔名回 `None`。
pub fn extractor_for(path: &Path) -> Option<&'static dyn Extractor> {
    let ext = path.extension()?.to_str()?;
    lang::by_extension(ext)
}

/// 抽取一個檔案。呼叫端負責讀檔，本層不做 I/O。
pub fn extract(rel_path: &str, source: &str) -> Option<FileParse> {
    let ex = extractor_for(Path::new(rel_path))?;
    Some(ex.extract(rel_path, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_files_are_recognised() {
        let ex = extractor_for(Path::new("src/main.rs")).unwrap();
        assert_eq!(ex.language(), "rust");
    }

    #[test]
    fn unknown_extensions_are_not_an_error() {
        assert!(extractor_for(Path::new("README.md")).is_none());
        assert!(extractor_for(Path::new("Makefile")).is_none());
        assert!(extractor_for(Path::new("noext")).is_none());
        assert!(extract("assets/logo.png", "").is_none());
    }

    #[test]
    fn extraction_is_a_pure_function() {
        let src = "fn a() {}\nfn b() {}\n";
        let first = extract("src/x.rs", src).unwrap();
        let second = extract("src/x.rs", src).unwrap();
        assert_eq!(first, second, "同樣的輸入產生了不同的結果");
    }

    #[test]
    fn an_empty_parse_reports_itself_as_empty() {
        let parse = extract("src/x.rs", "").unwrap();
        assert!(parse.is_empty());
        assert!(parse.errors.is_empty());
    }
}
