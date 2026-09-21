//! PHP 抽取器。
//!
//! PHP 的呼叫寫法把「對誰呼叫」寫得比多數語言清楚，抽取階段因此能多做一些
//! 改寫，少留一些歧義給解析階段：
//!
//! - `$this->run()`、`self::run()`、`static::run()` 指的都是所在的類別，而
//!   那個類別名此刻就在容器鏈上，直接改寫成 `App::User::run`。
//! - `parent::boot()` 指的是 `extends` 的那一個，名字寫在 `base_clause` 裡。
//! - `Helper::slug()` 的 `Helper` 要嘛是檔案頂端 `use` 進來的名字——解析階段
//!   的 import 表正好認得它——要嘛就在當前的命名空間底下。
//!
//! 最後那一條是 PHP 的名字規則：未限定的名字以**當前命名空間**解釋，除非
//! `use` 過。抽取階段知道這兩件事，所以算得出限定名；解析階段不必認識 PHP。
//!
//! 命名空間是容器而不是符號：`namespace App\Models;` 這種寫法管的是整個檔案
//! 剩下的部分，留成符號的話每個檔案都會多一個橫跨全檔的東西。
//!
//! 屬性（`private int $age`）不留成符號。它們的名字短而且重複（`$name`、
//! `$id`、`$value`），留下來只會在查詢結果裡洗掉真正的答案；類別常數則會留，
//! 那是寫程式時真的會去查的東西。

use tree_sitter::{Language, Node};

use super::super::ts;
use super::super::{Extractor, FileParse, Import, ImportTarget};
use super::bindings::Bindings;
use super::common::{self, Declaration, TypeShapes};
use crate::extract::moniker;
use crate::model::{Kind, RawRef, Rel};

/// PHP 的文件註解。`/** ... */` 與 `//` 在語法樹裡都是 `comment`。
const DOC_PREFIXES: &[&str] = &["/**", "///", "//", "#"];

/// 夾在文件註解與宣告之間、不打斷註解的節點。
const DOC_SKIP: &[&str] = &["attribute_list"];

/// 型別名在 PHP 的語法樹裡長什麼樣。
///
/// `int`、`string` 這些是 `primitive_type`，不會被收進來。
const TYPES: TypeShapes = TypeShapes {
    leaves: &["name"],
    scoped: &[],
    opaque: &[],
};

/// 帶本體的類別種宣告，以及它們對應的種類。
const CONTAINERS: &[(&str, Kind)] = &[
    ("class_declaration", Kind::Class),
    ("interface_declaration", Kind::Interface),
    ("trait_declaration", Kind::Trait),
    ("enum_declaration", Kind::Enum),
];

pub struct PhpExtractor;

impl Extractor for PhpExtractor {
    fn language(&self) -> &'static str {
        "php"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["php", "phtml"]
    }

    fn extract(&self, rel_path: &str, source: &str) -> FileParse {
        let language: Language = tree_sitter_php::LANGUAGE_PHP.into();
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

        // import 要先收齊：判斷一個名字是不是 `use` 進來的，得先知道有哪些。
        collect_imports(root, source, &mut out);
        let locals: Vec<String> = out.imports.iter().map(|i| i.local.clone()).collect();

        let path = moniker::normalize_path(rel_path);
        let ctx = Ctx {
            path: &path,
            namespace: &[],
            container: &[],
            locals: &locals,
            owner: None,
        };
        walk(root, source, ctx, &mut out);
        out
    }

    /// PSR-4 把命名空間對到目錄，但對法寫在 `composer.json` 裡，路徑本身
    /// 說了不算。限定名帶著命名空間，跨檔的對應交給 import 表。
    fn module_path(&self, _rel_path: &str) -> String {
        String::new()
    }
}

/// 走訪時的位置。
#[derive(Clone, Copy)]
struct Ctx<'a> {
    path: &'a str,
    /// 目前的命名空間。
    namespace: &'a [String],
    /// 容器鏈：命名空間再加上類別。
    container: &'a [String],
    /// 這個檔案 `use` 進來的名字。
    locals: &'a [String],
    /// 目前所在的類別。
    owner: Option<&'a Owner>,
}

impl Ctx<'_> {
    /// 原始碼裡寫的類別名轉成限定名。
    ///
    /// 開頭是反斜線的已經是完整的；`use` 過的名字保持原樣，解析階段的 import
    /// 表認得它；其餘的補上當前命名空間——那是 PHP 的規則。
    fn resolve(&self, written: &str) -> String {
        let qualified = qualify(written);
        if written.starts_with('\\') || self.namespace.is_empty() {
            return qualified;
        }

        let first = qualified.split("::").next().unwrap_or(&qualified);
        if self.locals.iter().any(|l| l == first) {
            return qualified;
        }
        format!("{}::{qualified}", self.namespace.join("::"))
    }
}

/// 目前所在的類別。`$this` 與 `self::` 要靠它才知道指向誰。
struct Owner {
    /// 類別的限定名，例如 `App::Models::User`。
    qualified: String,
    /// `extends` 的那一個，`parent::` 指向它。
    parent: Option<String>,
}

/// 走訪一層節點。
fn walk(node: Node<'_>, source: &str, ctx: Ctx<'_>, out: &mut FileParse) {
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    walk_nodes(&children, source, ctx, out);
}

/// 走訪同一層的一串節點。
///
/// 沒有本體的 `namespace A;` 管的是它**後面**的所有宣告，所以遇到它就帶著新
/// 的命名空間走剩下的部分。
fn walk_nodes(children: &[Node<'_>], source: &str, ctx: Ctx<'_>, out: &mut FileParse) {
    for (i, child) in children.iter().enumerate() {
        let child = *child;
        match child.kind() {
            "namespace_definition" => {
                let mut nested = ctx.namespace.to_vec();
                if let Some(name) = child.child_by_field_name("name") {
                    nested.extend(segments_of(name, source));
                }
                let inner = Ctx {
                    namespace: &nested,
                    container: &nested,
                    ..ctx
                };
                match child.child_by_field_name("body") {
                    Some(body) => walk(body, source, inner, out),
                    None => {
                        walk_nodes(&children[i + 1..], source, inner, out);
                        return;
                    }
                }
            }
            "const_declaration" => constants(child, source, ctx, out),
            "function_definition" => function(child, source, ctx, Kind::Function, out),
            "method_declaration" => function(child, source, ctx, Kind::Method, out),
            kind if kind_of(kind).is_some() => {
                let Some(kind) = kind_of(kind) else { continue };
                type_decl(child, source, ctx, kind, out);
            }
            // 類別與列舉的本體。
            "declaration_list" | "enum_declaration_list" => walk(child, source, ctx, out),
            _ => {}
        }
    }
}

/// 這個節點種類對應的符號種類。
fn kind_of(node_kind: &str) -> Option<Kind> {
    CONTAINERS
        .iter()
        .find(|(k, _)| *k == node_kind)
        .map(|(_, kind)| *kind)
}

/// 原始碼寫的名字轉成限定名。`\App\Helper` 是 `App::Helper`。
fn qualify(written: &str) -> String {
    written.trim_start_matches('\\').replace('\\', "::")
}

/// `namespace_name` 或 `qualified_name` 拆成一段一段。
fn segments_of(node: Node<'_>, source: &str) -> Vec<String> {
    ts::text(node, source)
        .split('\\')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// 收齊整個檔案的 `use`，包含寫在 `namespace A { ... }` 裡的那些。
fn collect_imports(node: Node<'_>, source: &str, out: &mut FileParse) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "namespace_use_declaration" => uses(child, source, out),
            "namespace_definition" | "compound_statement" => collect_imports(child, source, out),
            _ => {}
        }
    }
}

/// `use App\Support\Helper;`、`use App\Support\{A, B};`、`use X as Y;`
///
/// 引入的名字是最後一段，有別名就是別名。目標從專案根算起——PSR-4 通常會讓
/// 命名空間少掉最前面幾段（`App\` 對到 `src/`），解析階段的結尾比對補得回來。
fn uses(node: Node<'_>, source: &str, out: &mut FileParse) {
    let mut cursor = node.walk();
    let prefix: Vec<String> = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "namespace_name")
        .map(|n| segments_of(n, source))
        .unwrap_or_default();

    for clause in clauses_of(node) {
        let mut inner = clause.walk();
        let Some(named) = clause
            .named_children(&mut inner)
            .find(|c| matches!(c.kind(), "name" | "qualified_name"))
        else {
            continue;
        };

        let mut segments = prefix.clone();
        segments.extend(segments_of(named, source));

        let alias = common::field_text(clause, "alias", source).map(str::to_string);
        let Some(local) = alias.or_else(|| segments.last().cloned()) else {
            continue;
        };
        out.imports.push(Import {
            local,
            target: ImportTarget::Rooted(segments),
            line: ts::line_of(clause),
        });
    }
}

/// 一條 `use` 底下的每個子句，群組寫法也走這裡。
fn clauses_of(node: Node<'_>) -> Vec<Node<'_>> {
    let mut found = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "namespace_use_clause" => found.push(child),
            "namespace_use_group" => {
                let mut inner = child.walk();
                found.extend(
                    child
                        .named_children(&mut inner)
                        .filter(|c| c.kind() == "namespace_use_clause"),
                );
            }
            _ => {}
        }
    }
    found
}

/// `class`、`interface`、`trait`、`enum`。
fn type_decl(node: Node<'_>, source: &str, ctx: Ctx<'_>, kind: Kind, out: &mut FileParse) {
    let Some(name) = common::field_text(node, "name", source) else {
        return;
    };
    let moniker = push(node, source, ctx, kind, name, out);

    // 繼承與實作的型別是這個宣告最實在的依賴。那兩個節點沒有欄位名。
    let mut found = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if matches!(child.kind(), "base_clause" | "class_interface_clause") {
            common::gather_types(child, source, TYPES, &[], &mut found);
        }
    }
    common::emit_types(&moniker, found, out);

    let mut nested = ctx.container.to_vec();
    nested.push(name.to_string());
    let owner = Owner {
        qualified: nested.join("::"),
        parent: parent_of(node, source).map(|written| ctx.resolve(&written)),
    };

    if let Some(body) = node.child_by_field_name("body") {
        let inner = Ctx {
            container: &nested,
            owner: Some(&owner),
            ..ctx
        };
        walk(body, source, inner, out);
    }
}

/// `extends` 的那一個類別，照原始碼寫的樣子。
///
/// 介面可以繼承多個，那種情況下 `parent::` 本來就不能用，取第一個即可。
fn parent_of(node: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let base = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "base_clause")?;

    let mut inner = base.walk();
    base.named_children(&mut inner)
        .find(|c| matches!(c.kind(), "name" | "qualified_name"))
        .map(|n| ts::text(n, source).to_string())
}

/// 函數與方法：簽名記型別，本體記呼叫。
fn function(node: Node<'_>, source: &str, ctx: Ctx<'_>, kind: Kind, out: &mut FileParse) {
    let Some(name) = common::field_text(node, "name", source) else {
        return;
    };
    let moniker = push(node, source, ctx, kind, name, out);

    // 參數只看型別那一欄：變數名底下也是 `name` 節點，整段收會把 `$u` 當成
    // 型別。
    let mut found = Vec::new();
    if let Some(parameters) = node.child_by_field_name("parameters") {
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            if let Some(annotation) = parameter.child_by_field_name("type") {
                common::gather_types(annotation, source, TYPES, &[], &mut found);
            }
        }
    }
    if let Some(returns) = node.child_by_field_name("return_type") {
        common::gather_types(returns, source, TYPES, &[], &mut found);
    }
    common::emit_types(&moniker, found, out);

    let Some(body) = node.child_by_field_name("body") else {
        // 抽象方法與介面的方法沒有本體，那是宣告的一半。
        return;
    };

    let mut bindings = Bindings::new();
    if let Some(parameters) = node.child_by_field_name("parameters") {
        bind_parameters(parameters, source, &mut bindings);
    }
    collect_calls(body, source, &moniker, ctx, &mut bindings, out);
}

/// 常數：`const MAX = 10;`，類別裡的也走這裡。
fn constants(node: Node<'_>, source: &str, ctx: Ctx<'_>, out: &mut FileParse) {
    let mut cursor = node.walk();
    for element in node.named_children(&mut cursor) {
        if element.kind() != "const_element" {
            continue;
        }
        let mut inner = element.walk();
        let Some(name) = element
            .named_children(&mut inner)
            .find(|c| c.kind() == "name")
        else {
            continue;
        };
        push(node, source, ctx, Kind::Const, ts::text(name, source), out);
    }
}

/// 收下一個符號，補上 PHP 特有的簽名與註解取法。
fn push(
    node: Node<'_>,
    source: &str,
    ctx: Ctx<'_>,
    kind: Kind,
    name: &str,
    out: &mut FileParse,
) -> String {
    common::push(
        node,
        ctx.path,
        Declaration {
            kind,
            name,
            container: ctx.container,
            signature: common::signature(node, source, &["body"], &[';', '{']),
            has_body: common::has_body(node, &["body"]),
            docstring: ts::leading_line_comments(node, source, "comment", DOC_PREFIXES, DOC_SKIP),
        },
        out,
    )
}

/// 記下參數的型別，`$u->greet()` 才知道 `$u` 是什麼。
fn bind_parameters(parameters: Node<'_>, source: &str, bindings: &mut Bindings) {
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        let Some(type_node) = parameter.child_by_field_name("type") else {
            continue;
        };
        let Some(name) = parameter.child_by_field_name("name") else {
            continue;
        };
        bindings.insert(
            ts::text(name, source),
            &base_type(ts::text(type_node, source)),
        );
    }
}

/// 型別寫法去掉可空標記：`?Helper` 與 `Helper` 是同一個型別。
///
/// 命名空間留著，交給 [`Ctx::resolve`] 決定要不要補上當前命名空間。
fn base_type(written: &str) -> String {
    written.trim_start_matches('?').trim().to_string()
}

/// 走遍節點底下所有的呼叫，記到 `from` 名下。
fn collect_calls(
    node: Node<'_>,
    source: &str,
    from: &str,
    ctx: Ctx<'_>,
    bindings: &mut Bindings,
    out: &mut FileParse,
) {
    if let Some(name) = callee_name(node, source, ctx, bindings) {
        out.refs.push(RawRef {
            from: from.to_string(),
            name,
            rel: Rel::Calls,
            line: ts::line_of(node),
        });
    }

    // `new Helper()` 的目標是型別，不是函數。
    if node.kind() == "object_creation_expression"
        && let Some(created) = created_type(node)
    {
        let mut found = Vec::new();
        common::gather_types(created, source, TYPES, &[], &mut found);
        common::emit_types(from, found, out);
    }

    let opens_block = node.kind() == "compound_statement";
    if opens_block {
        bindings.enter();
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, source, from, ctx, bindings, out);
    }

    // 綁定在右邊走完之後才生效。
    if node.kind() == "assignment_expression" {
        bind_assignment(node, source, bindings);
    }

    if opens_block {
        bindings.leave();
    }
}

/// `new Helper()` 建出來的那個型別。
fn created_type(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| matches!(c.kind(), "name" | "qualified_name"))
}

/// `$u = new Helper();` 記下 `$u` 的型別。
fn bind_assignment(node: Node<'_>, source: &str, bindings: &mut Bindings) {
    let Some(left) = node.child_by_field_name("left") else {
        return;
    };
    let Some(right) = node.child_by_field_name("right") else {
        return;
    };
    if left.kind() != "variable_name" || right.kind() != "object_creation_expression" {
        return;
    }
    let Some(created) = created_type(right) else {
        return;
    };
    bindings.insert(
        ts::text(left, source),
        &base_type(ts::text(created, source)),
    );
}

/// 被呼叫者在原始碼裡的寫法，能指名的就改寫成限定名。
fn callee_name(node: Node<'_>, source: &str, ctx: Ctx<'_>, bindings: &Bindings) -> Option<String> {
    match node.kind() {
        // `helper()`、`\App\helper()`
        "function_call_expression" => {
            let function = node.child_by_field_name("function")?;
            matches!(function.kind(), "name" | "qualified_name")
                .then(|| qualify(ts::text(function, source)))
        }
        // `$u->greet()`、`$this->run()`、`$u?->greet()`
        "member_call_expression" | "nullsafe_member_call_expression" => {
            let name = ts::text(node.child_by_field_name("name")?, source);
            let object = node.child_by_field_name("object")?;
            let receiver = ts::collapse_whitespace(ts::text(object, source));

            if receiver == "$this"
                && let Some(owner) = ctx.owner
            {
                return Some(format!("{}::{name}", owner.qualified));
            }
            if object.kind() == "variable_name"
                && let Some(type_name) = bindings.get(&receiver)
            {
                return Some(format!("{}::{name}", ctx.resolve(type_name)));
            }
            Some(format!("{receiver}.{name}"))
        }
        // `Helper::slug()`、`self::run()`、`parent::boot()`
        "scoped_call_expression" => {
            let name = ts::text(node.child_by_field_name("name")?, source);
            let written = ts::text(node.child_by_field_name("scope")?, source);

            let resolved = match written {
                "self" | "static" => ctx.owner.map(|o| o.qualified.clone()),
                "parent" => ctx.owner.and_then(|o| o.parent.clone()),
                other => Some(ctx.resolve(other)),
            }?;
            Some(format!("{resolved}::{name}"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> FileParse {
        PhpExtractor.extract("src/Models/User.php", src)
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

    fn calls(p: &FileParse, from_name: &str) -> Vec<String> {
        refs_by(p, from_name, Rel::Calls)
    }

    #[test]
    fn top_level_declarations_are_extracted() {
        let p = parse(
            "<?php\n\
             class Box {}\n\
             interface Shape {}\n\
             trait Timestamps {}\n\
             enum Status { case Active; }\n\
             function helper(): void {}\n\
             const MAX = 10;\n",
        );

        assert_eq!(
            names(&p),
            ["Box", "Shape", "Timestamps", "Status", "helper", "MAX"]
        );
        let kinds: Vec<Kind> = p.symbols.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Class,
                Kind::Interface,
                Kind::Trait,
                Kind::Enum,
                Kind::Function,
                Kind::Const
            ]
        );
    }

    /// 沒有本體的 `namespace A\B;` 管的是檔案剩下的部分。
    #[test]
    fn a_namespace_qualifies_everything_after_it() {
        let p = parse("<?php\nnamespace App\\Models;\nclass User { public function name() {} }\n");

        assert_eq!(names(&p), ["App::Models::User", "App::Models::User::name"]);
    }

    /// 有本體的寫法只管到大括號為止。
    #[test]
    fn a_braced_namespace_only_covers_its_body() {
        let p = parse("<?php\nnamespace App { class In {} }\nnamespace { class Out {} }\n");

        assert_eq!(names(&p), ["App::In", "Out"]);
    }

    #[test]
    fn a_namespace_is_not_a_symbol_of_its_own() {
        let p = parse("<?php\nnamespace App\\Models;\n");

        assert!(p.symbols.is_empty());
    }

    #[test]
    fn methods_are_qualified_by_their_class() {
        let p = parse("<?php\nclass User {\n  public function name(): string { return 'a'; }\n}\n");

        assert_eq!(names(&p), ["User", "User::name"]);
        assert_eq!(p.symbols[1].kind, Kind::Method);
    }

    /// 抽象方法與介面的方法沒有本體，那是宣告的一半。
    #[test]
    fn an_abstract_method_has_no_body() {
        let p = parse(
            "<?php\n\
             abstract class Base { abstract public function run(): void; }\n\
             class Impl extends Base { public function run(): void {} }\n",
        );

        let bodies: Vec<bool> = p
            .symbols
            .iter()
            .filter(|s| s.name == "run")
            .map(|s| s.has_body)
            .collect();
        assert_eq!(bodies, [false, true]);
    }

    #[test]
    fn class_constants_are_symbols_but_properties_are_not() {
        let p = parse(
            "<?php\nclass User {\n  public const ROLE = 'admin';\n  private int $age = 0;\n}\n",
        );

        assert_eq!(names(&p), ["User", "User::ROLE"]);
    }

    #[test]
    fn extends_and_implements_are_type_references() {
        let p = parse("<?php\nclass User extends Model implements Jsonable {}\n");

        assert_eq!(refs_by(&p, "User", Rel::UsesType), ["Model", "Jsonable"]);
    }

    #[test]
    fn parameter_and_return_types_are_recorded() {
        let p = parse("<?php\nfunction helper(User $u): Status { return $u; }\n");

        assert_eq!(refs_by(&p, "helper", Rel::UsesType), ["User", "Status"]);
    }

    // ── 呼叫 ───────────────────────────────────────────────

    /// `$this->` 指的就是所在的類別，那個名字此刻在容器鏈上。
    #[test]
    fn this_becomes_the_enclosing_class() {
        let p = parse(
            "<?php\nnamespace App;\nclass User {\n  public function boot() { $this->name(); }\n  public function name() {}\n}\n",
        );

        assert_eq!(calls(&p, "boot"), ["App::User::name"]);
    }

    #[test]
    fn self_and_static_also_become_the_enclosing_class() {
        let p = parse(
            "<?php\nclass User {\n  public function boot() { self::stat(); static::stat(); }\n  public static function stat() {}\n}\n",
        );

        assert_eq!(calls(&p, "boot"), ["User::stat", "User::stat"]);
    }

    /// `parent::` 指的是 `extends` 的那一個，名字寫在 `base_clause` 裡。
    #[test]
    fn parent_becomes_the_base_class() {
        let p = parse(
            "<?php\nclass User extends Model {\n  public function boot() { parent::boot(); }\n}\n",
        );

        assert_eq!(calls(&p, "boot"), ["Model::boot"]);
    }

    /// 父類別沒有 `use` 過，就在當前的命名空間底下。
    #[test]
    fn parent_is_resolved_in_the_current_namespace() {
        let p = parse(
            "<?php\nnamespace App;\nclass User extends Model {\n  public function boot() { parent::boot(); }\n}\n",
        );

        assert_eq!(calls(&p, "boot"), ["App::Model::boot"]);
    }

    /// 沒有父類別時不硬掰一個。
    #[test]
    fn parent_without_a_base_class_is_not_guessed() {
        let p = parse("<?php\nclass User {\n  public function boot() { parent::boot(); }\n}\n");

        assert!(calls(&p, "boot").is_empty());
    }

    #[test]
    fn a_static_call_keeps_the_class_name() {
        let p = parse(
            "<?php\nfunction go() { Helper::slug('x'); \\App\\Support\\Helper::slug('y'); }\n",
        );

        assert_eq!(
            calls(&p, "go"),
            ["Helper::slug", "App::Support::Helper::slug"]
        );
    }

    /// PHP 的規則：未限定的類別名以當前命名空間解釋。
    #[test]
    fn an_unqualified_class_is_in_the_current_namespace() {
        let p = parse("<?php\nnamespace App;\nfunction go() { Helper::slug('x'); }\n");

        assert_eq!(calls(&p, "go"), ["App::Helper::slug"]);
    }

    /// `use` 過的名字保持原樣，解析階段的 import 表認得它。
    #[test]
    fn an_imported_class_keeps_its_written_name() {
        let p = parse(
            "<?php\nnamespace App;\nuse Lib\\Helper;\nfunction go() { Helper::slug('x'); }\n",
        );

        assert_eq!(calls(&p, "go"), ["Helper::slug"]);
    }

    /// 參數有型別標註，接收者就查得到型別。
    #[test]
    fn a_typed_parameter_makes_the_receiver_resolvable() {
        let p = parse("<?php\nfunction go(User $u) { $u->name(); }\n");

        assert_eq!(calls(&p, "go"), ["User::name"]);
    }

    /// 可空標記不改變型別，完整寫出的命名空間照樣保留。
    #[test]
    fn a_nullable_fully_qualified_type_hint_is_resolved() {
        let p = parse("<?php\nfunction go(?\\App\\User $u) { $u->name(); }\n");

        assert_eq!(calls(&p, "go"), ["App::User::name"]);
    }

    #[test]
    fn a_new_expression_binds_the_variable_and_records_the_type() {
        let p = parse("<?php\nfunction go() { $u = new User(); $u->name(); }\n");

        assert_eq!(refs_by(&p, "go", Rel::UsesType), ["User"]);
        assert_eq!(calls(&p, "go"), ["User::name"]);
    }

    #[test]
    fn a_nullsafe_call_is_a_call() {
        let p = parse("<?php\nfunction go(User $u) { $u?->name(); }\n");

        assert_eq!(calls(&p, "go"), ["User::name"]);
    }

    /// 型別查不到的接收者保留原文。
    #[test]
    fn an_unknown_receiver_keeps_the_written_form() {
        let p = parse("<?php\nfunction go($u) { $u->name(); }\n");

        assert_eq!(calls(&p, "go"), ["$u.name"]);
    }

    #[test]
    fn a_plain_function_call_is_recorded() {
        let p = parse("<?php\nfunction go() { helper(1); \\App\\helper(2); }\n");

        assert_eq!(calls(&p, "go"), ["helper", "App::helper"]);
    }

    // ── import ─────────────────────────────────────────────

    #[test]
    fn a_use_statement_becomes_an_import() {
        let p = parse("<?php\nuse App\\Support\\Helper;\n");

        assert_eq!(p.imports.len(), 1);
        assert_eq!(p.imports[0].local, "Helper");
        assert_eq!(
            p.imports[0].target,
            ImportTarget::Rooted(vec![
                "App".to_string(),
                "Support".to_string(),
                "Helper".to_string()
            ])
        );
    }

    #[test]
    fn an_alias_is_the_name_this_file_sees() {
        let p = parse("<?php\nuse App\\Support\\Helper as H;\n");

        assert_eq!(p.imports[0].local, "H");
    }

    #[test]
    fn a_group_use_brings_in_every_name() {
        let p = parse("<?php\nuse App\\Support\\{A, B};\n");

        let locals: Vec<&str> = p.imports.iter().map(|i| i.local.as_str()).collect();
        assert_eq!(locals, ["A", "B"]);
        assert_eq!(
            p.imports[1].target,
            ImportTarget::Rooted(vec![
                "App".to_string(),
                "Support".to_string(),
                "B".to_string()
            ])
        );
    }

    #[test]
    fn a_use_inside_a_braced_namespace_is_collected() {
        let p = parse("<?php\nnamespace App { use Lib\\Helper; class In {} }\n");

        assert_eq!(p.imports.len(), 1);
        assert_eq!(p.imports[0].local, "Helper");
    }

    // ── 其他 ───────────────────────────────────────────────

    #[test]
    fn a_doc_comment_above_a_declaration_is_kept() {
        let p = parse("<?php\n/** 使用者。 */\nclass User {}\n");

        assert!(
            p.symbols[0]
                .docstring
                .as_deref()
                .is_some_and(|d| d.contains("使用者")),
            "{:?}",
            p.symbols[0].docstring
        );
    }

    #[test]
    fn a_signature_stops_before_the_body() {
        let p = parse("<?php\nfunction helper(int $a): string { return 'x'; }\n");

        assert_eq!(
            p.symbols[0].signature.as_deref(),
            Some("function helper(int $a): string")
        );
    }

    #[test]
    fn php_has_no_module_tree_and_no_implicit_receiver() {
        assert_eq!(PhpExtractor.module_path("src/Models/User.php"), "");
        assert!(!PhpExtractor.implicit_receiver());
        assert_eq!(PhpExtractor.family(), "php");
    }

    #[test]
    fn a_file_with_syntax_errors_still_yields_symbols() {
        let p = parse("<?php\nclass User { public function name() {} }\nfunction broken(\n");

        assert!(names(&p).contains(&"User"));
        assert!(!p.errors.is_empty(), "語法錯誤要回報");
    }

    #[test]
    fn an_empty_file_is_empty() {
        let p = parse("");

        assert!(p.is_empty());
        assert!(p.errors.is_empty());
    }
}
