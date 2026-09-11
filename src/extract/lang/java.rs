//! Java 抽取器。
//!
//! 呼叫在語法樹上是 `method_invocation`，接收者放在 `object` 欄位裡。
//! `Helper.compute()` 與 `box.area()` 的節點形狀完全一樣，差別只在接收者
//! 是型別還是值——靠命名慣例分辨（見 [`super::jvm::looks_like_type`]），
//! 是型別就改寫成限定名 `Helper::compute`，讓解析階段接得上 import。

use tree_sitter::{Language, Node};

use super::super::ts;
use super::super::{Extractor, FileParse, Import, ImportTarget};
use super::common::{self, Declaration, TypeShapes};
use super::jvm;
use crate::extract::moniker;
use crate::model::{Kind, RawRef, Rel};

/// Java 的文件註解。`/** ... */` 是 block_comment。
const DOC_PREFIXES: &[&str] = &["/**", "//"];

/// 夾在文件註解與宣告之間、不打斷註解的節點。
const DOC_SKIP: &[&str] = &["modifiers", "marker_annotation", "annotation"];

/// 型別名在 Java 的語法樹裡長什麼樣。
///
/// `int` 這類原生型別是 `integral_type` / `floating_point_type`，不是
/// `type_identifier`，自然不會被收進來。
const TYPES: TypeShapes = TypeShapes {
    leaves: &["type_identifier"],
    scoped: &["scoped_type_identifier"],
    opaque: &[],
};

const SUFFIXES: &[&str] = &[".java"];

pub struct JavaExtractor;

impl Extractor for JavaExtractor {
    fn language(&self) -> &'static str {
        "java"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["java"]
    }

    fn extract(&self, rel_path: &str, source: &str) -> FileParse {
        let language: Language = tree_sitter_java::LANGUAGE.into();
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

/// `import a.b.C;` 引入的名字是 `C`，目標是 `a/b/C.java`。
fn collect_imports(root: Node<'_>, source: &str, out: &mut FileParse) {
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() != "import_declaration" {
            continue;
        }
        let text = ts::text(child, source)
            .trim_start_matches("import")
            .trim()
            .trim_start_matches("static")
            .trim()
            .trim_end_matches(';')
            .trim();

        let segments: Vec<String> = text
            .split('.')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        // `import a.b.*;` 沒有指名任何東西。
        let Some(last) = segments.last().cloned() else {
            continue;
        };
        if last == "*" {
            continue;
        }

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
            "class_declaration" => declare(child, source, path, container, Kind::Class, out),
            "record_declaration" => declare(child, source, path, container, Kind::Struct, out),
            "interface_declaration" => {
                declare(child, source, path, container, Kind::Interface, out);
            }
            "enum_declaration" => declare(child, source, path, container, Kind::Enum, out),
            "annotation_type_declaration" => {
                declare(child, source, path, container, Kind::Interface, out);
            }
            "method_declaration" | "constructor_declaration" => {
                function(child, source, path, container, out);
            }
            "field_declaration" | "constant_declaration" => {
                field(child, source, path, container, out);
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

    // 繼承與實作的型別就是這個宣告最實在的依賴。
    let mut found = Vec::new();
    for field in ["superclass", "interfaces", "type_parameters", "permits"] {
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

/// 方法與建構子：簽名記型別，本體記呼叫。
fn function(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let Some(name) = common::field_text(node, "name", source) else {
        return;
    };
    let moniker = push(node, source, path, container, Kind::Method, name, out);

    let mut found = Vec::new();
    for field in ["type", "parameters", "throws"] {
        if let Some(part) = node.child_by_field_name(field) {
            common::gather_types(part, source, TYPES, &[], &mut found);
        }
    }
    common::emit_types(&moniker, found, out);

    if let Some(body) = node.child_by_field_name("body") {
        collect_calls(body, source, &moniker, out);
    }
}

/// 欄位：型別是所屬類別的依賴，初始式裡的呼叫算在欄位頭上。
fn field(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let Some(declarator) = node.child_by_field_name("declarator") else {
        return;
    };
    let Some(name) = common::field_text(declarator, "name", source) else {
        return;
    };
    let moniker = push(node, source, path, container, Kind::Const, name, out);

    if let Some(annotation) = node.child_by_field_name("type") {
        let mut found = Vec::new();
        common::gather_types(annotation, source, TYPES, &[], &mut found);
        common::emit_types(&moniker, found, out);
    }
    if let Some(value) = declarator.child_by_field_name("value") {
        collect_calls(value, source, &moniker, out);
    }
}

/// 收下一個符號，補上 Java 特有的簽名與註解取法。
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
            signature: common::signature(node, source, &["body"], &[';', '=']),
            has_body: common::has_body(node, &["body"]),
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
    if node.kind() == "method_invocation"
        && let Some(name) = callee_name(node, source)
    {
        out.refs.push(RawRef {
            from: from.to_string(),
            name,
            rel: Rel::Calls,
            line: ts::line_of(node),
        });
    }

    // `new Box()` 的目標是型別，不是函數。
    if node.kind() == "object_creation_expression"
        && let Some(created) = node.child_by_field_name("type")
    {
        let mut found = Vec::new();
        common::gather_types(created, source, TYPES, &[], &mut found);
        common::emit_types(from, found, out);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, source, from, out);
    }
}

/// 被呼叫者在原始碼裡的寫法。
///
/// 接收者是型別時改寫成限定名——那是解析階段認得的形式，而且 import 表
/// 正好指名了那個型別在哪個檔案。是值就保留點號。
fn callee_name(node: Node<'_>, source: &str) -> Option<String> {
    let name = ts::text(node.child_by_field_name("name")?, source);

    let Some(object) = node.child_by_field_name("object") else {
        return Some(name.to_string());
    };
    let receiver = ts::collapse_whitespace(ts::text(object, source));

    if object.kind() == "identifier" && jvm::looks_like_type(&receiver) {
        return Some(format!("{receiver}::{name}"));
    }
    Some(format!("{receiver}.{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> FileParse {
        JavaExtractor.extract("src/main/java/com/example/A.java", src)
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
            "public class Box {}\n\
             interface Shape {}\n\
             enum Colour { RED }\n\
             record Point(int x) {}\n",
        );

        assert_eq!(names(&p), ["Box", "Shape", "Colour", "Point"]);
        let kinds: Vec<Kind> = p.symbols.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [Kind::Class, Kind::Interface, Kind::Enum, Kind::Struct]
        );
    }

    #[test]
    fn methods_are_qualified_by_their_class() {
        let p = parse("class Box {\n    public int area() { return 1; }\n}\n");

        assert_eq!(names(&p), ["Box", "Box::area"]);
        assert_eq!(p.symbols[1].kind, Kind::Method);
    }

    #[test]
    fn interface_methods_are_recorded_too() {
        let p = parse("interface Shape {\n    int area();\n}\n");

        assert_eq!(names(&p), ["Shape", "Shape::area"]);
    }

    #[test]
    fn a_nested_class_carries_its_outer_name() {
        let p = parse("class Outer {\n    static class Inner {\n        void run() {}\n    }\n}\n");

        assert_eq!(names(&p), ["Outer", "Outer::Inner", "Outer::Inner::run"]);
    }

    #[test]
    fn implemented_interfaces_are_type_references() {
        let p = parse("class Box implements Shape, Sized {}\n");

        assert_eq!(refs_by(&p, "Box", Rel::UsesType), ["Shape", "Sized"]);
    }

    #[test]
    fn a_superclass_is_a_type_reference() {
        let p = parse("class Box extends Base {}\n");

        assert_eq!(refs_by(&p, "Box", Rel::UsesType), ["Base"]);
    }

    /// 原生型別不是符號，記了只會變成永遠解析不了的雜訊。
    #[test]
    fn only_named_types_are_recorded() {
        let p = parse("class Box {\n    Report make(Widget w, int n) { return null; }\n}\n");

        assert_eq!(
            refs_by(&p, "Box::make", Rel::UsesType),
            ["Report", "Widget"]
        );
    }

    #[test]
    fn fields_are_recorded_with_their_types() {
        let p = parse("class Box {\n    private Widget inner;\n}\n");

        assert_eq!(names(&p), ["Box", "Box::inner"]);
        assert_eq!(refs_by(&p, "Box::inner", Rel::UsesType), ["Widget"]);
    }

    /// 接收者大寫開頭是型別，改寫成限定名讓解析階段接得上 import。
    #[test]
    fn a_static_call_becomes_a_qualified_name() {
        let p = parse("class Box {\n    int run() { return Helper.compute(); }\n}\n");

        assert_eq!(refs_by(&p, "Box::run", Rel::Calls), ["Helper::compute"]);
    }

    /// 接收者小寫開頭是值，點號要保留。
    #[test]
    fn an_instance_call_keeps_its_dot() {
        let p = parse("class Box {\n    int run() { return helper.compute(); }\n}\n");

        assert_eq!(refs_by(&p, "Box::run", Rel::Calls), ["helper.compute"]);
    }

    #[test]
    fn a_bare_call_has_no_receiver() {
        let p = parse("class Box {\n    int run() { return compute(); }\n}\n");

        assert_eq!(refs_by(&p, "Box::run", Rel::Calls), ["compute"]);
    }

    #[test]
    fn a_constructor_call_is_a_type_reference() {
        let p = parse("class Box {\n    void run() { new Widget(); }\n}\n");

        assert_eq!(refs_by(&p, "Box::run", Rel::UsesType), ["Widget"]);
    }

    #[test]
    fn imports_bind_the_simple_name() {
        let p = parse("import com.example.util.Helper;\nclass Box {}\n");

        assert_eq!(p.imports.len(), 1);
        assert_eq!(p.imports[0].local, "Helper");
        assert_eq!(
            p.imports[0].target,
            ImportTarget::Rooted(
                ["com", "example", "util", "Helper"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            )
        );
    }

    #[test]
    fn a_static_import_binds_the_member_name() {
        let p = parse("import static com.example.U.f;\nclass Box {}\n");

        assert_eq!(p.imports[0].local, "f");
    }

    /// 萬用 import 沒有指名任何東西。
    #[test]
    fn a_wildcard_import_binds_nothing() {
        let p = parse("import com.example.util.*;\nclass Box {}\n");

        assert!(p.imports.is_empty(), "{:?}", p.imports);
    }

    #[test]
    fn module_paths_follow_the_build_layout() {
        assert_eq!(
            JavaExtractor.module_path("src/main/java/com/example/app/Box.java"),
            "com::example::app"
        );
    }

    #[test]
    fn a_syntax_error_still_yields_what_was_parsed() {
        let p = parse("class Ok {}\nclass Broken {\n    void f( {\n");

        assert!(!p.errors.is_empty());
        assert!(names(&p).contains(&"Ok"), "{:?}", names(&p));
    }
}
