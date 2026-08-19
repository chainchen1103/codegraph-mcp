//! Go 抽取器。
//!
//! Go 的套件是**目錄**而不是檔案：`import "x/y/util"` 綁的是那個目錄裡
//! 所有檔案的匯出符號。import 表是「名字對到一個檔案」，對不上這個模型，
//! 所以 Go 走另一條既有的路——模組路徑。
//!
//! 做法是在抽取階段就把 `util.Thing()` 改寫成 `util::Thing`。抽取器看得
//! 到自己檔案的 import，知道 `util` 是套件名而不是變數名；解析階段接手
//! 時那已經是個限定名，`by_module` 拿它去比對 `files.module_path` 就找得
//! 到——與 Rust、Python 完全同一條路徑。
//!
//! 分不出來的（`b.Area()` 的 `b` 是變數）保留原文，交給解析階段判斷。

use std::collections::HashSet;

use tree_sitter::{Language, Node};

use super::super::ts;
use super::super::{Extractor, FileParse, Import, ImportTarget};
use super::common::{self, Declaration, TypeShapes};
use crate::extract::moniker;
use crate::model::{Kind, RawRef, Rel};

/// Go 的文件註解前綴。慣例是宣告正上方的 `//`。
const DOC_PREFIXES: &[&str] = &["//"];

/// 夾在文件註解與宣告之間、不打斷註解的節點。
const DOC_SKIP: &[&str] = &[];

/// 型別名在 Go 的語法樹裡長什麼樣。
///
/// `int` 這類內建型別也是 `type_identifier`，解析階段查不到就會被判為
/// 外部而丟棄，不必在這裡特別排除。
const TYPES: TypeShapes = TypeShapes {
    leaves: &["type_identifier"],
    scoped: &["qualified_type"],
    opaque: &[],
};

pub struct GoExtractor;

impl Extractor for GoExtractor {
    fn language(&self) -> &'static str {
        "go"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["go"]
    }

    fn extract(&self, rel_path: &str, source: &str) -> FileParse {
        let language: Language = tree_sitter_go::LANGUAGE.into();
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

        // import 要先收：改寫呼叫時得知道哪些名字是套件。
        let packages = collect_imports(root, source, &mut out);

        let path = moniker::normalize_path(rel_path);
        walk(root, source, &path, &[], &packages, &mut out);
        out
    }

    /// 套件路徑就是檔案所在的目錄。
    ///
    /// Go 沒有「代表目錄的那個檔案」這種慣例——目錄裡每個 `.go` 都屬於
    /// 同一個套件，因此 `directory_modules` 留空。
    fn module_path(&self, rel_path: &str) -> String {
        let normalized = rel_path.replace('\\', "/");
        let mut segments: Vec<&str> = normalized.split('/').collect();

        if !segments.pop().is_some_and(|f| f.ends_with(".go")) {
            return String::new();
        }
        segments.join("::")
    }
}

/// 這個檔案 import 了哪些套件名。
///
/// 回傳的是可以出現在呼叫左邊的名字：有別名就是別名，沒有就是 import
/// 路徑的最後一段。
fn collect_imports(root: Node<'_>, source: &str, out: &mut FileParse) -> HashSet<String> {
    let mut packages = HashSet::new();
    let mut cursor = root.walk();

    for child in root.named_children(&mut cursor) {
        if child.kind() != "import_declaration" {
            continue;
        }
        for spec in descendants(child, "import_spec") {
            let Some(path) = spec.child_by_field_name("path") else {
                continue;
            };
            // 字串節點含引號，內容在裡面那一層。
            let literal = path
                .named_child(0)
                .map(|n| ts::text(n, source))
                .unwrap_or_else(|| ts::text(path, source).trim_matches('"'));

            let segments: Vec<String> = literal
                .split('/')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            let Some(last) = segments.last().cloned() else {
                continue;
            };

            let local = match spec.child_by_field_name("name") {
                Some(alias) => ts::text(alias, source).to_string(),
                None => last,
            };
            // `_` 只為了副作用，`.` 把符號倒進當前命名空間，兩者都不是
            // 可以寫在呼叫左邊的名字。
            if local != "_" && local != "." {
                packages.insert(local.clone());
            }

            out.imports.push(Import {
                local,
                target: ImportTarget::Rooted(segments),
                line: ts::line_of(spec),
            });
        }
    }

    packages
}

/// 節點底下所有指定種類的後代。
fn descendants<'a>(node: Node<'a>, kind: &str) -> Vec<Node<'a>> {
    let mut found = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == kind {
            found.push(child);
        } else {
            found.extend(descendants(child, kind));
        }
    }
    found
}

/// 走訪一層節點。
fn walk(
    node: Node<'_>,
    source: &str,
    path: &str,
    container: &[String],
    packages: &HashSet<String>,
    out: &mut FileParse,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                function(
                    child,
                    source,
                    path,
                    container,
                    Kind::Function,
                    packages,
                    out,
                );
            }
            // 方法屬於接收者的型別，不屬於檔案。
            "method_declaration" => {
                let owner = receiver_type(child, source);
                let scope = owner.map(|o| vec![o]).unwrap_or_default();
                function(child, source, path, &scope, Kind::Method, packages, out);
            }
            // `type X struct{}` 是 type_spec，`type Id = string` 是
            // type_alias，兩種節點都要收。
            "type_declaration" => {
                for spec in descendants(child, "type_spec")
                    .into_iter()
                    .chain(descendants(child, "type_alias"))
                {
                    type_spec(spec, source, path, container, packages, out);
                }
            }
            "const_declaration" | "var_declaration" => {
                for spec in descendants(child, "const_spec")
                    .into_iter()
                    .chain(descendants(child, "var_spec"))
                {
                    value_spec(spec, source, path, container, packages, out);
                }
            }
            _ => {}
        }
    }
}

/// 函數與方法：簽名記型別，本體記呼叫。
fn function(
    node: Node<'_>,
    source: &str,
    path: &str,
    container: &[String],
    kind: Kind,
    packages: &HashSet<String>,
    out: &mut FileParse,
) {
    let Some(name) = common::field_text(node, "name", source) else {
        return;
    };
    let moniker = declare(node, source, path, container, kind, name, out);

    // 型別只看簽名：參數、回傳與接收者。本體裡的是實作細節。
    let mut found = Vec::new();
    for field in ["parameters", "result"] {
        if let Some(part) = node.child_by_field_name(field) {
            common::gather_types(part, source, TYPES, &[], &mut found);
        }
    }
    common::emit_types(&moniker, found, out);

    if let Some(body) = node.child_by_field_name("body") {
        collect_calls(body, source, &moniker, packages, out);
    }
}

/// `type X struct {...}` / `interface {...}` / `= Y`。
fn type_spec(
    node: Node<'_>,
    source: &str,
    path: &str,
    container: &[String],
    packages: &HashSet<String>,
    out: &mut FileParse,
) {
    let Some(name) = common::field_text(node, "name", source) else {
        return;
    };
    let inner = node.child_by_field_name("type");
    let kind = match inner.map(|n| n.kind()) {
        Some("struct_type") => Kind::Struct,
        Some("interface_type") => Kind::Interface,
        _ => Kind::TypeAlias,
    };
    let moniker = declare(node, source, path, container, kind, name, out);

    // 欄位與嵌入的型別就是這個宣告的依賴。
    if let Some(inner) = inner {
        let mut found = Vec::new();
        common::gather_types(inner, source, TYPES, &[], &mut found);
        common::emit_types(&moniker, found, out);
    }

    // interface 的方法各自是符號，掛在 interface 名下。
    let mut nested = container.to_vec();
    nested.push(name.to_string());
    for method in inner
        .map(|n| descendants(n, "method_elem"))
        .unwrap_or_default()
    {
        function(method, source, path, &nested, Kind::Method, packages, out);
    }
}

/// `const X = ...` / `var Y T = ...`。
fn value_spec(
    node: Node<'_>,
    source: &str,
    path: &str,
    container: &[String],
    packages: &HashSet<String>,
    out: &mut FileParse,
) {
    let Some(name) = common::field_text(node, "name", source) else {
        return;
    };
    let moniker = declare(node, source, path, container, Kind::Const, name, out);

    if let Some(annotation) = node.child_by_field_name("type") {
        let mut found = Vec::new();
        common::gather_types(annotation, source, TYPES, &[], &mut found);
        common::emit_types(&moniker, found, out);
    }
    if let Some(value) = node.child_by_field_name("value") {
        collect_calls(value, source, &moniker, packages, out);
    }
}

/// 收下一個符號，補上 Go 特有的簽名與註解取法。
fn declare(
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
            signature: common::signature(node, source, &["body"], &[]),
            docstring: ts::leading_line_comments(node, source, "comment", DOC_PREFIXES, DOC_SKIP),
        },
        out,
    )
}

/// 方法接收者的型別名。
///
/// `func (b *Box) Area()` 的接收者是 `*Box`，指標與泛型都要剝掉，方法才
/// 會掛在 `Box` 名下而不是散成好幾組限定名。
fn receiver_type(node: Node<'_>, source: &str) -> Option<String> {
    let receiver = node.child_by_field_name("receiver")?;
    let mut found = Vec::new();
    common::gather_types(receiver, source, TYPES, &[], &mut found);
    found.into_iter().next().map(|(name, _)| name)
}

/// 走遍節點底下所有的呼叫，記到 `from` 名下。
fn collect_calls(
    node: Node<'_>,
    source: &str,
    from: &str,
    packages: &HashSet<String>,
    out: &mut FileParse,
) {
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && let Some(name) = callee_name(function, source, packages)
    {
        out.refs.push(RawRef {
            from: from.to_string(),
            name,
            rel: Rel::Calls,
            line: ts::line_of(node),
        });
    }

    // `Box{...}` 是建構，目標是型別而不是函數。
    if node.kind() == "composite_literal"
        && let Some(literal) = node.child_by_field_name("type")
    {
        let mut found = Vec::new();
        common::gather_types(literal, source, TYPES, &[], &mut found);
        common::emit_types(from, found, out);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, source, from, packages, out);
    }
}

/// 被呼叫者在原始碼裡的寫法。
///
/// `util.Thing()` 的 `util` 若是 import 進來的套件，就改寫成限定名
/// `util::Thing`——那是解析階段認得的形式。是變數的話保留點號，讓解析
/// 階段知道這是對某個值呼叫方法。
fn callee_name(function: Node<'_>, source: &str, packages: &HashSet<String>) -> Option<String> {
    match function.kind() {
        "identifier" => Some(ts::text(function, source).to_string()),
        "selector_expression" => {
            let operand = function.child_by_field_name("operand")?;
            let field = function.child_by_field_name("field")?;
            let method = ts::text(field, source);
            let receiver = ts::collapse_whitespace(ts::text(operand, source));

            if operand.kind() == "identifier" && packages.contains(&receiver) {
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
        GoExtractor.extract("pkg/a.go", src)
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
            "package main\n\
             const Limit = 10\n\
             type Box struct {\n\tW int\n}\n\
             type Shape interface {\n\tArea() int\n}\n\
             type Id = string\n\
             func Make() {}\n",
        );

        assert_eq!(
            names(&p),
            ["Limit", "Box", "Shape", "Shape::Area", "Id", "Make"]
        );
        let kinds: Vec<Kind> = p.symbols.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Const,
                Kind::Struct,
                Kind::Interface,
                Kind::Method,
                Kind::TypeAlias,
                Kind::Function
            ]
        );
    }

    /// 方法掛在接收者的型別名下，指標要剝掉。
    #[test]
    fn a_method_is_qualified_by_its_receiver_type() {
        let p = parse("package main\nfunc (b *Box) Area() int {\n\treturn 1\n}\n");

        assert_eq!(names(&p), ["Box::Area"]);
        assert_eq!(p.symbols[0].kind, Kind::Method);
    }

    #[test]
    fn a_value_receiver_works_the_same_way() {
        let p = parse("package main\nfunc (b Box) Area() int {\n\treturn 1\n}\n");

        assert_eq!(names(&p), ["Box::Area"]);
    }

    #[test]
    fn struct_fields_record_their_types() {
        let p = parse("package main\ntype Holder struct {\n\tInner Widget\n\tTag string\n}\n");

        let types = refs_by(&p, "Holder", Rel::UsesType);
        assert!(types.contains(&"Widget".to_string()), "{types:?}");
    }

    #[test]
    fn a_signature_records_the_types_it_mentions() {
        let p = parse("package main\nfunc Make(w Widget) *Report {\n\treturn nil\n}\n");

        assert_eq!(refs_by(&p, "Make", Rel::UsesType), ["Widget", "Report"]);
    }

    /// 本體裡的型別是實作細節，不算對外依賴——但建構是。
    #[test]
    fn a_composite_literal_is_a_type_reference() {
        let p = parse("package main\nfunc Make() {\n\t_ = Box{}\n}\n");

        assert_eq!(refs_by(&p, "Make", Rel::UsesType), ["Box"]);
    }

    #[test]
    fn calls_are_attributed_to_the_enclosing_declaration() {
        let p = parse("package main\nfunc Outer() {\n\thelper()\n\tb.Area()\n}\n");

        assert_eq!(refs_by(&p, "Outer", Rel::Calls), ["helper", "b.Area"]);
    }

    /// 套件名寫在呼叫左邊時改寫成限定名，那是解析階段認得的形式。
    #[test]
    fn a_package_qualified_call_becomes_a_qualified_name() {
        let p = parse(
            "package main\n\
             import \"example.com/m/pkg/util\"\n\
             func Outer() {\n\tutil.Thing()\n}\n",
        );

        assert_eq!(refs_by(&p, "Outer", Rel::Calls), ["util::Thing"]);
    }

    /// 變數不是套件，點號要保留讓解析階段知道這是接收者呼叫。
    #[test]
    fn a_variable_receiver_keeps_its_dot() {
        let p = parse(
            "package main\n\
             import \"example.com/m/pkg/util\"\n\
             func Outer() {\n\tbox.Area()\n}\n",
        );

        assert_eq!(refs_by(&p, "Outer", Rel::Calls), ["box.Area"]);
    }

    #[test]
    fn an_aliased_import_binds_the_alias() {
        let p = parse(
            "package main\n\
             import u \"example.com/m/pkg/util\"\n\
             func Outer() {\n\tu.Thing()\n}\n",
        );

        assert_eq!(refs_by(&p, "Outer", Rel::Calls), ["u::Thing"]);
        assert_eq!(p.imports[0].local, "u");
    }

    /// `_` 只為了副作用，不是可以寫在呼叫左邊的名字。
    #[test]
    fn a_blank_import_binds_no_usable_name() {
        let p = parse("package main\nimport _ \"example.com/m/pkg/driver\"\n");

        assert_eq!(p.imports.len(), 1);
        assert_eq!(p.imports[0].local, "_");
    }

    #[test]
    fn grouped_imports_are_all_recorded() {
        let p = parse("package main\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n");

        let locals: Vec<&str> = p.imports.iter().map(|i| i.local.as_str()).collect();
        assert_eq!(locals, ["fmt", "os"]);
    }

    /// Go 的套件就是目錄，沒有代表目錄的那個檔案。
    #[test]
    fn module_paths_follow_the_directory_layout() {
        assert_eq!(GoExtractor.module_path("pkg/util/helpers.go"), "pkg::util");
        assert_eq!(GoExtractor.module_path("main.go"), "");
        assert_eq!(GoExtractor.module_path("README.md"), "");
        assert!(GoExtractor.directory_modules().is_empty());
    }

    #[test]
    fn module_paths_do_not_depend_on_the_path_separator() {
        assert_eq!(
            GoExtractor.module_path(r"pkg\util\helpers.go"),
            GoExtractor.module_path("pkg/util/helpers.go")
        );
    }

    #[test]
    fn a_syntax_error_still_yields_what_was_parsed() {
        let p = parse("package main\nfunc Ok() {}\nfunc Broken( {\n");

        assert!(!p.errors.is_empty());
        assert!(names(&p).contains(&"Ok"), "{:?}", names(&p));
    }
}
