//! 原始碼 → 符號與原始引用。
//!
//! **這一層是純函數：** 同樣的 `(path, source)` 永遠產出同樣的結果，
//! 不碰 DB、不做跨檔推論（ARCHITECTURE.md §5.1）。這讓抽取可以並行、
//! 可以用 content hash 跳過未變更的檔案、可以用 fixture 做快照測試。
//! 跨檔的事全部推給 resolve 層。

pub mod lang;
pub mod moniker;
pub mod ts;

use std::path::Path;

use crate::model::{RawRef, RawSymbol};

/// 單一檔案的抽取結果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileParse {
    pub symbols: Vec<RawSymbol>,
    /// Stage 6 才會有內容——目前抽取只認符號，不認呼叫。
    pub refs: Vec<RawRef>,
    /// 語法錯誤等非致命問題。**有錯誤仍然回傳已抽到的符號**：
    /// 使用者編輯到一半的檔案本來就常常是壞的，這時候還能給出
    /// 檔案上半部的結構，比整個放棄有用。
    pub errors: Vec<String>,
}

impl FileParse {
    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty() && self.refs.is_empty()
    }
}

/// 一種語言的抽取器。
pub trait Extractor: Send + Sync {
    /// 寫進 `files.language` 的名字。
    fn language(&self) -> &'static str;

    /// 認得的副檔名（不含點）。
    fn extensions(&self) -> &'static [&'static str];

    /// 抽取單一檔案。
    ///
    /// `rel_path` 必須是**相對專案根目錄**的路徑——它是 moniker 的
    /// 組成部分，絕對路徑會把「誰的機器」烙進索引。
    fn extract(&self, rel_path: &str, source: &str) -> FileParse;
}

/// 依副檔名挑抽取器。認不得的副檔名回 `None`——這不是錯誤，
/// 是「這個檔案不歸我管」。
pub fn extractor_for(path: &Path) -> Option<&'static dyn Extractor> {
    let ext = path.extension()?.to_str()?;
    lang::by_extension(ext)
}

/// 抽取一個檔案。呼叫端負責讀檔——這一層不碰 I/O，才能保持純函數。
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
