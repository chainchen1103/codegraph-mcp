//! Rust 抽取器。
//!
//! 採用明確的遞迴走訪而非 tree-sitter query。函數與方法的區分取決於
//! 祖先節點，容器名稱也需要沿祖先鏈累積，這類帶上下文的判斷用宣告式
//! query 表達不便。

use tree_sitter::{Language, Node};

use super::super::ts;
use super::super::{Extractor, FileParse, Import, ImportTarget};
use super::bindings::Bindings;
use super::common::{self, Declaration, TypeShapes};
use crate::extract::moniker;
use crate::model::{Kind, RawRef, Rel};

/// Rust 的文件註解前綴。
const DOC_PREFIXES: &[&str] = &["///", "//!"];

/// 夾在文件註解與宣告之間、不打斷註解的節點。
const DOC_SKIP: &[&str] = &["attribute_item"];

/// 型別名在 Rust 的語法樹裡長什麼樣。
///
/// 原生型別是 `primitive_type`，不是 `type_identifier`，自然不會被收
/// 進來。帶路徑的型別只取最後一段：`crate::a::Widget` 與 `Widget` 是
/// 同一個型別。
const TYPES: TypeShapes = TypeShapes {
    leaves: &["type_identifier"],
    scoped: &["scoped_type_identifier"],
    opaque: &[],
};

pub struct RustExtractor;

impl Extractor for RustExtractor {
    fn language(&self) -> &'static str {
        "rust"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn extract(&self, rel_path: &str, source: &str) -> FileParse {
        let language: Language = tree_sitter_rust::LANGUAGE.into();
        let Some(tree) = ts::parse(&language, source) else {
            return FileParse {
                errors: vec![format!("{rel_path}：tree-sitter 無法解析")],
                ..Default::default()
            };
        };

        let mut out = FileParse::default();
        let root = tree.root_node();
        if root.has_error() {
            // 編輯中的檔案經常暫時不合語法，仍然回傳已抽到的符號。
            out.errors
                .push(format!("{rel_path}：有語法錯誤，結果可能不完整"));
        }

        let path = moniker::normalize_path(rel_path);
        walk(root, source, &path, &Scope::default(), &mut out);
        out
    }

    /// 慣例：`mod.rs`、`lib.rs`、`main.rs` 代表所在目錄本身；`src/` 以外
    /// 的根目錄底下每個檔案自成一個 crate，模組路徑為空。
    fn directory_modules(&self) -> &'static [&'static str] {
        &["mod.rs", "lib.rs"]
    }

    fn module_path(&self, rel_path: &str) -> String {
        let normalized = rel_path.replace('\\', "/");
        let mut segments: Vec<&str> = normalized.split('/').collect();

        let Some(stem) = segments.pop().and_then(|f| f.strip_suffix(".rs")) else {
            return String::new();
        };

        let in_src = segments.first() == Some(&"src");
        if segments.first().is_some_and(|s| SOURCE_ROOTS.contains(s)) {
            segments.remove(0);
        }

        // src 以外的根目錄，每個檔案自成一個 crate 的根。
        if !in_src && segments.is_empty() {
            return String::new();
        }

        if !matches!(stem, "mod" | "lib" | "main") {
            segments.push(stem);
        }

        segments.join("::")
    }
}

/// 這些目錄各自是一棵原始碼樹的根。
const SOURCE_ROOTS: [&str; 4] = ["src", "tests", "benches", "examples"];

/// 走訪時攜帶的上下文。
#[derive(Clone, Debug, Default)]
struct Scope {
    /// 祖先鏈上的名字，用於組出限定名。
    container: Vec<String>,
    /// 直屬容器是否為型別，也就是 `impl` 或 `trait`。
    ///
    /// 決定函數要記成 function 還是 method。模組底下的函數不是方法。
    in_type: bool,
}

impl Scope {
    fn child(&self, name: &str, in_type: bool) -> Self {
        let mut container = self.container.clone();
        container.push(name.to_string());
        Self { container, in_type }
    }

    /// `self` 在這個位置指的是哪個型別。
    fn self_type(&self) -> Option<&str> {
        if self.in_type {
            self.container.last().map(String::as_str)
        } else {
            None
        }
    }
}

/// 走訪一層節點，把找到的宣告收進 `out`。
fn walk(node: Node<'_>, source: &str, path: &str, scope: &Scope, out: &mut FileParse) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "mod_item" => {
                let Some(name) = common::field_text(child, "name", source) else {
                    continue;
                };
                declare_symbol(child, source, path, scope, Kind::Module, name, out);
                descend(child, source, path, &scope.child(name, false), out);
            }
            "trait_item" => {
                let Some(name) = common::field_text(child, "name", source) else {
                    continue;
                };
                let moniker = declare_symbol(child, source, path, scope, Kind::Trait, name, out);
                // 本體裡的方法簽名各自是符號，各自記自己用到的型別。
                collect_types(child, source, &moniker, Body::Skip, out);
                descend(child, source, path, &scope.child(name, true), out);
            }
            // impl 區塊本身不是符號，只提供方法所屬的型別。
            "impl_item" => {
                let name = child
                    .child_by_field_name("type")
                    .map(|n| type_base_name(n, source))
                    .unwrap_or_else(|| "impl".to_string());
                let before = out.symbols.len();
                descend(child, source, path, &scope.child(&name, true), out);
                collect_implemented_trait(child, source, before, out);
            }
            // 不把本體裡的巢狀函數當成符號，它們無法從外部呼叫，但本體
            // 裡的呼叫仍要記錄下來。
            "function_item" | "function_signature_item" => {
                let Some(name) = common::field_text(child, "name", source) else {
                    continue;
                };
                let kind = if scope.in_type {
                    Kind::Method
                } else {
                    Kind::Function
                };
                let moniker = declare_symbol(child, source, path, scope, kind, name, out);
                // 本體裡的型別是區域的實作細節，不算這個函數的對外依賴。
                collect_types(child, source, &moniker, Body::Skip, out);
                if let Some(body) = child.child_by_field_name("body") {
                    let mut bindings = Bindings::new();
                    bind_parameters(child, source, scope, &mut bindings);
                    collect_calls(body, source, &moniker, &mut bindings, out);
                }
            }
            "use_declaration" => {
                if let Some(argument) = child.child_by_field_name("argument") {
                    collect_use(argument, source, &[], out);
                }
            }
            // 欄位與變體的型別都在本體裡，這幾種要連本體一起看。
            "struct_item" => leaf(child, source, path, scope, Kind::Struct, out),
            "enum_item" => leaf(child, source, path, scope, Kind::Enum, out),
            "union_item" => leaf(child, source, path, scope, Kind::Struct, out),
            "type_item" => leaf(child, source, path, scope, Kind::TypeAlias, out),
            "const_item" | "static_item" => leaf(child, source, path, scope, Kind::Const, out),
            _ => {}
        }
    }
}

/// 收下一個符號，補上 Rust 特有的限定名與文件註解取法。
fn declare_symbol(
    node: Node<'_>,
    source: &str,
    path: &str,
    scope: &Scope,
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
            container: &scope.container,
            signature: common::signature(node, source, &["body"], &['=', ':']),
            has_body: common::has_body(node, &["body"]),
            docstring: ts::leading_line_comments(
                node,
                source,
                "line_comment",
                DOC_PREFIXES,
                DOC_SKIP,
            ),
        },
        out,
    )
}

/// 走一條 `use`，把它引入的每個名字記下來。
///
/// `prefix` 是已經走過的模組段。`use a::{b, c::d}` 會分岔成兩條路徑，
/// 因此要一路把前綴帶下去。
fn collect_use(node: Node<'_>, source: &str, prefix: &[String], out: &mut FileParse) {
    match node.kind() {
        // a::b::Thing
        "scoped_identifier" => {
            let mut segments = prefix.to_vec();
            if let Some(path) = node.child_by_field_name("path") {
                segments.extend(path_segments(path, source));
            }
            if let Some(name) = node.child_by_field_name("name") {
                let local = ts::text(name, source).to_string();
                push_import(local, segments, ts::line_of(node), out);
            }
        }
        // a::{b, c}
        "scoped_use_list" => {
            let mut segments = prefix.to_vec();
            if let Some(path) = node.child_by_field_name("path") {
                segments.extend(path_segments(path, source));
            }
            if let Some(list) = node.child_by_field_name("list") {
                let mut cursor = list.walk();
                for item in list.named_children(&mut cursor) {
                    collect_use(item, source, &segments, out);
                }
            }
        }
        // {b, c} 裡的一項，或整條 `use a::b as c`
        "use_as_clause" => {
            let (Some(path), Some(alias)) = (
                node.child_by_field_name("path"),
                node.child_by_field_name("alias"),
            ) else {
                return;
            };
            // `use std::fmt::Write as _` 沒有引入可用的名字。
            let local = ts::text(alias, source);
            if local == "_" {
                return;
            }

            let mut segments = prefix.to_vec();
            if let Some(container) = path.child_by_field_name("path") {
                segments.extend(path_segments(container, source));
            }
            push_import(local.to_string(), segments, ts::line_of(node), out);
        }
        // {b, c} 裡不帶別名的那幾項
        "identifier" => {
            let local = ts::text(node, source).to_string();
            push_import(local, prefix.to_vec(), ts::line_of(node), out);
        }
        // `use a::*` 沒有引入具體的名字，無從記錄。
        _ => {}
    }
}

/// 把模組路徑攤平成一串段。
fn path_segments(node: Node<'_>, source: &str) -> Vec<String> {
    match node.kind() {
        "scoped_identifier" => {
            let mut out = node
                .child_by_field_name("path")
                .map(|p| path_segments(p, source))
                .unwrap_or_default();
            if let Some(name) = node.child_by_field_name("name") {
                out.push(ts::text(name, source).to_string());
            }
            out
        }
        _ => vec![ts::text(node, source).to_string()],
    }
}

/// 把 Rust 的模組路徑翻成與語言無關的目標。
///
/// `crate::` 從專案根算起，`super::` 是同一個目錄裡的兄弟模組——當前模
/// 組就是這個檔案，它的上一層即所在目錄。`self::` 與外部 crate 都無從
/// 判斷，交給解析階段比對不到之後判為外部。
fn push_import(local: String, segments: Vec<String>, line: u32, out: &mut FileParse) {
    let mut segments = segments;
    let target = match segments.first().map(String::as_str) {
        Some("crate") => {
            segments.remove(0);
            ImportTarget::Rooted(segments)
        }
        Some("super") => {
            segments.remove(0);
            ImportTarget::Relative(segments.join("/"))
        }
        Some("self") => {
            segments.remove(0);
            ImportTarget::Relative(segments.join("/"))
        }
        Some(_) => ImportTarget::Rooted(segments),
        None => return,
    };

    out.imports.push(Import {
        local,
        target,
        line,
    });
}

/// 走進容器的本體，沒有本體時不做任何事。
fn descend(node: Node<'_>, source: &str, path: &str, scope: &Scope, out: &mut FileParse) {
    if let Some(body) = node.child_by_field_name("body") {
        walk(body, source, path, scope, out);
    }
}

fn leaf(node: Node<'_>, source: &str, path: &str, scope: &Scope, kind: Kind, out: &mut FileParse) {
    if let Some(name) = common::field_text(node, "name", source) {
        let moniker = declare_symbol(node, source, path, scope, kind, name, out);
        collect_types(node, source, &moniker, Body::Include, out);
    }
}

/// 找型別時要不要連宣告的本體一起看。
#[derive(Copy, Clone, PartialEq, Eq)]
enum Body {
    /// 結構的欄位、列舉的變體都在本體裡，那些型別就是這個宣告的依賴。
    Include,
    /// 函數本體裡的型別是區域的實作細節，trait 本體裡的簽名各自是符號。
    Skip,
}

/// 記下宣告用到的型別，成為 `UsesType` 引用。
///
/// 這是 blast radius 的材料：改一個型別會波及誰，答案就是誰的宣告提到
/// 了它。呼叫關係回答不了這個問題——把一個型別多加一個欄位，用到它的
/// 函數一個都沒被呼叫，照樣全部要跟著改。
fn collect_types(node: Node<'_>, source: &str, from: &str, body: Body, out: &mut FileParse) {
    // 自己宣告的泛型參數不是對外依賴，`fn f<T>(x: T)` 的 `T` 不指向任何
    // 符號。約束裡的型別則要記，`T: Extractor` 確實用到了 Extractor。
    let declared = declared_type_parameters(node, source);
    let skip_body = (body == Body::Skip)
        .then(|| node.child_by_field_name("body"))
        .flatten();
    // `struct Widget` 的 `Widget` 在語法上也是個型別節點，但那是這個
    // 宣告自己，不是它用到的東西。
    let own_name = node.child_by_field_name("name");

    let mut found: Vec<(String, u32)> = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if Some(child) == skip_body || Some(child) == own_name {
            continue;
        }
        common::gather_types(child, source, TYPES, &declared, &mut found);
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

/// `impl Trait for Type` 的 `Trait`，記到這個區塊裡的每個方法名下。
///
/// impl 區塊本身不是符號，沒有東西可以當引用的起點。記到方法上是真的：
/// 那些方法之所以存在，就是因為要實作這個 trait。自身型別（`for` 後面
/// 那個）不記——方法屬於它是同一件事的兩種說法，記了只是噪音。
fn collect_implemented_trait(node: Node<'_>, source: &str, from_index: usize, out: &mut FileParse) {
    let Some(implemented) = node.child_by_field_name("trait") else {
        return;
    };

    let declared = declared_type_parameters(node, source);
    let mut found: Vec<(String, u32)> = Vec::new();
    common::gather_types(implemented, source, TYPES, &declared, &mut found);
    if found.is_empty() {
        return;
    }

    let monikers: Vec<String> = out.symbols[from_index..]
        .iter()
        .map(|s| s.moniker.clone())
        .collect();
    for moniker in monikers {
        for (name, line) in &found {
            out.refs.push(RawRef {
                from: moniker.clone(),
                name: name.clone(),
                rel: Rel::Implements,
                line: *line,
            });
        }
    }
}

/// 這個宣告自己引入的泛型參數名。
fn declared_type_parameters(node: Node<'_>, source: &str) -> Vec<String> {
    let mut declared = Vec::new();
    // 依節點種類而不是欄位名尋找：不同宣告把泛型參數掛在不同的欄位下。
    let mut top = node.walk();
    let Some(parameters) = node
        .named_children(&mut top)
        .find(|c| c.kind() == "type_parameters")
    else {
        return declared;
    };

    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        // 裸的 `T` 自己就是名字；帶約束或預設值的把名字放在欄位裡，而
        // 欄位名依語法版本而異，兩個都試。
        let named = if parameter.kind() == "type_identifier" {
            Some(parameter)
        } else {
            parameter
                .child_by_field_name("name")
                .or_else(|| parameter.child_by_field_name("left"))
        };
        if let Some(named) = named
            && named.kind() == "type_identifier"
        {
            declared.push(ts::text(named, source).to_string());
        }
    }
    declared
}

/// 走遍節點底下所有的呼叫，記到 `from` 名下。
///
/// 一邊走一邊維護區塊與變數綁定：接收者的型別在這裡查得到的話，呼叫
/// 就直接記成 `Type::method`，解析階段走限定名比對並驗證該型別確實有
/// 這個方法。查不到就保留原文，交給解析階段判斷。
///
/// 巢狀函數與閉包裡的呼叫都算在外層函數頭上，它們是同一段邏輯的一
/// 部分。
fn collect_calls(
    node: Node<'_>,
    source: &str,
    from: &str,
    bindings: &mut Bindings,
    out: &mut FileParse,
) {
    if node.kind() == "call_expression"
        && let Some(name) = callee_name(node, source, bindings)
    {
        out.refs.push(RawRef {
            from: from.to_string(),
            name,
            rel: Rel::Calls,
            line: ts::line_of(node),
        });
    }

    let opens_block = matches!(node.kind(), "block" | "closure_expression");
    if opens_block {
        bindings.enter();
        if node.kind() == "closure_expression" {
            bind_closure_parameters(node, source, bindings);
        }
    }

    // 依原始碼順序遞迴，輸出才與檔案內容一致。
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, source, from, bindings, out);
    }

    // 綁定在初始化運算式走完之後才生效：`let x = x.wrap();` 右邊的 `x`
    // 指的是舊的那一個。
    if node.kind() == "let_declaration" {
        bind_let(node, source, bindings);
    }

    if opens_block {
        bindings.leave();
    }
}

/// 記下函數參數與 `self` 的型別。
fn bind_parameters(function: Node<'_>, source: &str, scope: &Scope, bindings: &mut Bindings) {
    if let Some(self_type) = scope.self_type() {
        bindings.insert("self", self_type);
    }

    let Some(parameters) = function.child_by_field_name("parameters") else {
        return;
    };

    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        bind_typed_pattern(parameter, source, bindings);
    }
}

/// 閉包參數只有帶型別標註時才記得下來。
fn bind_closure_parameters(closure: Node<'_>, source: &str, bindings: &mut Bindings) {
    let Some(parameters) = closure.child_by_field_name("parameters") else {
        return;
    };
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        bind_typed_pattern(parameter, source, bindings);
    }
}

/// `name: Type` 這種形狀的宣告。
fn bind_typed_pattern(node: Node<'_>, source: &str, bindings: &mut Bindings) {
    let (Some(pattern), Some(type_node)) = (
        node.child_by_field_name("pattern"),
        node.child_by_field_name("type"),
    ) else {
        return;
    };
    if pattern.kind() != "identifier" {
        return;
    }
    bindings.insert(
        ts::text(pattern, source),
        &type_base_name(type_node, source),
    );
}

/// `let` 綁定的型別，來自標註或初始化運算式。
fn bind_let(node: Node<'_>, source: &str, bindings: &mut Bindings) {
    let Some(pattern) = node.child_by_field_name("pattern") else {
        return;
    };
    if pattern.kind() != "identifier" {
        return;
    }
    let name = ts::text(pattern, source);

    if let Some(type_node) = node.child_by_field_name("type") {
        bindings.insert(name, &type_base_name(type_node, source));
        return;
    }

    if let Some(value) = node.child_by_field_name("value")
        && let Some(inferred) = initializer_type(value, source)
    {
        bindings.insert(name, &inferred);
    }
}

/// 從初始化運算式看得出來的型別。
///
/// 只認結構明確的幾種寫法。看不出來就不記，讓解析階段知道這裡沒有
/// 型別資訊，而不是給它一個猜的。
fn initializer_type(value: Node<'_>, source: &str) -> Option<String> {
    match value.kind() {
        // Foo { .. }
        "struct_expression" => value
            .child_by_field_name("name")
            .map(|n| type_base_name(n, source)),
        // Foo::new(..)
        "call_expression" => {
            let function = value.child_by_field_name("function")?;
            if function.kind() != "scoped_identifier" {
                return None;
            }
            let container = function.child_by_field_name("path")?;
            let name = type_base_name(container, source);
            // 只有大寫開頭才是型別，`store::open()` 的 `store` 是模組。
            name.starts_with(char::is_uppercase).then_some(name)
        }
        // &expr / &mut expr
        "reference_expression" => value
            .child_by_field_name("value")
            .and_then(|inner| initializer_type(inner, source)),
        _ => None,
    }
}

/// 被呼叫者在原始碼裡的寫法。
///
/// 接收者的型別查得到時改寫成 `Type::method`，這樣解析階段就能用限定
/// 名比對並驗證。查不到則保留原文，寫法裡的句點會讓解析階段知道這是
/// 對某個值呼叫方法。
fn callee_name(call: Node<'_>, source: &str, bindings: &Bindings) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    callee_name_of(function, source, bindings)
}

fn callee_name_of(function: Node<'_>, source: &str, bindings: &Bindings) -> Option<String> {
    match function.kind() {
        // foo() 與 a::b::c()
        "identifier" | "scoped_identifier" => Some(ts::text(function, source).to_string()),
        // x.method()
        "field_expression" => {
            let field = function.child_by_field_name("field")?;
            let method = ts::text(field, source);
            let value = function.child_by_field_name("value")?;
            let receiver = ts::collapse_whitespace(ts::text(value, source));

            if matches!(value.kind(), "identifier" | "self")
                && let Some(type_name) = bindings.get(&receiver)
            {
                return Some(format!("{type_name}::{method}"));
            }
            Some(format!("{receiver}.{method}"))
        }
        // foo::<T>()
        "generic_function" => function
            .child_by_field_name("function")
            .and_then(|inner| callee_name_of(inner, source, bindings)),
        _ => None,
    }
}

/// 取得型別運算式的基底名稱。
///
/// `Widget<T>`、`crate::a::Widget`、`&Widget` 指的都是同一個型別。不
/// 收斂成同一個名字，同一型別的方法會散落在多組限定名之下。
fn type_base_name(node: Node<'_>, source: &str) -> String {
    match node.kind() {
        "generic_type" => node
            .child_by_field_name("type")
            .map(|n| type_base_name(n, source))
            .unwrap_or_else(|| ts::text(node, source).to_string()),
        "scoped_type_identifier" => node
            .child_by_field_name("name")
            .map(|n| ts::text(n, source).to_string())
            .unwrap_or_else(|| ts::text(node, source).to_string()),
        // 參考與指標的內層欄位是 type，陣列是 element。
        "reference_type" | "pointer_type" | "array_type" | "slice_type" => node
            .child_by_field_name("type")
            .or_else(|| node.child_by_field_name("element"))
            .map(|n| type_base_name(n, source))
            .unwrap_or_else(|| ts::text(node, source).to_string()),
        _ => ts::text(node, source).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RawSymbol;

    fn parse(src: &str) -> FileParse {
        RustExtractor.extract("src/a.rs", src)
    }

    fn names(p: &FileParse) -> Vec<&str> {
        p.symbols.iter().map(|s| s.qualified.as_str()).collect()
    }

    fn find<'a>(p: &'a FileParse, qualified: &str) -> &'a RawSymbol {
        p.symbols
            .iter()
            .find(|s| s.qualified == qualified)
            .unwrap_or_else(|| panic!("找不到 {qualified}，有的是 {:?}", names(p)))
    }

    #[test]
    fn top_level_items_are_captured_with_their_kinds() {
        let p = parse(
            "fn free() {}\n\
             struct S { a: u8 }\n\
             enum E { A }\n\
             trait T {}\n\
             type Alias = u8;\n\
             const C: u8 = 1;\n\
             static ST: u8 = 2;\n\
             mod m {}\n",
        );

        assert_eq!(find(&p, "free").kind, Kind::Function);
        assert_eq!(find(&p, "S").kind, Kind::Struct);
        assert_eq!(find(&p, "E").kind, Kind::Enum);
        assert_eq!(find(&p, "T").kind, Kind::Trait);
        assert_eq!(find(&p, "Alias").kind, Kind::TypeAlias);
        assert_eq!(find(&p, "C").kind, Kind::Const);
        assert_eq!(find(&p, "ST").kind, Kind::Const);
        assert_eq!(find(&p, "m").kind, Kind::Module);
    }

    #[test]
    fn impl_blocks_turn_functions_into_methods() {
        let p = parse("struct S;\nimpl S {\n    fn open() {}\n}\n");

        let m = find(&p, "S::open");
        assert_eq!(m.kind, Kind::Method);
        assert_eq!(m.name, "open");
        assert!(!names(&p).contains(&"impl"), "impl 區塊本身不該是符號");
    }

    #[test]
    fn every_impl_of_a_type_lands_under_the_same_name() {
        let p = parse(
            "struct Widget<T>(T);\n\
             impl<T> Widget<T> {\n    fn new() {}\n}\n\
             impl std::fmt::Display for Widget<u8> {\n    fn fmt() {}\n}\n\
             impl crate::other::Widget {\n    fn extra() {}\n}\n",
        );

        let n = names(&p);
        assert!(n.contains(&"Widget::new"), "{n:?}");
        assert!(n.contains(&"Widget::fmt"), "{n:?}");
        assert!(n.contains(&"Widget::extra"), "{n:?}");
        assert!(
            !n.iter().any(|q| q.contains('<')),
            "限定名裡混進了泛型參數：{n:?}"
        );
    }

    #[test]
    fn wrapper_types_in_impl_targets_are_unwrapped() {
        let p = parse(
            "struct W;\n\
             impl Marker for &W {\n    fn a() {}\n}\n\
             impl Marker for [W; 4] {\n    fn b() {}\n}\n\
             impl Marker for *const W {\n    fn c() {}\n}\n",
        );
        let n = names(&p);
        for expected in ["W::a", "W::b", "W::c"] {
            assert!(n.contains(&expected), "少了 {expected}：{n:?}");
        }
    }

    #[test]
    fn an_impl_on_an_unnamed_target_still_yields_its_methods() {
        let p = parse("impl Marker for (u8, u8) {\n    fn tupled() {}\n}\n");
        assert_eq!(p.symbols.len(), 1);
        assert_eq!(p.symbols[0].name, "tupled");
        assert_eq!(p.symbols[0].kind, Kind::Method);
    }

    #[test]
    fn unions_are_treated_like_structs() {
        let p = parse("union U {\n    a: u8,\n    b: u16,\n}\n");
        assert_eq!(find(&p, "U").kind, Kind::Struct);
    }

    #[test]
    fn trait_impls_attribute_methods_to_the_type_not_the_trait() {
        let p = parse("struct S;\nimpl std::fmt::Display for S {\n    fn fmt() {}\n}\n");
        assert!(names(&p).contains(&"S::fmt"), "{:?}", names(&p));
    }

    #[test]
    fn nested_modules_accumulate_qualified_names() {
        let p = parse("mod a {\n    mod b {\n        fn c() {}\n    }\n}\n");
        assert!(names(&p).contains(&"a::b::c"), "{:?}", names(&p));
    }

    #[test]
    fn functions_in_modules_are_not_methods() {
        let p =
            parse("mod tests {\n    fn helper() {}\n}\nstruct S;\nimpl S {\n    fn m() {}\n}\n");
        assert_eq!(find(&p, "tests::helper").kind, Kind::Function);
        assert_eq!(find(&p, "S::m").kind, Kind::Method);
    }

    #[test]
    fn a_module_inside_a_type_resets_the_method_context() {
        let p = parse("trait T {\n    fn required(&self);\n}\nmod m {\n    fn plain() {}\n}\n");
        assert_eq!(find(&p, "T::required").kind, Kind::Method);
        assert_eq!(find(&p, "m::plain").kind, Kind::Function);
    }

    #[test]
    fn a_module_declaration_without_a_body_is_fine() {
        let p = parse("mod other;\nfn f() {}\n");
        assert_eq!(find(&p, "other").kind, Kind::Module);
        assert_eq!(find(&p, "f").kind, Kind::Function);
    }

    #[test]
    fn trait_method_signatures_without_bodies_are_captured() {
        let p = parse("trait T {\n    fn required(&self) -> u8;\n}\n");
        let m = find(&p, "T::required");
        assert_eq!(m.kind, Kind::Method);
        assert_eq!(m.signature.as_deref(), Some("fn required(&self) -> u8;"));
    }

    #[test]
    fn functions_nested_inside_bodies_are_ignored() {
        let p = parse("fn outer() {\n    fn inner() {}\n}\n");
        assert_eq!(names(&p), vec!["outer"]);
    }

    #[test]
    fn line_numbers_point_at_the_declaration() {
        let p = parse("\n\n// 一般註解\nfn target() {\n    let x = 1;\n}\n");
        let f = find(&p, "target");
        assert_eq!(f.start_line, 4);
        assert_eq!(f.end_line, 6);
    }

    #[test]
    fn signatures_are_the_declaration_without_the_body() {
        let p =
            parse("fn wide(\n    a: u8,\n    b: u8,\n) -> Result<(), Error> {\n    todo!()\n}\n");
        let f = find(&p, "wide");
        assert_eq!(
            f.signature.as_deref(),
            Some("fn wide( a: u8, b: u8, ) -> Result<(), Error>")
        );
        assert!(
            !f.signature.as_deref().unwrap().contains("todo!"),
            "簽名不該包含 body"
        );
    }

    #[test]
    fn doc_comments_are_collected_and_stripped() {
        let p = parse("/// 第一行\n/// 第二行\nfn documented() {}\n");
        assert_eq!(
            find(&p, "documented").docstring.as_deref(),
            Some("第一行\n第二行")
        );
    }

    #[test]
    fn attributes_do_not_hide_doc_comments() {
        let p = parse("/// 說明\n#[derive(Debug, Clone)]\n#[non_exhaustive]\nstruct S;\n");
        assert_eq!(find(&p, "S").docstring.as_deref(), Some("說明"));
    }

    #[test]
    fn a_blank_line_ends_the_doc_block() {
        let p = parse("/// 屬於別人的註解\n\n/// 屬於這裡的\nfn f() {}\n");
        assert_eq!(find(&p, "f").docstring.as_deref(), Some("屬於這裡的"));
    }

    #[test]
    fn plain_comments_are_not_documentation() {
        let p = parse("// 只是註解\nfn f() {}\n");
        assert_eq!(find(&p, "f").docstring, None);
    }

    #[test]
    fn monikers_follow_the_documented_shape() {
        let p = parse("fn f() {}\n");
        assert_eq!(find(&p, "f").moniker, "src/a.rs:function:f:1");
    }

    #[test]
    fn a_broken_file_still_yields_what_it_can() {
        let p = parse("fn good() {}\nfn broken( { \n");
        assert!(!p.errors.is_empty(), "語法錯誤沒有被回報");
        assert!(names(&p).contains(&"good"), "壞檔案讓好符號一起消失了");
    }

    #[test]
    fn an_empty_file_produces_nothing_and_no_errors() {
        let p = parse("");
        assert!(p.symbols.is_empty());
        assert!(p.errors.is_empty());
    }

    /// 某個符號發出的、指定種類的引用。
    fn refs_by(p: &FileParse, from_name: &str, rel: Rel) -> Vec<String> {
        let from = p
            .symbols
            .iter()
            .find(|s| s.name == from_name || s.qualified == from_name)
            .unwrap_or_else(|| panic!("找不到 {from_name}"));
        p.refs
            .iter()
            .filter(|r| r.from == from.moniker && r.rel == rel)
            .map(|r| r.name.clone())
            .collect()
    }

    fn refs_of(p: &FileParse, from_name: &str) -> Vec<String> {
        refs_by(p, from_name, Rel::Calls)
    }

    fn types_of(p: &FileParse, from_name: &str) -> Vec<String> {
        refs_by(p, from_name, Rel::UsesType)
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

    #[test]
    fn a_crate_path_import_is_rooted_at_the_project() {
        let p = parse(
            "use crate::a::b::Thing;
",
        );

        assert_eq!(imports_of(&p), [("Thing".to_string(), rooted(&["a", "b"]))]);
    }

    /// `super::` 指的是上一層模組，也就是這個檔案所在的目錄。
    #[test]
    fn a_super_path_import_is_relative() {
        let p = parse(
            "use super::sibling::Thing;
",
        );

        assert_eq!(
            imports_of(&p),
            [(
                "Thing".to_string(),
                ImportTarget::Relative("sibling".to_string())
            )]
        );
    }

    /// 大括號會分岔，每個名字各自成一條 import。
    #[test]
    fn a_braced_use_records_every_name_it_binds() {
        let p = parse(
            "use crate::a::{One, Two as Alias};
",
        );

        assert_eq!(
            imports_of(&p),
            [
                ("One".to_string(), rooted(&["a"])),
                ("Alias".to_string(), rooted(&["a"])),
            ]
        );
    }

    /// `as _` 沒有引入可用的名字。
    #[test]
    fn an_underscore_alias_binds_nothing() {
        let p = parse(
            "use std::fmt::Write as _;
",
        );

        assert!(p.imports.is_empty(), "{:?}", p.imports);
    }

    /// 萬用字元沒有指名任何東西，記不下來。
    #[test]
    fn a_wildcard_use_records_nothing() {
        let p = parse(
            "use crate::a::*;
",
        );

        assert!(p.imports.is_empty(), "{:?}", p.imports);
    }

    /// 外部 crate 與頂層模組長得一樣，一律當成從根算起，解析不到就是外部。
    #[test]
    fn a_bare_path_is_rooted_and_may_turn_out_external() {
        let p = parse(
            "use serde::Serialize;
",
        );

        assert_eq!(
            imports_of(&p),
            [("Serialize".to_string(), rooted(&["serde"]))]
        );
    }

    #[test]
    fn a_signature_records_the_types_it_mentions() {
        let p = parse("fn build(w: Widget, n: u32) -> Report {\n    make()\n}\n");

        assert_eq!(types_of(&p, "build"), ["Widget", "Report"]);
    }

    /// 原生型別不是符號，記了只會變成永遠解析不了的雜訊。
    #[test]
    fn primitives_are_not_type_references() {
        let p = parse("fn count(n: u32, ok: bool) -> usize {\n    0\n}\n");

        assert!(
            types_of(&p, "count").is_empty(),
            "{:?}",
            types_of(&p, "count")
        );
    }

    /// 結構的欄位型別就是它的依賴。
    #[test]
    fn struct_fields_record_their_types() {
        let p = parse("struct Holder {\n    inner: Widget,\n    tags: Vec<Label>,\n}\n");

        assert_eq!(types_of(&p, "Holder"), ["Widget", "Vec", "Label"]);
    }

    /// 宣告自己的名字不是它用到的型別。
    #[test]
    fn a_declaration_does_not_reference_itself() {
        let p = parse("struct Widget;\nenum Colour { Red }\ntype Alias = Widget;\n");

        assert!(types_of(&p, "Widget").is_empty());
        assert!(types_of(&p, "Colour").is_empty());
        assert_eq!(types_of(&p, "Alias"), ["Widget"]);
    }

    /// 自己引入的泛型參數不指向任何符號，但約束裡的型別要記。
    #[test]
    fn generic_parameters_are_skipped_but_their_bounds_are_not() {
        let p = parse("fn run<T: Extractor>(item: T) -> T {\n    item\n}\n");

        assert_eq!(types_of(&p, "run"), ["Extractor"]);
    }

    /// 路徑只記最後那一段，`crate::a::Widget` 與 `Widget` 是同一個型別。
    #[test]
    fn a_scoped_type_records_only_its_base_name() {
        let p = parse("fn take(w: crate::a::Widget) {}\n");

        assert_eq!(types_of(&p, "take"), ["Widget"]);
    }

    /// `Self` 指的是所屬型別，不是另一個符號。
    #[test]
    fn self_is_not_a_type_reference() {
        let p = parse("struct A;\nimpl A {\n    fn make() -> Self {\n        A\n    }\n}\n");

        assert!(types_of(&p, "A::make").is_empty());
    }

    /// 本體裡的型別是實作細節，不算這個函數的對外依賴。
    #[test]
    fn types_inside_a_body_are_not_counted() {
        let p = parse("fn run() {\n    let x: Local = Local::new();\n}\n");

        assert!(types_of(&p, "run").is_empty(), "{:?}", types_of(&p, "run"));
    }

    /// 實作一個 trait 記在該區塊的每個方法名下——impl 本身不是符號。
    #[test]
    fn implementing_a_trait_is_recorded_on_its_methods() {
        let p = parse(
            "struct Square;\nimpl Shape for Square {\n    fn area(&self) {}\n    fn name(&self) {}\n}\n",
        );

        for method in ["Square::area", "Square::name"] {
            let implemented = refs_by(&p, method, Rel::Implements);
            assert_eq!(implemented, ["Shape"], "{method}");
        }
    }

    /// 沒有 trait 的 inherent impl 不產生實作關係。
    #[test]
    fn an_inherent_impl_implements_nothing() {
        let p = parse("struct A;\nimpl A {\n    fn run(&self) {}\n}\n");

        assert!(refs_by(&p, "A::run", Rel::Implements).is_empty());
    }

    /// 同一個型別在一個宣告裡出現多次只記一次，避免同一條邊灌水。
    #[test]
    fn a_type_named_twice_in_one_declaration_is_recorded_once() {
        let p = parse("fn swap(a: Widget, b: Widget) -> Widget {\n    a\n}\n");

        assert_eq!(types_of(&p, "swap"), ["Widget"]);
    }

    #[test]
    fn plain_calls_are_recorded_against_the_enclosing_function() {
        let p = parse("fn caller() {\n    callee();\n}\n");

        assert_eq!(refs_of(&p, "caller"), vec!["callee"]);
        assert_eq!(p.refs[0].rel, Rel::Calls);
        assert_eq!(p.refs[0].line, 2);
    }

    #[test]
    fn qualified_calls_keep_their_full_path() {
        let p = parse("fn caller() {\n    Store::open();\n    a::b::c();\n}\n");
        assert_eq!(refs_of(&p, "caller"), vec!["Store::open", "a::b::c"]);
    }

    /// 接收者的型別查不到時保留原文。句點的存在讓解析階段知道這是對
    /// 某個值呼叫方法，而不是直接呼叫函數。
    #[test]
    fn a_method_call_on_an_unknown_receiver_keeps_the_written_form() {
        let p = parse("fn caller() {\n    let s = make();\n    s.open();\n}\n");
        assert_eq!(refs_of(&p, "caller"), vec!["make", "s.open"]);
    }

    /// 參數有型別標註，方法呼叫直接記成限定名。
    #[test]
    fn a_typed_parameter_turns_a_method_call_into_a_qualified_name() {
        let p = parse("fn caller(s: Store) {\n    s.open();\n}\n");
        assert_eq!(refs_of(&p, "caller"), vec!["Store::open"]);
    }

    /// `self` 就是所屬的型別，不需要推測。
    #[test]
    fn self_resolves_to_the_enclosing_type() {
        let p = parse(
            "struct S;\nimpl S {\n    fn a(&self) {\n        self.b();\n    }\n    fn b(&self) {}\n}\n",
        );
        assert_eq!(refs_of(&p, "S::a"), vec!["S::b"]);
    }

    /// 自由函數裡的 `self` 沒有型別可言。
    #[test]
    fn self_outside_a_type_has_no_binding() {
        let p = parse("fn caller() {\n    self.close();\n}\n");
        assert_eq!(refs_of(&p, "caller"), vec!["self.close"]);
    }

    #[test]
    fn a_let_binding_with_an_annotation_is_tracked() {
        let p = parse("fn caller() {\n    let s: Store = build();\n    s.open();\n}\n");
        assert!(refs_of(&p, "caller").contains(&"Store::open".to_string()));
    }

    #[test]
    fn a_let_binding_initialised_by_a_constructor_is_tracked() {
        let p = parse(
            "fn caller() {\n    let s = Store::new();\n    let w = Writer { inner: 1 };\n\
             s.open();\n    w.flush();\n}\n",
        );
        let refs = refs_of(&p, "caller");
        assert!(refs.contains(&"Store::open".to_string()), "{refs:?}");
        assert!(refs.contains(&"Writer::flush".to_string()), "{refs:?}");
    }

    /// 模組路徑不是型別，`store::open()` 的 `store` 不能拿來當接收者型別。
    #[test]
    fn a_module_path_initialiser_is_not_treated_as_a_type() {
        let p = parse("fn caller() {\n    let s = store::open();\n    s.flush();\n}\n");
        assert!(refs_of(&p, "caller").contains(&"s.flush".to_string()));
    }

    /// 內層區塊的綁定遮蔽外層，離開區塊之後外層的綁定回來。
    #[test]
    fn an_inner_binding_shadows_the_outer_one() {
        let p = parse(
            "fn caller(s: Store) {\n    {\n        let s: Writer = build();\n        s.run();\n    }\n\
             s.run();\n}\n",
        );
        let refs = refs_of(&p, "caller");
        assert!(refs.contains(&"Writer::run".to_string()), "{refs:?}");
        assert!(refs.contains(&"Store::run".to_string()), "{refs:?}");
    }

    /// 初始化運算式裡的名字指的是舊的綁定。
    #[test]
    fn a_binding_takes_effect_only_after_its_initialiser() {
        let p = parse("fn caller(s: Store) {\n    let s = Writer { inner: s.take() };\n}\n");
        assert!(
            refs_of(&p, "caller").contains(&"Store::take".to_string()),
            "{:?}",
            refs_of(&p, "caller")
        );
    }

    #[test]
    fn a_typed_closure_parameter_is_tracked() {
        let p = parse("fn caller() {\n    run(|s: Store| {\n        s.open();\n    });\n}\n");
        assert!(refs_of(&p, "caller").contains(&"Store::open".to_string()));
    }

    #[test]
    fn turbofish_calls_are_recorded_by_name() {
        let p = parse("fn caller() {\n    parse::<u8>();\n}\n");
        assert_eq!(refs_of(&p, "caller"), vec!["parse"]);
    }

    #[test]
    fn calls_inside_closures_belong_to_the_enclosing_function() {
        let p = parse("fn caller() {\n    run(|| {\n        inner();\n    });\n}\n");
        let refs = refs_of(&p, "caller");
        assert!(refs.contains(&"run".to_string()), "{refs:?}");
        assert!(refs.contains(&"inner".to_string()), "{refs:?}");
    }

    #[test]
    fn calls_are_attributed_to_the_method_that_contains_them() {
        let p = parse(
            "struct S;\nimpl S {\n    fn a(&self) {\n        helper();\n    }\n    fn b(&self) {}\n}\n",
        );

        assert_eq!(refs_of(&p, "S::a"), vec!["helper"]);
        assert!(refs_of(&p, "S::b").is_empty());
    }

    #[test]
    fn chained_calls_are_all_recorded() {
        let p = parse("fn caller() {\n    first().second().third();\n}\n");
        let refs = refs_of(&p, "caller");
        assert!(refs.iter().any(|r| r == "first"), "{refs:?}");
        assert!(refs.iter().any(|r| r.ends_with(".second")), "{refs:?}");
        assert!(refs.iter().any(|r| r.ends_with(".third")), "{refs:?}");
    }

    /// 巨集不是呼叫，語意由巨集展開決定，靜態解析看不到。
    #[test]
    fn macro_invocations_are_not_recorded_as_calls() {
        let p = parse("fn caller() {\n    println!(\"hi\");\n    vec![1, 2];\n}\n");
        assert!(refs_of(&p, "caller").is_empty(), "{:?}", p.refs);
    }

    #[test]
    fn a_function_without_calls_produces_no_refs() {
        let p = parse("fn quiet() {\n    let x = 1;\n}\n");
        assert!(p.refs.is_empty());
    }

    #[test]
    fn call_extraction_is_deterministic() {
        let src = "fn caller() {\n    a();\n    b();\n    c();\n}\n";
        let first: Vec<String> = parse(src).refs.into_iter().map(|r| r.name).collect();
        let second: Vec<String> = parse(src).refs.into_iter().map(|r| r.name).collect();
        assert_eq!(first, second);
    }

    #[test]
    fn extraction_order_is_deterministic() {
        let src = "fn b() {}\nfn a() {}\nstruct C;\n";
        let first: Vec<String> = parse(src).symbols.into_iter().map(|s| s.moniker).collect();
        let second: Vec<String> = parse(src).symbols.into_iter().map(|s| s.moniker).collect();
        assert_eq!(first, second);

        // 依原始碼出現順序，不是字母順序。
        assert_eq!(
            first,
            vec![
                "src/a.rs:function:b:1",
                "src/a.rs:function:a:2",
                "src/a.rs:struct:C:3"
            ]
        );
    }
}
