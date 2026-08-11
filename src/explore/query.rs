//! 查詢字串的拆解。
//!
//! 輸入可以混合符號名、限定名、檔案路徑與自然語言。這一層只做分類，
//! 不查資料庫。

/// 拆解後的查詢。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    /// 符號名或限定名，例如 `open`、`Store::open`。
    pub names: Vec<String>,
    /// 檔案路徑，例如 `src/store/mod.rs`。
    pub paths: Vec<String>,
    /// 無法歸類的詞，交給全文檢索。
    pub text: Vec<String>,
}

impl Query {
    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.paths.is_empty() && self.text.is_empty()
    }

    /// 使用者輸入的所有詞，維持原本的順序。
    pub fn tokens(&self) -> Vec<&str> {
        self.names
            .iter()
            .chain(&self.paths)
            .chain(&self.text)
            .map(String::as_str)
            .collect()
    }
}

/// 依空白切開並分類每個詞。
pub fn parse(input: &str) -> Query {
    let mut q = Query::default();

    for raw in input.split_whitespace() {
        let token = raw.trim_matches(|c: char| c == '"' || c == '\'' || c == ',');
        if token.is_empty() {
            continue;
        }

        if looks_like_path(token) {
            q.paths.push(token.to_string());
        } else if looks_like_name(token) {
            q.names.push(token.to_string());
        } else {
            q.text.push(token.to_string());
        }
    }

    q
}

/// 含有路徑分隔符，或帶有原始碼副檔名。
fn looks_like_path(token: &str) -> bool {
    if token.contains('/') || token.contains('\\') {
        return true;
    }
    match token.rsplit_once('.') {
        Some((stem, ext)) => !stem.is_empty() && crate::extract::lang::by_extension(ext).is_some(),
        None => false,
    }
}

/// 看起來像識別字：只由識別字字元組成，或是以 `::` 或 `.` 連接的限定名。
///
/// 中文與其他非識別字字元一律歸到全文檢索，那是自然語言而不是符號名。
fn looks_like_name(token: &str) -> bool {
    let stripped = token.replace("::", "_").replace('.', "_");
    !stripped.is_empty()
        && stripped
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(input: &str) -> Vec<String> {
        parse(input).names
    }

    #[test]
    fn plain_identifiers_are_names() {
        assert_eq!(names("open"), vec!["open"]);
        assert_eq!(names("open_store build2"), vec!["open_store", "build2"]);
    }

    #[test]
    fn qualified_names_stay_intact() {
        assert_eq!(names("Store::open"), vec!["Store::open"]);
        assert_eq!(names("a::b::c"), vec!["a::b::c"]);
        assert_eq!(names("obj.method"), vec!["obj.method"]);
    }

    #[test]
    fn paths_are_recognised_by_separator_or_extension() {
        let q = parse("src/store/mod.rs lib.rs Store::open");
        assert_eq!(q.paths, vec!["src/store/mod.rs", "lib.rs"]);
        assert_eq!(q.names, vec!["Store::open"]);
    }

    #[test]
    fn windows_separators_count_as_paths() {
        assert_eq!(parse(r"src\store\mod.rs").paths, vec![r"src\store\mod.rs"]);
    }

    /// 副檔名不支援的檔名不是路徑，也不該被當成符號名。
    #[test]
    fn unsupported_extensions_fall_through_to_text() {
        let q = parse("README.md");
        assert!(q.paths.is_empty());
        assert_eq!(q.names, vec!["README.md"], "點號連接的詞仍視為限定名");
    }

    #[test]
    fn natural_language_goes_to_full_text() {
        let q = parse("索引 怎麼 建立");
        assert!(q.names.is_empty());
        assert!(q.paths.is_empty());
        assert_eq!(q.text, vec!["索引", "怎麼", "建立"]);
    }

    #[test]
    fn mixed_input_is_split_by_category() {
        let q = parse("Store::open src/store/mod.rs 怎麼運作");
        assert_eq!(q.names, vec!["Store::open"]);
        assert_eq!(q.paths, vec!["src/store/mod.rs"]);
        assert_eq!(q.text, vec!["怎麼運作"]);
        assert_eq!(
            q.tokens(),
            vec!["Store::open", "src/store/mod.rs", "怎麼運作"]
        );
    }

    #[test]
    fn punctuation_around_tokens_is_trimmed() {
        assert_eq!(names("\"open\", 'close'"), vec!["open", "close"]);
    }

    #[test]
    fn an_empty_query_is_reported_as_such() {
        assert!(parse("").is_empty());
        assert!(parse("   \t \n ").is_empty());
    }
}
