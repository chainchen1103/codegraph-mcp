//! Kotlin 抽取器。
//!
//! 這個文法有兩處跟其他語言不同，都在這裡處理掉：
//!
//! 1. **型別沒有專用節點**。型別包在 `user_type` 裡，裡面就是普通的
//!    `identifier`——與參數名、變數名同一種節點。因此不能用共用的
//!    [`common::gather_types`] 直接掃，得先找出 `user_type` 再取名字。
//! 2. **`class` / `interface` / `object` 是同一種節點**（`class_declaration`），
//!    要看宣告開頭的關鍵字才分得出來。

use tree_sitter::{Language, Node};

use super::super::ts;
use super::super::{Extractor, FileParse, Import, ImportTarget};
use super::common::{self, Declaration};
use super::jvm;
use crate::extract::moniker;
use crate::model::{Kind, RawRef, Rel};

/// Kotlin 的文件註解。`/** ... */` 是 multiline_comment。
const DOC_PREFIXES: &[&str] = &["/**", "//"];

/// 夾在文件註解與宣告之間、不打斷註解的節點。
const DOC_SKIP: &[&str] = &["modifiers", "annotation"];

const SUFFIXES: &[&str] = &[".kt", ".kts"];

pub struct KotlinExtractor;

impl Extractor for KotlinExtractor {
    fn language(&self) -> &'static str {
        "kotlin"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["kt", "kts"]
    }

    fn extract(&self, rel_path: &str, source: &str) -> FileParse {
        let language: Language = tree_sitter_kotlin_ng::LANGUAGE.into();
        let Some(tree) = ts::parse(&language, source) else {
            return FileParse {
                errors: vec![format!("{rel_path}：tree-sitter 無法解析")],
                ..Default::default()
            };
        };

        let mut out = FileParse::default();
        let root = tree.root_node();
        if root.has_error() {
            out.errors
                .push(format!("{rel_path}：有語法錯誤，結果可能不完整"));
        }

        collect_imports(root, source, &mut out);

        let path = moniker::normalize_path(rel_path);
        walk(root, source, &path, &[], &mut out);
        out
    }

    fn module_path(&self, rel_path: &str) -> String {
        jvm::package_path(rel_path, SUFFIXES)
    }

    /// 類別內部寫 `render()` 就是 `this.render()`。
    fn implicit_receiver(&self) -> bool {
        true
    }
}

/// `import a.b.C` 引入的名字是 `C`。
fn collect_imports(root: Node<'_>, source: &str, out: &mut FileParse) {
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() != "import" && child.kind() != "import_header" {
            continue;
        }
        let text = ts::collapse_whitespace(ts::text(child, source))
            .trim_start_matches("import")
            .trim()
            .to_string();

        // 萬用字元與 `as` 別名以外的形式先不拆，交給名字比對。
        if text.contains('*') {
            continue;
        }
        let (path, alias) = match text.split_once(" as ") {
            Some((path, alias)) => (path.trim(), Some(alias.trim().to_string())),
            None => (text.as_str(), None),
        };

        let segments: Vec<String> = path
            .split('.')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let Some(last) = segments.last().cloned() else {
            continue;
        };

        out.imports.push(Import {
            local: alias.unwrap_or(last),
            target: ImportTarget::Rooted(segments),
            line: ts::line_of(child),
        });
    }
}

/// 走訪一層節點。`container` 是祖先鏈上的名字。
fn walk(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "class_declaration" | "object_declaration" => {
                declare(child, source, path, container, out);
            }
            "function_declaration" => function(child, source, path, container, out),
            "property_declaration" => property(child, source, path, container, out),
            _ => {}
        }
    }
}

/// `class` / `interface` / `object`。
///
/// 三者共用 `class_declaration` 這個節點，種類要看宣告開頭的關鍵字。
fn declare(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let Some(name) = common::field_text(node, "name", source) else {
        return;
    };
    let kind = declared_kind(node, source);
    let moniker = push(node, source, path, container, kind, name, out);

    // 建構參數與繼承／實作的型別是這個宣告的依賴。
    let mut found = Vec::new();
    for part in node.named_children(&mut node.walk()) {
        if matches!(
            part.kind(),
            "primary_constructor" | "delegation_specifiers" | "type_parameters"
        ) {
            gather_types(part, source, &mut found);
        }
    }
    emit(&moniker, found, out);

    let mut nested = container.to_vec();
    nested.push(name.to_string());
    for body in node.named_children(&mut node.walk()) {
        if body.kind() == "class_body" || body.kind() == "enum_class_body" {
            walk(body, source, path, &nested, out);
        }
    }
}

/// 宣告開頭的關鍵字決定種類。
///
/// 文法把 `class` / `interface` / `object` 收在同一種節點底下，只有那個
/// 關鍵字分得開。取不到就當成 class——那是最常見的一種。
fn declared_kind(node: Node<'_>, source: &str) -> Kind {
    let text = ts::text(node, source);
    for word in text.split_whitespace() {
        match word {
            "interface" => return Kind::Interface,
            "object" => return Kind::Class,
            "enum" => return Kind::Enum,
            "class" => return Kind::Class,
            _ => {}
        }
    }
    Kind::Class
}

/// `fun`：簽名記型別，本體記呼叫。
fn function(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let Some(name) = common::field_text(node, "name", source) else {
        return;
    };
    // 頂層的是函數，類別裡的是方法。
    let kind = if container.is_empty() {
        Kind::Function
    } else {
        Kind::Method
    };
    let moniker = push(node, source, path, container, kind, name, out);

    // 型別只看簽名。本體要跳過——它跟參數是同一層的兄弟節點。
    let mut found = Vec::new();
    for part in node.named_children(&mut node.walk()) {
        if part.kind() == "function_body" {
            continue;
        }
        gather_types(part, source, &mut found);
    }
    emit(&moniker, found, out);

    for body in node.named_children(&mut node.walk()) {
        if body.kind() == "function_body" {
            collect_calls(body, source, &moniker, out);
        }
    }
}

/// `val` / `var`。
fn property(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let Some(declaration) = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "variable_declaration")
    else {
        return;
    };
    let Some(name) = declaration
        .named_children(&mut declaration.walk())
        .find(|c| c.kind() == "identifier")
        .map(|n| ts::text(n, source))
    else {
        return;
    };
    let moniker = push(node, source, path, container, Kind::Const, name, out);

    let mut found = Vec::new();
    gather_types(declaration, source, &mut found);
    emit(&moniker, found, out);

    collect_calls(node, source, &moniker, out);
}

/// 收下一個符號，補上 Kotlin 特有的簽名與註解取法。
fn push(
    node: Node<'_>,
    source: &str,
    path: &str,
    container: &[String],
    kind: Kind,
    name: &str,
    out: &mut FileParse,
) -> String {
    common::push(
        node,
        path,
        Declaration {
            kind,
            name,
            container,
            signature: common::signature(node, source, &["body"], &['=']),
            docstring: ts::leading_line_comments(
                node,
                source,
                "multiline_comment",
                DOC_PREFIXES,
                DOC_SKIP,
            ),
        },
        out,
    )
}

/// 收集節點底下 `user_type` 指到的型別名。
///
/// 不能沿用共用的走訪：Kotlin 的型別就是 `identifier`，與參數名同一種
/// 節點，直接掃會把參數名也當成型別。先定位 `user_type`，再取它的第一個
/// 名字——泛型引數在 `type_arguments` 底下，那裡面的 `user_type` 會被
/// 遞迴收到。
fn gather_types(node: Node<'_>, source: &str, found: &mut Vec<(String, u32)>) {
    if node.kind() == "user_type" {
        if let Some(name) = node
            .named_children(&mut node.walk())
            .find(|c| c.kind() == "identifier")
        {
            let text = ts::text(name, source);
            if !found.iter().any(|(n, _)| n == text) {
                found.push((text.to_string(), ts::line_of(name)));
            }
        }
        // 泛型引數仍要走進去。
        for child in node.named_children(&mut node.walk()) {
            if child.kind() == "type_arguments" {
                gather_types(child, source, found);
            }
        }
        return;
    }

    for child in node.named_children(&mut node.walk()) {
        gather_types(child, source, found);
    }
}

fn emit(from: &str, found: Vec<(String, u32)>, out: &mut FileParse) {
    common::emit_types(from, found, out);
}

/// 走遍節點底下所有的呼叫，記到 `from` 名下。
fn collect_calls(node: Node<'_>, source: &str, from: &str, out: &mut FileParse) {
    if node.kind() == "call_expression"
        && let Some(target) = node.named_child(0)
        && let Some(name) = callee_name(target, source)
    {
        out.refs.push(RawRef {
            from: from.to_string(),
            name,
            rel: Rel::Calls,
            line: ts::line_of(node),
        });
    }

    for child in node.named_children(&mut node.walk()) {
        collect_calls(child, source, from, out);
    }
}

/// 被呼叫者在原始碼裡的寫法。
///
/// Kotlin 沒有 `new`：`Box()` 與 `helper()` 長得一樣，是不是建構函數要到
/// 解析階段比對候選的種類才知道。
fn callee_name(target: Node<'_>, source: &str) -> Option<String> {
    match target.kind() {
        "identifier" => Some(ts::text(target, source).to_string()),
        // a.b —— 兩個 identifier 並排，沒有欄位名。
        "navigation_expression" => {
            let mut cursor = target.walk();
            let mut parts = target.named_children(&mut cursor);
            let receiver = ts::collapse_whitespace(ts::text(parts.next()?, source));
            let method = ts::text(parts.next()?, source).to_string();

            if jvm::looks_like_type(&receiver) {
                return Some(format!("{receiver}::{method}"));
            }
            Some(format!("{receiver}.{method}"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> FileParse {
        KotlinExtractor.extract("src/main/kotlin/com/example/A.kt", src)
    }

    fn names(p: &FileParse) -> Vec<&str> {
        p.symbols.iter().map(|s| s.qualified.as_str()).collect()
    }

    fn refs_by(p: &FileParse, from_name: &str, rel: Rel) -> Vec<String> {
        let from = p
            .symbols
            .iter()
            .find(|s| s.name == from_name || s.qualified == from_name)
            .unwrap_or_else(|| panic!("找不到 {from_name}，有的是 {:?}", names(p)));
        p.refs
            .iter()
            .filter(|r| r.from == from.moniker && r.rel == rel)
            .map(|r| r.name.clone())
            .collect()
    }

    #[test]
    fn top_level_declarations_are_extracted() {
        let p = parse("class Box\ninterface Shape\nfun make() {}\nval LIMIT = 10\n");

        assert_eq!(names(&p), ["Box", "Shape", "make", "LIMIT"]);
        let kinds: Vec<Kind> = p.symbols.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [Kind::Class, Kind::Interface, Kind::Function, Kind::Const]
        );
    }

    /// `class` 與 `interface` 是同一種節點，靠關鍵字分辨。
    #[test]
    fn the_leading_keyword_decides_the_kind() {
        assert_eq!(parse("interface Shape").symbols[0].kind, Kind::Interface);
        assert_eq!(parse("class Box").symbols[0].kind, Kind::Class);
    }

    #[test]
    fn methods_are_qualified_by_their_class() {
        let p = parse("class Box {\n    fun area(): Int = 1\n}\n");

        assert_eq!(names(&p), ["Box", "Box::area"]);
        assert_eq!(p.symbols[1].kind, Kind::Method);
    }

    #[test]
    fn a_signature_records_the_types_it_mentions() {
        let p = parse("fun make(w: Widget): Report? = null\n");

        let types = refs_by(&p, "make", Rel::UsesType);
        assert!(types.contains(&"Widget".to_string()), "{types:?}");
        assert!(types.contains(&"Report".to_string()), "{types:?}");
    }

    /// 參數名與型別在語法樹上同樣是 identifier，只有型別能進來。
    #[test]
    fn parameter_names_are_not_mistaken_for_types() {
        let p = parse("fun make(widget: Widget) {}\n");

        assert_eq!(refs_by(&p, "make", Rel::UsesType), ["Widget"]);
    }

    #[test]
    fn constructor_parameters_and_supertypes_are_type_references() {
        let p = parse("class Box(val inner: Widget) : Shape\n");

        let types = refs_by(&p, "Box", Rel::UsesType);
        assert!(types.contains(&"Widget".to_string()), "{types:?}");
        assert!(types.contains(&"Shape".to_string()), "{types:?}");
    }

    #[test]
    fn calls_are_attributed_to_the_enclosing_declaration() {
        let p = parse("fun outer() {\n    helper()\n    box.area()\n}\n");

        assert_eq!(refs_by(&p, "outer", Rel::Calls), ["helper", "box.area"]);
    }

    /// 接收者大寫開頭是型別，改寫成限定名。
    #[test]
    fn a_call_on_a_type_becomes_a_qualified_name() {
        let p = parse("fun outer(): Int = Helper.compute()\n");

        assert_eq!(refs_by(&p, "outer", Rel::Calls), ["Helper::compute"]);
    }

    #[test]
    fn imports_bind_the_simple_name() {
        let p = parse("import com.example.util.Helper\nclass Box\n");

        assert_eq!(p.imports.len(), 1);
        assert_eq!(p.imports[0].local, "Helper");
    }

    #[test]
    fn an_aliased_import_binds_the_alias() {
        let p = parse("import com.example.util.Helper as H\nclass Box\n");

        assert_eq!(p.imports[0].local, "H");
    }

    #[test]
    fn a_wildcard_import_binds_nothing() {
        let p = parse("import com.example.util.*\nclass Box\n");

        assert!(p.imports.is_empty(), "{:?}", p.imports);
    }

    #[test]
    fn module_paths_follow_the_build_layout() {
        assert_eq!(
            KotlinExtractor.module_path("src/main/kotlin/com/example/app/Box.kt"),
            "com::example::app"
        );
    }

    #[test]
    fn a_syntax_error_still_yields_what_was_parsed() {
        let p = parse("fun ok() {}\nfun broken( {\n");

        assert!(!p.errors.is_empty());
        assert!(names(&p).contains(&"ok"), "{:?}", names(&p));
    }
}
