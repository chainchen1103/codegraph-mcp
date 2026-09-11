//! 抽取器共用的零件。
//!
//! 每個語言的走訪邏輯不一樣——Rust 要處理 `impl` 區塊與接收者型別，
//! TypeScript 要穿透 `export`，Python 的文件字串在本體裡——但走訪之後
//! 要做的事是同一件：組出限定名與 moniker、切出簽名、把型別引用寫進
//! 結果。那幾段每個語言抄一份沒有意義，抄第 15 份更沒有。
//!
//! 這裡只放**行為完全相同**的部分。語意有差別的留在各自的模組裡，硬
//! 塞進共用層只會變成一堆旗標。

use tree_sitter::Node;

use super::super::FileParse;
use super::super::ts;
use crate::extract::moniker;
use crate::model::{Kind, RawRef, RawSymbol, Rel};

/// 一個宣告在索引裡的樣子。
///
/// 簽名與文件字串由呼叫端算好：取法因語言而異（Python 的文件字串是本體
/// 裡的字串，其他語言是註解），但算出來之後的處置完全一樣。
pub struct Declaration<'a> {
    pub kind: Kind,
    pub name: &'a str,
    /// 祖先鏈上的名字，用來組限定名。
    pub container: &'a [String],
    pub signature: Option<String>,
    pub docstring: Option<String>,
    /// 有沒有本體。只有宣告沒有本體的符號，與別處那個定義是同一件東西
    /// 的兩面；不是函數的符號一律為真。
    pub has_body: bool,
}

/// 收下一個符號，回傳它的 moniker。
pub fn push(node: Node<'_>, path: &str, decl: Declaration<'_>, out: &mut FileParse) -> String {
    let start_line = ts::line_of(node);
    let moniker = moniker::build(path, decl.kind, decl.name, start_line);

    out.symbols.push(RawSymbol {
        moniker: moniker.clone(),
        name: decl.name.to_string(),
        qualified: qualify(decl.container, decl.name),
        kind: decl.kind,
        start_line,
        end_line: ts::end_line_of(node),
        signature: decl.signature,
        docstring: decl.docstring,
        has_body: decl.has_body,
    });

    moniker
}

/// 限定名：容器鏈接上自己的名字。
///
/// 分隔符一律是 `::`，即使 TypeScript 寫成 `Box.area`、Python 寫成
/// `Box.area`。點號在解析階段代表「對某個值呼叫方法」，限定名用點號會
/// 讓明確的宣告被誤判成接收者呼叫。
pub fn qualify(container: &[String], name: &str) -> String {
    if container.is_empty() {
        name.to_string()
    } else {
        format!("{}::{name}", container.join("::"))
    }
}

/// 宣告的簽名，也就是本體之前的部分。
///
/// `body_fields` 依序試，第一個存在的欄位就是切點；都沒有時整個節點都
/// 算簽名。`trim` 是切完之後要從尾端去掉的符號。
pub fn signature(
    node: Node<'_>,
    source: &str,
    body_fields: &[&str],
    trim: &[char],
) -> Option<String> {
    let full = ts::text(node, source);
    let cut = body_fields
        .iter()
        .find_map(|field| node.child_by_field_name(field))
        .map(|b| b.start_byte() - node.start_byte())
        .unwrap_or(full.len());

    let decl = full.get(..cut)?.trim_end().trim_end_matches(trim);
    let s = ts::collapse_whitespace(decl);
    if s.is_empty() { None } else { Some(s) }
}

/// 這個節點有沒有本體。
///
/// `body_fields` 與 [`signature`] 用同一份清單：切簽名的地方就是本體開始
/// 的地方，兩者判斷的是同一件事。
pub fn has_body(node: Node<'_>, body_fields: &[&str]) -> bool {
    body_fields
        .iter()
        .any(|field| node.child_by_field_name(field).is_some())
}

pub fn field_text<'a>(node: Node<'_>, field: &str, source: &'a str) -> Option<&'a str> {
    node.child_by_field_name(field).map(|n| ts::text(n, source))
}

/// 哪些節點算型別名。
///
/// 各語言的語法樹形狀不同：Rust 與 TypeScript 有專門的 `type_identifier`，
/// Python 的型別標註就是一般的運算式。
#[derive(Clone, Copy)]
pub struct TypeShapes {
    /// 節點本身就是型別名。
    pub leaves: &'static [&'static str],
    /// 帶路徑的型別，只取 `name` 欄位——`crate::a::Widget` 記成 `Widget`。
    pub scoped: &'static [&'static str],
    /// 不往裡面走的節點。字串形式的前向參考要求值才知道指向誰。
    pub opaque: &'static [&'static str],
}

/// 收集節點底下出現的型別名，同一個名字只記第一次出現的位置。
///
/// `declared` 是這個宣告自己引入的泛型參數，那些不指向任何符號。
pub fn gather_types(
    node: Node<'_>,
    source: &str,
    shapes: TypeShapes,
    declared: &[String],
    found: &mut Vec<(String, u32)>,
) {
    if shapes.opaque.contains(&node.kind()) {
        return;
    }

    let named = if shapes.leaves.contains(&node.kind()) {
        Some(node)
    } else if shapes.scoped.contains(&node.kind()) {
        node.child_by_field_name("name")
    } else {
        None
    };

    if let Some(named) = named {
        let name = ts::text(named, source);
        // `Self` 指的是所屬的型別，不是另一個符號。
        if name != "Self"
            && !declared.iter().any(|d| d == name)
            && !found.iter().any(|(n, _)| n == name)
        {
            found.push((name.to_string(), ts::line_of(named)));
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        gather_types(child, source, shapes, declared, found);
    }
}

/// 把收集到的型別名寫成 `UsesType` 引用。
pub fn emit_types(from: &str, found: Vec<(String, u32)>, out: &mut FileParse) {
    for (name, line) in found {
        out.refs.push(RawRef {
            from: from.to_string(),
            name,
            rel: Rel::UsesType,
            line,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_top_level_name_is_its_own_qualified_name() {
        assert_eq!(qualify(&[], "run"), "run");
    }

    #[test]
    fn a_nested_name_is_joined_with_double_colons() {
        let container = vec!["Box".to_string(), "Inner".to_string()];
        assert_eq!(qualify(&container, "run"), "Box::Inner::run");
    }
}
