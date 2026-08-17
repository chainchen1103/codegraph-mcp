//! TypeScript 抽取器。
//!
//! 結構與 Rust 抽取器相同：明確的遞迴走訪，容器名稱沿祖先鏈累積。
//!
//! 限定名一律用 `::` 連接，即使 TypeScript 自己寫成 `Box.area`。分隔符
//! 是解析階段的內部約定：點號在那一層代表「對某個值呼叫方法」，限定名
//! 用點號會讓 `Box.area` 這種明確的宣告被誤判成接收者呼叫。查詢端接受
//! 兩種寫法。

use tree_sitter::{Language, Node};

use super::super::ts;
use super::super::{Extractor, FileParse};
use crate::extract::moniker;
use crate::model::{Kind, RawRef, RawSymbol, Rel};

/// TypeScript 的文件註解前綴。
const DOC_PREFIXES: &[&str] = &["//"];

/// 夾在文件註解與宣告之間、不打斷註解的節點。
const DOC_SKIP: &[&str] = &["decorator"];

pub struct TypeScriptExtractor;

impl Extractor for TypeScriptExtractor {
    fn language(&self) -> &'static str {
        "typescript"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["ts", "tsx", "mts", "cts"]
    }

    fn extract(&self, rel_path: &str, source: &str) -> FileParse {
        let language: Language = if rel_path.ends_with(".tsx") {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        };

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

        let path = moniker::normalize_path(rel_path);
        walk(root, source, &path, &[], &mut out);
        out
    }

    /// TypeScript 沒有由路徑決定的模組樹——import 寫的是相對路徑，不是
    /// 模組名。這一欄留空，跨檔的對應交給解析階段。
    fn module_path(&self, _rel_path: &str) -> String {
        String::new()
    }
}

/// 走訪一層節點。`container` 是祖先鏈上的名字。
fn walk(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        // `export` 只是修飾，真正的宣告在裡面。
        let declared = if child.kind() == "export_statement" {
            child.child_by_field_name("declaration")
        } else {
            Some(child)
        };
        let Some(child) = declared else {
            continue;
        };

        match child.kind() {
            "class_declaration" | "abstract_class_declaration" => {
                declare(child, source, path, container, Kind::Class, out);
            }
            "interface_declaration" => {
                declare(child, source, path, container, Kind::Interface, out);
            }
            "enum_declaration" => {
                declare(child, source, path, container, Kind::Enum, out);
            }
            "type_alias_declaration" => {
                leaf(child, source, path, container, Kind::TypeAlias, out);
            }
            "function_declaration" | "generator_function_declaration" => {
                function(child, source, path, container, Kind::Function, out);
            }
            "method_definition" | "method_signature" | "abstract_method_signature" => {
                function(child, source, path, container, Kind::Method, out);
            }
            // `const twice = (n) => ...` 是函數，`const LIMIT = 10` 不是。
            "lexical_declaration" | "variable_declaration" => {
                let mut inner = child.walk();
                for declarator in child.named_children(&mut inner) {
                    if declarator.kind() == "variable_declarator" {
                        variable(declarator, source, path, container, out);
                    }
                }
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
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    let moniker = push(node, source, path, container, kind, name, out);
    collect_types(node, source, &moniker, Body::Skip, out);

    let mut nested = container.to_vec();
    nested.push(name.to_string());
    if let Some(body) = node.child_by_field_name("body") {
        walk(body, source, path, &nested, out);
    }
}

/// 函數與方法：簽名記型別，本體記呼叫。
fn function(
    node: Node<'_>,
    source: &str,
    path: &str,
    container: &[String],
    kind: Kind,
    out: &mut FileParse,
) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    let moniker = push(node, source, path, container, kind, name, out);
    collect_types(node, source, &moniker, Body::Skip, out);

    if let Some(body) = node.child_by_field_name("body") {
        collect_calls(body, source, &moniker, out);
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
    if let Some(name) = field_text(node, "name", source) {
        let moniker = push(node, source, path, container, kind, name, out);
        collect_types(node, source, &moniker, Body::Include, out);
    }
}

/// `const x = ...`：值是函數就記成函數，否則是常數。
fn variable(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    let value = node.child_by_field_name("value");
    let is_function = value.is_some_and(|v| {
        matches!(
            v.kind(),
            "arrow_function" | "function_expression" | "function"
        )
    });

    let kind = if is_function {
        Kind::Function
    } else {
        Kind::Const
    };
    let moniker = push(node, source, path, container, kind, name, out);
    collect_types(node, source, &moniker, Body::Skip, out);

    if let Some(value) = value {
        collect_calls(value, source, &moniker, out);
    }
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

    // `new Box()` 也是依賴，記成型別引用而不是呼叫——目標是型別。
    if node.kind() == "new_expression"
        && let Some(constructor) = node.child_by_field_name("constructor")
        && constructor.kind() == "identifier"
    {
        out.refs.push(RawRef {
            from: from.to_string(),
            name: ts::text(constructor, source).to_string(),
            rel: Rel::UsesType,
            line: ts::line_of(node),
        });
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
        // obj.method() —— 接收者的型別未知，保留原文讓解析階段判斷。
        "member_expression" => {
            let object = function.child_by_field_name("object")?;
            let property = function.child_by_field_name("property")?;
            let receiver = ts::collapse_whitespace(ts::text(object, source));
            Some(format!("{receiver}.{}", ts::text(property, source)))
        }
        _ => None,
    }
}

/// 找型別時要不要連宣告的本體一起看。
#[derive(Copy, Clone, PartialEq, Eq)]
enum Body {
    Include,
    Skip,
}

/// 記下宣告用到的型別。
fn collect_types(node: Node<'_>, source: &str, from: &str, body: Body, out: &mut FileParse) {
    let declared = declared_type_parameters(node, source);
    let skip_body = (body == Body::Skip)
        .then(|| node.child_by_field_name("body"))
        .flatten();
    let own_name = node.child_by_field_name("name");

    let mut found: Vec<(String, u32)> = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if Some(child) == skip_body || Some(child) == own_name {
            continue;
        }
        gather_types(child, source, &declared, &mut found);
    }

    for (name, line) in found {
        out.refs.push(RawRef {
            from: from.to_string(),
            name,
            rel: Rel::UsesType,
            line,
        });
    }
}

/// 這個宣告自己引入的泛型參數名。
fn declared_type_parameters(node: Node<'_>, source: &str) -> Vec<String> {
    let mut declared = Vec::new();
    let mut top = node.walk();
    let Some(parameters) = node
        .named_children(&mut top)
        .find(|c| c.kind() == "type_parameters")
    else {
        return declared;
    };

    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if let Some(named) = parameter.child_by_field_name("name")
            && named.kind() == "type_identifier"
        {
            declared.push(ts::text(named, source).to_string());
        }
    }
    declared
}

/// 收集節點底下出現的型別名。`number` 這類內建型別是 `predefined_type`，
/// 不是 `type_identifier`，自然不會被收進來。
fn gather_types(node: Node<'_>, source: &str, declared: &[String], found: &mut Vec<(String, u32)>) {
    if node.kind() == "type_identifier" {
        let name = ts::text(node, source);
        if !declared.iter().any(|d| d == name) && !found.iter().any(|(n, _)| n == name) {
            found.push((name.to_string(), ts::line_of(node)));
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        gather_types(child, source, declared, found);
    }
}

/// 收下一個符號，回傳它的 moniker。
fn push(
    node: Node<'_>,
    source: &str,
    path: &str,
    container: &[String],
    kind: Kind,
    name: &str,
    out: &mut FileParse,
) -> String {
    let start_line = ts::line_of(node);
    let qualified = if container.is_empty() {
        name.to_string()
    } else {
        format!("{}::{name}", container.join("::"))
    };
    let moniker = moniker::build(path, kind, name, start_line);

    out.symbols.push(RawSymbol {
        moniker: moniker.clone(),
        name: name.to_string(),
        qualified,
        kind,
        start_line,
        end_line: ts::end_line_of(node),
        signature: signature(node, source),
        docstring: ts::leading_line_comments(node, source, "comment", DOC_PREFIXES, DOC_SKIP),
    });

    moniker
}

/// 宣告的簽名，也就是本體之前的部分。
fn signature(node: Node<'_>, source: &str) -> Option<String> {
    let full = ts::text(node, source);
    let cut = node
        .child_by_field_name("body")
        .or_else(|| node.child_by_field_name("value"))
        .map(|b| b.start_byte() - node.start_byte())
        .unwrap_or(full.len());
    let decl = full
        .get(..cut)?
        .trim_end()
        .trim_end_matches(&['=', ':'][..]);
    let s = ts::collapse_whitespace(decl);
    if s.is_empty() { None } else { Some(s) }
}

fn field_text<'a>(node: Node<'_>, field: &str, source: &'a str) -> Option<&'a str> {
    node.child_by_field_name(field).map(|n| ts::text(n, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> FileParse {
        TypeScriptExtractor.extract("src/a.ts", src)
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
            "export function make(): void {}\n\
             export class Box {}\n\
             export interface Shape {}\n\
             export type Id = string;\n\
             enum Colour { Red }\n",
        );

        assert_eq!(names(&p), ["make", "Box", "Shape", "Id", "Colour"]);
        let kinds: Vec<Kind> = p.symbols.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Function,
                Kind::Class,
                Kind::Interface,
                Kind::TypeAlias,
                Kind::Enum
            ]
        );
    }

    /// `export` 只是修飾，不該擋住裡面的宣告。
    #[test]
    fn exported_and_bare_declarations_are_treated_alike() {
        let exported = parse("export function make(): void {}\n");
        let bare = parse("function make(): void {}\n");

        assert_eq!(names(&exported), names(&bare));
    }

    #[test]
    fn methods_are_qualified_by_their_class() {
        let p = parse("class Box {\n  area(): number { return 1; }\n}\n");

        assert_eq!(names(&p), ["Box", "Box::area"]);
        assert_eq!(p.symbols[1].kind, Kind::Method);
    }

    #[test]
    fn interface_members_are_recorded_as_methods() {
        let p = parse("interface Shape {\n  area(): number;\n}\n");

        assert_eq!(names(&p), ["Shape", "Shape::area"]);
    }

    /// 箭頭函數是函數，其他 const 是常數。
    #[test]
    fn an_arrow_binding_is_a_function_and_a_plain_one_is_not() {
        let p = parse("const twice = (n: number) => n * 2;\nconst LIMIT = 10;\n");

        assert_eq!(p.symbols[0].kind, Kind::Function);
        assert_eq!(p.symbols[1].kind, Kind::Const);
    }

    #[test]
    fn calls_are_attributed_to_the_enclosing_declaration() {
        let p = parse("function outer(): void {\n  helper();\n  obj.method();\n}\n");

        assert_eq!(refs_by(&p, "outer", Rel::Calls), ["helper", "obj.method"]);
    }

    #[test]
    fn calls_inside_an_arrow_binding_belong_to_it() {
        let p = parse("const run = () => helper();\n");

        assert_eq!(refs_by(&p, "run", Rel::Calls), ["helper"]);
    }

    /// 內建型別不是符號，記了只會變成永遠解析不了的雜訊。
    #[test]
    fn only_named_types_are_recorded() {
        let p = parse("function make(w: Widget, n: number): Report {\n  return null;\n}\n");

        assert_eq!(refs_by(&p, "make", Rel::UsesType), ["Widget", "Report"]);
    }

    #[test]
    fn implemented_interfaces_are_recorded_as_type_references() {
        let p = parse("class Box implements Shape {\n  area(): number { return 1; }\n}\n");

        assert_eq!(refs_by(&p, "Box", Rel::UsesType), ["Shape"]);
    }

    /// `new Box()` 的目標是型別，不是函數。
    #[test]
    fn a_constructor_call_is_a_type_reference() {
        let p = parse("function make(): void {\n  const b = new Box();\n}\n");

        assert_eq!(refs_by(&p, "make", Rel::UsesType), ["Box"]);
        assert!(refs_by(&p, "make", Rel::Calls).is_empty());
    }

    #[test]
    fn generic_parameters_are_skipped_but_their_arguments_are_not() {
        let p = parse("function wrap<T>(item: T): Box<T> {\n  return null;\n}\n");

        assert_eq!(refs_by(&p, "wrap", Rel::UsesType), ["Box"]);
    }

    #[test]
    fn tsx_files_parse_as_tsx() {
        let p = TypeScriptExtractor.extract(
            "src/a.tsx",
            "export function View() {\n  return <div />;\n}\n",
        );

        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert_eq!(names(&p), ["View"]);
    }

    #[test]
    fn typescript_has_no_module_path() {
        assert_eq!(TypeScriptExtractor.module_path("src/a/b.ts"), "");
    }

    #[test]
    fn a_syntax_error_still_yields_what_was_parsed() {
        let p = parse("function ok(): void {}\nfunction broken(: {\n");

        assert!(!p.errors.is_empty());
        assert!(names(&p).contains(&"ok"), "{:?}", names(&p));
    }
}
