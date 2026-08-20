//! Scala 抽取器。
//!
//! `object` 是單例，語法上是宣告、語意上既是型別也是值。記成 `Class`：
//! 它的成員要掛在它名下，而那正是 `Class` 這個種類的用途。
//!
//! 呼叫的接收者是型別還是值，跟 Java 一樣靠命名慣例分辨
//! （見 [`super::jvm::looks_like_type`]）。

use tree_sitter::{Language, Node};

use super::super::ts;
use super::super::{Extractor, FileParse, Import, ImportTarget};
use super::common::{self, Declaration, TypeShapes};
use super::jvm;
use crate::extract::moniker;
use crate::model::{Kind, RawRef, Rel};

/// Scala 的文件註解。`/** ... */` 是 block_comment。
const DOC_PREFIXES: &[&str] = &["/**", "//"];

/// 夾在文件註解與宣告之間、不打斷註解的節點。
const DOC_SKIP: &[&str] = &["annotation", "modifiers"];

/// 型別名在 Scala 的語法樹裡長什麼樣。
const TYPES: TypeShapes = TypeShapes {
    leaves: &["type_identifier"],
    scoped: &["projected_type"],
    opaque: &[],
};

const SUFFIXES: &[&str] = &[".scala", ".sc"];

pub struct ScalaExtractor;

impl Extractor for ScalaExtractor {
    fn language(&self) -> &'static str {
        "scala"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["scala", "sc"]
    }

    fn extract(&self, rel_path: &str, source: &str) -> FileParse {
        let language: Language = tree_sitter_scala::LANGUAGE.into();
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
///
/// 路徑在語法樹上是一串重複的 `path` 欄位，取整段原文再切點號比逐欄位
/// 取穩：大括號選擇器（`import a.{B, C}`）的形狀又是另一回事，那種寫法
/// 這裡不拆，交給名字比對。
fn collect_imports(root: Node<'_>, source: &str, out: &mut FileParse) {
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() != "import_declaration" {
            continue;
        }
        let text = ts::collapse_whitespace(ts::text(child, source))
            .trim_start_matches("import")
            .trim()
            .to_string();

        // 大括號與萬用字元都沒有指名單一目標。
        if text.contains('{') || text.ends_with('_') || text.ends_with('*') {
            continue;
        }

        let segments: Vec<String> = text
            .split('.')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let Some(last) = segments.last().cloned() else {
            continue;
        };

        out.imports.push(Import {
            local: last,
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
            "class_definition" | "case_class_definition" => {
                declare(child, source, path, container, Kind::Class, out);
            }
            // object 是單例，成員掛在它名下，這正是 Class 的用途。
            "object_definition" => declare(child, source, path, container, Kind::Class, out),
            "trait_definition" => declare(child, source, path, container, Kind::Trait, out),
            "enum_definition" => declare(child, source, path, container, Kind::Enum, out),
            "type_definition" => leaf(child, source, path, container, Kind::TypeAlias, out),
            // function_declaration 是沒有本體的抽象方法。
            "function_definition" | "function_declaration" => {
                function(child, source, path, container, out);
            }
            "val_definition" | "var_definition" | "val_declaration" | "var_declaration" => {
                value(child, source, path, container, out);
            }
            _ => {}
        }
    }
}

/// 有本體、內部還有其他宣告的容器。
fn declare(
    node: Node<'_>,
    source: &str,
    path: &str,
    container: &[String],
    kind: Kind,
    out: &mut FileParse,
) {
    let Some(name) = common::field_text(node, "name", source) else {
        return;
    };
    let moniker = push(node, source, path, container, kind, name, out);

    // 建構參數、繼承與混入的型別都是這個宣告的依賴。
    let mut found = Vec::new();
    for field in ["class_parameters", "extend", "type_parameters"] {
        if let Some(part) = node.child_by_field_name(field) {
            common::gather_types(part, source, TYPES, &[], &mut found);
        }
    }
    common::emit_types(&moniker, found, out);

    let mut nested = container.to_vec();
    nested.push(name.to_string());
    if let Some(body) = node.child_by_field_name("body") {
        walk(body, source, path, &nested, out);
    }
}

/// 沒有本體的宣告。
fn leaf(
    node: Node<'_>,
    source: &str,
    path: &str,
    container: &[String],
    kind: Kind,
    out: &mut FileParse,
) {
    if let Some(name) = common::field_text(node, "name", source) {
        let moniker = push(node, source, path, container, kind, name, out);
        let mut found = Vec::new();
        common::gather_types(node, source, TYPES, &[], &mut found);
        common::emit_types(&moniker, found, out);
    }
}

/// `def` 的宣告與定義。
fn function(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let Some(name) = common::field_text(node, "name", source) else {
        return;
    };
    let moniker = push(node, source, path, container, Kind::Method, name, out);

    let mut found = Vec::new();
    for field in ["parameters", "return_type", "type_parameters"] {
        if let Some(part) = node.child_by_field_name(field) {
            common::gather_types(part, source, TYPES, &[], &mut found);
        }
    }
    common::emit_types(&moniker, found, out);

    if let Some(body) = node.child_by_field_name("body") {
        collect_calls(body, source, &moniker, out);
    }
}

/// `val` / `var`。
fn value(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let Some(pattern) = node.child_by_field_name("pattern") else {
        return;
    };
    // 解構賦值沒有單一名字可記。
    if pattern.kind() != "identifier" {
        return;
    }
    let name = ts::text(pattern, source);
    let moniker = push(node, source, path, container, Kind::Const, name, out);

    if let Some(annotation) = node.child_by_field_name("type") {
        let mut found = Vec::new();
        common::gather_types(annotation, source, TYPES, &[], &mut found);
        common::emit_types(&moniker, found, out);
    }
    if let Some(value) = node.child_by_field_name("value") {
        collect_calls(value, source, &moniker, out);
    }
}

/// 收下一個符號，補上 Scala 特有的簽名與註解取法。
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
            signature: common::signature(node, source, &["body", "value"], &['=']),
            has_body: common::has_body(node, &["body", "value"]),
            docstring: ts::leading_line_comments(
                node,
                source,
                "block_comment",
                DOC_PREFIXES,
                DOC_SKIP,
            ),
        },
        out,
    )
}

/// 走遍節點底下所有的呼叫，記到 `from` 名下。
fn collect_calls(node: Node<'_>, source: &str, from: &str, out: &mut FileParse) {
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && let Some(name) = callee_name(function, source)
    {
        out.refs.push(RawRef {
            from: from.to_string(),
            name,
            rel: Rel::Calls,
            line: ts::line_of(node),
        });
    }

    // `new Box(...)` 的目標是型別。
    if node.kind() == "instance_expression" {
        let mut found = Vec::new();
        common::gather_types(node, source, TYPES, &[], &mut found);
        common::emit_types(from, found, out);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, source, from, out);
    }
}

/// 被呼叫者在原始碼裡的寫法。
fn callee_name(function: Node<'_>, source: &str) -> Option<String> {
    match function.kind() {
        "identifier" => Some(ts::text(function, source).to_string()),
        "field_expression" => {
            let value = function.child_by_field_name("value")?;
            let field = function.child_by_field_name("field")?;
            let method = ts::text(field, source);
            let receiver = ts::collapse_whitespace(ts::text(value, source));

            if value.kind() == "identifier" && jvm::looks_like_type(&receiver) {
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
        ScalaExtractor.extract("src/main/scala/com/example/A.scala", src)
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
        let p = parse(
            "class Box\n\
             trait Shape\n\
             object Main\n\
             type Id = String\n",
        );

        assert_eq!(names(&p), ["Box", "Shape", "Main", "Id"]);
        let kinds: Vec<Kind> = p.symbols.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [Kind::Class, Kind::Trait, Kind::Class, Kind::TypeAlias]
        );
    }

    #[test]
    fn methods_are_qualified_by_their_container() {
        let p = parse("class Box {\n  def area: Int = 1\n}\n");

        assert_eq!(names(&p), ["Box", "Box::area"]);
        assert_eq!(p.symbols[1].kind, Kind::Method);
    }

    /// trait 裡沒有本體的方法也是符號。
    #[test]
    fn an_abstract_method_is_recorded() {
        let p = parse("trait Shape {\n  def area: Int\n}\n");

        assert_eq!(names(&p), ["Shape", "Shape::area"]);
    }

    #[test]
    fn object_members_hang_under_the_object() {
        let p = parse("object Main {\n  val Limit = 10\n  def run(): Unit = {}\n}\n");

        assert_eq!(names(&p), ["Main", "Main::Limit", "Main::run"]);
    }

    #[test]
    fn an_extended_type_is_a_type_reference() {
        let p = parse("class Box extends Base\n");

        assert_eq!(refs_by(&p, "Box", Rel::UsesType), ["Base"]);
    }

    #[test]
    fn constructor_parameters_are_type_references() {
        let p = parse("class Box(w: Widget)\n");

        assert_eq!(refs_by(&p, "Box", Rel::UsesType), ["Widget"]);
    }

    #[test]
    fn a_signature_records_the_types_it_mentions() {
        let p = parse("object M {\n  def make(w: Widget): Report = null\n}\n");

        assert_eq!(refs_by(&p, "M::make", Rel::UsesType), ["Widget", "Report"]);
    }

    #[test]
    fn calls_are_attributed_to_the_enclosing_declaration() {
        let p = parse("object M {\n  def run(): Unit = { helper(); box.area() }\n}\n");

        let calls = refs_by(&p, "M::run", Rel::Calls);
        assert!(calls.contains(&"helper".to_string()), "{calls:?}");
        assert!(calls.contains(&"box.area".to_string()), "{calls:?}");
    }

    /// 接收者大寫開頭是型別，改寫成限定名。
    #[test]
    fn a_call_on_a_type_becomes_a_qualified_name() {
        let p = parse("object M {\n  def run(): Int = Helper.compute()\n}\n");

        assert_eq!(refs_by(&p, "M::run", Rel::Calls), ["Helper::compute"]);
    }

    #[test]
    fn an_instance_expression_is_a_type_reference() {
        let p = parse("object M {\n  def run(): Unit = { new Widget() }\n}\n");

        // 回傳型別 `Unit` 也是一筆型別引用，這裡只關心建構的那一筆。
        let types = refs_by(&p, "M::run", Rel::UsesType);
        assert!(types.contains(&"Widget".to_string()), "{types:?}");
    }

    #[test]
    fn imports_bind_the_simple_name() {
        let p = parse("import com.example.util.Helper\nclass Box\n");

        assert_eq!(p.imports.len(), 1);
        assert_eq!(p.imports[0].local, "Helper");
    }

    /// 大括號選擇器與萬用字元都沒有指名單一目標。
    #[test]
    fn a_selector_or_wildcard_import_binds_nothing() {
        let braced = parse("import com.example.{A, B}\nclass Box\n");
        assert!(braced.imports.is_empty(), "{:?}", braced.imports);

        let wildcard = parse("import com.example._\nclass Box\n");
        assert!(wildcard.imports.is_empty(), "{:?}", wildcard.imports);
    }

    #[test]
    fn module_paths_follow_the_build_layout() {
        assert_eq!(
            ScalaExtractor.module_path("src/main/scala/com/example/app/Box.scala"),
            "com::example::app"
        );
    }

    #[test]
    fn a_syntax_error_still_yields_what_was_parsed() {
        let p = parse("class Ok\nclass Broken {\n  def f( : Int = \n");

        assert!(!p.errors.is_empty());
        assert!(names(&p).contains(&"Ok"), "{:?}", names(&p));
    }
}
