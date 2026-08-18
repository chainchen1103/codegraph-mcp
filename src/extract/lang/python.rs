//! Python 抽取器。
//!
//! 結構與另外兩個抽取器相同：明確的遞迴走訪，容器名稱沿祖先鏈累積，
//! 限定名一律用 `::` 連接（見 typescript.rs 的說明）。
//!
//! 文件字串不是註解而是本體裡的第一個字串運算式，因此不能沿用共用的
//! 註解抽取，得另外處理。

use tree_sitter::{Language, Node};

use super::super::ts;
use super::super::{Extractor, FileParse, Import, ImportTarget};
use crate::extract::moniker;
use crate::model::{Kind, RawRef, RawSymbol, Rel};

pub struct PythonExtractor;

impl Extractor for PythonExtractor {
    fn language(&self) -> &'static str {
        "python"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["py", "pyi"]
    }

    fn extract(&self, rel_path: &str, source: &str) -> FileParse {
        let language: Language = tree_sitter_python::LANGUAGE.into();
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

    fn directory_modules(&self) -> &'static [&'static str] {
        &["__init__.py", "__init__.pyi"]
    }

    /// 套件路徑，`__init__.py` 代表所在的套件本身。
    ///
    /// 與 Rust 的 `mod.rs` 是同一個概念。連接符用 `::` 而不是 Python 自
    /// 己的點號：點號在解析階段代表接收者呼叫。
    fn module_path(&self, rel_path: &str) -> String {
        let normalized = rel_path.replace('\\', "/");
        let mut segments: Vec<&str> = normalized.split('/').collect();

        let Some(stem) = segments
            .pop()
            .and_then(|f| f.strip_suffix(".py").or_else(|| f.strip_suffix(".pyi")))
        else {
            return String::new();
        };

        if stem != "__init__" {
            segments.push(stem);
        }
        segments.join("::")
    }
}

/// 走訪一層節點。`container` 是祖先鏈上的名字。
fn walk(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        // 裝飾器只是包裝，真正的宣告在裡面。
        let declared = if child.kind() == "decorated_definition" {
            child.child_by_field_name("definition")
        } else {
            Some(child)
        };
        let Some(child) = declared else {
            continue;
        };

        match child.kind() {
            "class_definition" => class(child, source, path, container, out),
            "function_definition" => function(child, source, path, container, out),
            "import_statement" | "import_from_statement" => collect_import(child, source, out),
            // 模組層與類別層的賦值是常數與欄位，本體裡的區域變數不算。
            "expression_statement" => assignment(child, source, path, container, out),
            _ => {}
        }
    }
}

/// 走一條 import，把它引入的每個名字記下來。
///
/// `import a.b` 引入的名字是 `a.b` 本身——之後會寫成 `a.b.thing()`。
/// `from a.b import thing` 引入的是 `thing`，目標是 `a.b` 那個檔案。
fn collect_import(node: Node<'_>, source: &str, out: &mut FileParse) {
    let line = ts::line_of(node);

    if node.kind() == "import_statement" {
        let mut cursor = node.walk();
        for item in node.named_children(&mut cursor) {
            let (path_node, local) = match item.kind() {
                "dotted_name" => (item, ts::text(item, source).to_string()),
                "aliased_import" => {
                    let (Some(name), Some(alias)) = (
                        item.child_by_field_name("name"),
                        item.child_by_field_name("alias"),
                    ) else {
                        continue;
                    };
                    (name, ts::text(alias, source).to_string())
                }
                _ => continue,
            };
            let segments = dotted_segments(path_node, source);
            out.imports.push(Import {
                local,
                target: ImportTarget::Rooted(segments),
                line,
            });
        }
        return;
    }

    // from <module> import <names>
    let Some(module) = node.child_by_field_name("module_name") else {
        return;
    };
    let target = module_target(module, source);

    let mut cursor = node.walk();
    for item in node.named_children(&mut cursor) {
        if item.id() == module.id() {
            continue;
        }
        let local = match item.kind() {
            "dotted_name" => ts::text(item, source).to_string(),
            "aliased_import" => match item.child_by_field_name("alias") {
                Some(alias) => ts::text(alias, source).to_string(),
                None => continue,
            },
            // `from a import *` 沒有引入具體的名字。
            _ => continue,
        };
        out.imports.push(Import {
            local,
            target: target.clone(),
            line,
        });
    }
}

/// `from` 後面那一段指向哪裡。
///
/// 前導的點是套件層級：一個點是所在套件，兩個點是上一層。`from . import
/// x` 的目標就是這個檔案所在的目錄。
fn module_target(node: Node<'_>, source: &str) -> ImportTarget {
    if node.kind() != "relative_import" {
        return ImportTarget::Rooted(dotted_segments(node, source));
    }

    let text = ts::text(node, source);
    let dots = text.len() - text.trim_start_matches('.').len();
    // 一個點是當前目錄，之後每多一個點往上一層。
    let up = "../".repeat(dots.saturating_sub(1));
    let rest = text.trim_start_matches('.').replace('.', "/");

    ImportTarget::Relative(format!("./{up}{rest}"))
}

/// `a.b.c` 攤平成三段。
fn dotted_segments(node: Node<'_>, source: &str) -> Vec<String> {
    ts::text(node, source)
        .split('.')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn class(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    let moniker = push(node, source, path, container, Kind::Class, name, out);

    // 基底類別寫在 superclasses 裡，那是這個類別最實在的依賴。
    if let Some(bases) = node.child_by_field_name("superclasses") {
        let mut found = Vec::new();
        gather_types(bases, source, &mut found);
        emit_types(&moniker, found, out);
    }

    let mut nested = container.to_vec();
    nested.push(name.to_string());
    if let Some(body) = node.child_by_field_name("body") {
        walk(body, source, path, &nested, out);
    }
}

fn function(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    // 類別底下的是方法，模組層的是函數。
    let kind = if container.is_empty() {
        Kind::Function
    } else {
        Kind::Method
    };
    let moniker = push(node, source, path, container, kind, name, out);

    // 型別只看標註：參數與回傳。本體裡的是實作細節。
    let mut found = Vec::new();
    for field in ["parameters", "return_type"] {
        if let Some(part) = node.child_by_field_name(field) {
            gather_types(part, source, &mut found);
        }
    }
    emit_types(&moniker, found, out);

    if let Some(body) = node.child_by_field_name("body") {
        collect_calls(body, source, &moniker, out);
        // 巢狀函數的呼叫已經算在外層頭上，但巢狀類別仍是獨立的符號。
        let mut nested = container.to_vec();
        nested.push(name.to_string());
        walk_nested_classes(body, source, path, &nested, out);
    }
}

/// 函數本體裡的類別定義仍要收，本體裡的區域賦值不收。
fn walk_nested_classes(
    node: Node<'_>,
    source: &str,
    path: &str,
    container: &[String],
    out: &mut FileParse,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "class_definition" {
            class(child, source, path, container, out);
        }
    }
}

/// `NAME = value` 或 `name: Type = value`。
fn assignment(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "assignment" {
            continue;
        }
        let Some(left) = child.child_by_field_name("left") else {
            continue;
        };
        if left.kind() != "identifier" {
            continue;
        }

        let name = ts::text(left, source);
        let moniker = push(child, source, path, container, Kind::Const, name, out);

        if let Some(annotation) = child.child_by_field_name("type") {
            let mut found = Vec::new();
            gather_types(annotation, source, &mut found);
            emit_types(&moniker, found, out);
        }
        if let Some(value) = child.child_by_field_name("right") {
            collect_calls(value, source, &moniker, out);
        }
    }
}

/// 走遍節點底下所有的呼叫，記到 `from` 名下。
fn collect_calls(node: Node<'_>, source: &str, from: &str, out: &mut FileParse) {
    if node.kind() == "call"
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

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, source, from, out);
    }
}

/// 被呼叫者在原始碼裡的寫法。
///
/// Python 沒有 `new`：`Box()` 與 `helper()` 長得一樣，是不是建構函數要
/// 到解析階段比對候選的種類才知道。
fn callee_name(function: Node<'_>, source: &str) -> Option<String> {
    match function.kind() {
        "identifier" => Some(ts::text(function, source).to_string()),
        // obj.method() —— 接收者的型別未知，保留原文。
        "attribute" => {
            let object = function.child_by_field_name("object")?;
            let attribute = function.child_by_field_name("attribute")?;
            let receiver = ts::collapse_whitespace(ts::text(object, source));
            Some(format!("{receiver}.{}", ts::text(attribute, source)))
        }
        _ => None,
    }
}

/// 收集節點底下出現的型別名。
///
/// Python 的型別標註就是一般的運算式，因此只收 `identifier`；`int` 這
/// 類內建型別解析階段查不到，會被判為外部而丟棄。
fn gather_types(node: Node<'_>, source: &str, found: &mut Vec<(String, u32)>) {
    if node.kind() == "identifier" {
        let name = ts::text(node, source);
        if !found.iter().any(|(n, _)| n == name) {
            found.push((name.to_string(), ts::line_of(node)));
        }
        return;
    }
    // 字串形式的前向參考（`def f() -> "Box"`）不解析，那需要求值。
    if node.kind() == "string" {
        return;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        gather_types(child, source, found);
    }
}

fn emit_types(from: &str, found: Vec<(String, u32)>, out: &mut FileParse) {
    for (name, line) in found {
        out.refs.push(RawRef {
            from: from.to_string(),
            name,
            rel: Rel::UsesType,
            line,
        });
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
        docstring: docstring(node, source),
    });

    moniker
}

/// 宣告的簽名，也就是本體之前的部分。
fn signature(node: Node<'_>, source: &str) -> Option<String> {
    let full = ts::text(node, source);
    let cut = node
        .child_by_field_name("body")
        .map(|b| b.start_byte() - node.start_byte())
        .unwrap_or(full.len());
    let decl = full.get(..cut)?.trim_end().trim_end_matches(':');
    let s = ts::collapse_whitespace(decl);
    if s.is_empty() { None } else { Some(s) }
}

/// 本體裡的第一個字串運算式。
fn docstring(node: Node<'_>, source: &str) -> Option<String> {
    let body = node.child_by_field_name("body")?;
    let first = body.named_child(0)?;
    if first.kind() != "expression_statement" {
        return None;
    }
    let literal = first.named_child(0)?;
    if literal.kind() != "string" {
        return None;
    }

    let text = ts::text(literal, source);
    let trimmed = text
        .trim_start_matches(['r', 'b', 'f', 'u', 'R', 'B', 'F', 'U'])
        .trim_matches('"')
        .trim_matches('\'')
        .trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn field_text<'a>(node: Node<'_>, field: &str, source: &'a str) -> Option<&'a str> {
    node.child_by_field_name(field).map(|n| ts::text(n, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> FileParse {
        PythonExtractor.extract("src/a.py", src)
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

    fn imports_of(p: &FileParse) -> Vec<(String, ImportTarget)> {
        p.imports
            .iter()
            .map(|i| (i.local.clone(), i.target.clone()))
            .collect()
    }

    fn rooted(segments: &[&str]) -> ImportTarget {
        ImportTarget::Rooted(segments.iter().map(|s| s.to_string()).collect())
    }

    /// `import a.b` 引入的名字就是 `a.b`，之後會寫成 `a.b.thing()`。
    #[test]
    fn a_plain_import_binds_the_dotted_name() {
        let p = parse(
            "import a.b
",
        );

        assert_eq!(imports_of(&p), [("a.b".to_string(), rooted(&["a", "b"]))]);
    }

    #[test]
    fn an_aliased_import_binds_the_alias() {
        let p = parse(
            "import a.b as ab
",
        );

        assert_eq!(imports_of(&p), [("ab".to_string(), rooted(&["a", "b"]))]);
    }

    #[test]
    fn a_from_import_binds_each_name_to_the_module() {
        let p = parse(
            "from a.b import thing, other as alias
",
        );

        assert_eq!(
            imports_of(&p),
            [
                ("thing".to_string(), rooted(&["a", "b"])),
                ("alias".to_string(), rooted(&["a", "b"])),
            ]
        );
    }

    /// 一個點是所在套件，多一個點往上一層。
    #[test]
    fn relative_imports_count_their_leading_dots() {
        let one = parse(
            "from . import sibling
",
        );
        assert_eq!(
            imports_of(&one),
            [(
                "sibling".to_string(),
                ImportTarget::Relative("./".to_string())
            )]
        );

        let two = parse(
            "from ..pkg import deep
",
        );
        assert_eq!(
            imports_of(&two),
            [(
                "deep".to_string(),
                ImportTarget::Relative("./../pkg".to_string())
            )]
        );
    }

    /// `from a import *` 沒有指名任何東西。
    #[test]
    fn a_star_import_binds_nothing() {
        let p = parse(
            "from a import *
",
        );

        assert!(p.imports.is_empty(), "{:?}", p.imports);
    }

    #[test]
    fn top_level_declarations_are_extracted() {
        let p = parse("LIMIT = 10\n\nclass Box:\n    pass\n\ndef make():\n    pass\n");

        assert_eq!(names(&p), ["LIMIT", "Box", "make"]);
        let kinds: Vec<Kind> = p.symbols.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [Kind::Const, Kind::Class, Kind::Function]);
    }

    #[test]
    fn methods_are_qualified_by_their_class() {
        let p = parse("class Box:\n    def area(self):\n        return 1\n");

        assert_eq!(names(&p), ["Box", "Box::area"]);
        assert_eq!(p.symbols[1].kind, Kind::Method);
    }

    /// 裝飾器只是包裝，不該擋住裡面的宣告。
    #[test]
    fn a_decorated_definition_is_still_extracted() {
        let p = parse("@cache\ndef make():\n    pass\n");

        assert_eq!(names(&p), ["make"]);
    }

    #[test]
    fn base_classes_are_recorded_as_type_references() {
        let p = parse("class Box(Base, Mixin):\n    pass\n");

        assert_eq!(refs_by(&p, "Box", Rel::UsesType), ["Base", "Mixin"]);
    }

    #[test]
    fn annotations_are_recorded_as_type_references() {
        let p = parse("def make(w: Widget, n: int) -> Report:\n    return None\n");

        let types = refs_by(&p, "make", Rel::UsesType);
        assert!(types.contains(&"Widget".to_string()), "{types:?}");
        assert!(types.contains(&"Report".to_string()), "{types:?}");
    }

    /// 本體裡的型別是實作細節，不算對外依賴。
    #[test]
    fn types_inside_a_body_are_not_counted() {
        let p = parse("def run():\n    x: Local = Local()\n    return x\n");

        assert!(refs_by(&p, "run", Rel::UsesType).is_empty());
    }

    #[test]
    fn calls_are_attributed_to_the_enclosing_declaration() {
        let p = parse("def outer():\n    helper()\n    obj.method()\n");

        assert_eq!(refs_by(&p, "outer", Rel::Calls), ["helper", "obj.method"]);
    }

    #[test]
    fn a_docstring_is_taken_from_the_first_string_in_the_body() {
        let p = parse("def make():\n    \"\"\"造一個盒子。\"\"\"\n    return 1\n");

        assert_eq!(p.symbols[0].docstring.as_deref(), Some("造一個盒子。"));
    }

    #[test]
    fn a_body_without_a_docstring_has_none() {
        let p = parse("def make():\n    return 1\n");

        assert!(p.symbols[0].docstring.is_none());
    }

    /// 字串形式的前向參考不當成型別，那需要求值才知道指向誰。
    #[test]
    fn a_string_annotation_is_not_a_type_reference() {
        let p = parse("def make() -> \"Box\":\n    return None\n");

        assert!(refs_by(&p, "make", Rel::UsesType).is_empty());
    }

    #[test]
    fn module_paths_follow_the_package_layout() {
        assert_eq!(PythonExtractor.module_path("a/b/thing.py"), "a::b::thing");
        assert_eq!(PythonExtractor.module_path("a/b/__init__.py"), "a::b");
        assert_eq!(PythonExtractor.module_path("thing.py"), "thing");
        assert_eq!(PythonExtractor.module_path("README.md"), "");
    }

    #[test]
    fn module_paths_do_not_depend_on_the_path_separator() {
        assert_eq!(
            PythonExtractor.module_path(r"a\b\thing.py"),
            PythonExtractor.module_path("a/b/thing.py")
        );
    }

    /// 函數本體裡的類別仍是符號，本體裡的區域賦值不是。
    #[test]
    fn a_class_inside_a_function_is_still_a_symbol() {
        let p = parse(
            "def make():
    tmp = 1

    class Inner:
        pass
    return Inner
",
        );

        assert_eq!(names(&p), ["make", "make::Inner"]);
    }

    /// 類別層的欄位標註是這個類別的依賴。
    #[test]
    fn a_class_level_annotation_is_a_type_reference() {
        let p = parse(
            "class Box:
    inner: Widget = None
",
        );

        assert_eq!(refs_by(&p, "Box::inner", Rel::UsesType), ["Widget"]);
    }

    /// 賦值右邊的呼叫算在那個常數頭上。
    #[test]
    fn a_call_in_an_assignment_belongs_to_the_binding() {
        let p = parse(
            "REGISTRY = build_registry()
",
        );

        assert_eq!(refs_by(&p, "REGISTRY", Rel::Calls), ["build_registry"]);
    }

    #[test]
    fn stub_files_are_extracted_too() {
        let p = PythonExtractor.extract(
            "src/a.pyi",
            "def make() -> Box: ...
",
        );

        assert_eq!(names(&p), ["make"]);
        assert_eq!(PythonExtractor.module_path("src/a.pyi"), "src::a");
    }

    #[test]
    fn a_syntax_error_still_yields_what_was_parsed() {
        let p = parse("def ok():\n    pass\n\ndef broken(\n");

        assert!(!p.errors.is_empty());
        assert!(names(&p).contains(&"ok"), "{:?}", names(&p));
    }
}
