//! tree-sitter 的共用輔助函數。

use std::cell::RefCell;

use tree_sitter::{Language, Node, Parser, Tree};

thread_local! {
    /// `Parser` 不是 `Sync` 且建立成本不低，每個執行緒持有一份重複使用。
    static PARSER: RefCell<Parser> = RefCell::new(Parser::new());
}

/// 以指定語言解析原始碼。
///
/// 回 `None` 表示無法建立語法樹，通常是語言設定錯誤。語法錯誤不屬於
/// 這種情況，tree-sitter 會回傳一棵包含 ERROR 節點的樹。
pub fn parse(language: &Language, source: &str) -> Option<Tree> {
    PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        parser.set_language(language).ok()?;
        parser.parse(source, None)
    })
}

/// 節點對應的原始碼片段。
///
/// 節點邊界一定落在 UTF-8 字元邊界上，可直接用位元組範圍切片。
pub fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

/// 節點的起始行，1 起算。
///
/// tree-sitter 的 row 從 0 起算，轉換集中在這裡。
pub fn line_of(node: Node<'_>) -> u32 {
    node.start_position().row as u32 + 1
}

/// 節點的結束行，1 起算。
pub fn end_line_of(node: Node<'_>) -> u32 {
    node.end_position().row as u32 + 1
}

/// 將多行宣告壓成單行，連續空白收斂為一個空格。
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

/// 收集節點正上方連續的文件註解。
///
/// `prefixes` 是該語言的文件註解前綴。`skip_kinds` 列出夾在註解與宣告
/// 之間但不打斷註解的節點，例如 Rust 的屬性。中間出現空行即停止。
pub fn leading_line_comments(
    node: Node<'_>,
    source: &str,
    comment_kind: &str,
    prefixes: &[&str],
    skip_kinds: &[&str],
) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut below_row = node.start_position().row;
    let mut cursor = node.prev_sibling();

    while let Some(prev) = cursor {
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

/// 節點最後一行有內容的行號。
///
/// 行註解節點的結束位置常落在下一行的第 0 欄，直接使用 `end_position`
/// 判斷相鄰性會忽略中間的空行。
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
        // 語法錯誤仍會建出一棵含 ERROR 節點的樹。
        assert!(parse(&lang, "fn (((").is_some());
    }
}
