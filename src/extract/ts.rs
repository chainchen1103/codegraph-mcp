//! tree-sitter 的共用機制：parser 重用、節點文字擷取、註解收集。

use std::cell::RefCell;

use tree_sitter::{Language, Node, Parser, Tree};

thread_local! {
    /// `Parser` 不是 `Sync`，而且建立成本不低。每個執行緒持有自己的一份，
    /// 在 rayon 的 worker 上重複使用（ARCHITECTURE.md §5.2）。
    static PARSER: RefCell<Parser> = RefCell::new(Parser::new());
}

/// 用指定語言解析原始碼。
///
/// 回 `None` 代表 tree-sitter 連語法樹都建不出來（語言設定錯誤或
/// 輸入過大）。**語法錯誤不會走到這裡**——tree-sitter 會盡力恢復並
/// 回傳一棵含 ERROR 節點的樹，那才是編輯到一半的檔案的常態。
pub fn parse(language: &Language, source: &str) -> Option<Tree> {
    PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        parser.set_language(language).ok()?;
        parser.parse(source, None)
    })
}

/// 節點對應的原始碼片段。
///
/// 用 byte range 直接切 `&str` 是安全的：tree-sitter 的節點邊界
/// 一定落在 UTF-8 字元邊界上。
pub fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

/// tree-sitter 的 row 是 0 起算，我們的行號一律 1 起算。
///
/// 這個差一錯誤如果漏掉，會一路污染到輸出，而且從結果看不太出來
/// ——所以集中在這一個函數，別在各處自己 +1。
pub fn line_of(node: Node<'_>) -> u32 {
    node.start_position().row as u32 + 1
}

pub fn end_line_of(node: Node<'_>) -> u32 {
    node.end_position().row as u32 + 1
}

/// 把多行、多空白的宣告壓成一行。
///
/// 簽名是要給人（和模型）一眼看懂的，原始碼裡的換行與縮排在這裡
/// 只是噪音。
pub fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            in_space = true;
            continue;
        }
        if in_space && !out.is_empty() {
            out.push(' ');
        }
        in_space = false;
        out.push(ch);
    }
    out
}

/// 收集節點正上方**連續**的文件註解。
///
/// - `prefixes`：該語言的文件註解前綴（Rust 是 `///` 與 `//!`）。
/// - `skip_kinds`：夾在註解與宣告之間、但不打斷這段註解的節點種類。
///   Rust 的 `#[derive(...)]` 就長在中間，不跳過的話有屬性的型別
///   全部都會抓不到文件。
///
/// 中間隔了空行就停止——那段註解屬於別的東西。
pub fn leading_line_comments(
    node: Node<'_>,
    source: &str,
    comment_kind: &str,
    prefixes: &[&str],
    skip_kinds: &[&str],
) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    // 目前這段註解的下緣。每收一行就往上移。
    let mut below_row = node.start_position().row;
    let mut cursor = node.prev_sibling();

    while let Some(prev) = cursor {
        // 中間空了一整行就不算同一段。
        if below_row.saturating_sub(last_content_row(prev)) > 1 {
            break;
        }

        if skip_kinds.contains(&prev.kind()) {
            below_row = prev.start_position().row;
            cursor = prev.prev_sibling();
            continue;
        }

        if prev.kind() != comment_kind {
            break;
        }

        let raw = text(prev, source).trim();
        let Some(body) = strip_any_prefix(raw, prefixes) else {
            break;
        };
        lines.push(body.trim().to_string());
        below_row = prev.start_position().row;
        cursor = prev.prev_sibling();
    }

    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    Some(lines.join("\n"))
}

/// 節點最後一行**有內容**的行號。
///
/// 行註解節點的結束位置常常落在下一行的第 0 欄（它把換行也算進去），
/// 直接拿 `end_position().row` 判斷相鄰性會把中間的空行吃掉，
/// 於是上一段不相干的註解會被誤收進來。
fn last_content_row(node: Node<'_>) -> usize {
    let end = node.end_position();
    if end.column == 0 {
        end.row.saturating_sub(1)
    } else {
        end.row
    }
}

fn strip_any_prefix<'a>(s: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    prefixes.iter().find_map(|p| s.strip_prefix(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_collapses_to_single_spaces() {
        assert_eq!(
            collapse_whitespace("fn  a(\n    x: u8,\n) -> u8"),
            "fn a( x: u8, ) -> u8"
        );
        assert_eq!(collapse_whitespace(""), "");
        assert_eq!(collapse_whitespace("   "), "");
        assert_eq!(collapse_whitespace("\n\tone\n\ttwo\n"), "one two");
    }

    #[test]
    fn line_numbers_are_one_based() {
        let lang: Language = tree_sitter_rust::LANGUAGE.into();
        let tree = parse(&lang, "fn a() {}\nfn b() {\n}\n").unwrap();
        let root = tree.root_node();

        let first = root.child(0).unwrap();
        assert_eq!(line_of(first), 1);
        assert_eq!(end_line_of(first), 1);

        let second = root.child(1).unwrap();
        assert_eq!(line_of(second), 2);
        assert_eq!(end_line_of(second), 3);
    }

    #[test]
    fn node_text_slices_the_original_source() {
        let lang: Language = tree_sitter_rust::LANGUAGE.into();
        let src = "fn 中文函數() {}";
        let tree = parse(&lang, src).unwrap();
        let f = tree.root_node().child(0).unwrap();
        assert_eq!(text(f, src), src, "多位元組字元把 byte range 切壞了");
    }

    #[test]
    fn prefix_stripping_takes_the_first_match() {
        assert_eq!(strip_any_prefix("/// doc", &["///", "//!"]), Some(" doc"));
        assert_eq!(
            strip_any_prefix("//! inner", &["///", "//!"]),
            Some(" inner")
        );
        assert_eq!(strip_any_prefix("// plain", &["///", "//!"]), None);
    }

    #[test]
    fn parsing_an_unsupported_input_is_not_a_panic() {
        let lang: Language = tree_sitter_rust::LANGUAGE.into();
        assert!(parse(&lang, "fn a() {}").is_some());
        // 語法錯誤仍然會建出一棵樹（含 ERROR 節點），不是 None。
        assert!(parse(&lang, "fn (((").is_some());
    }
}
