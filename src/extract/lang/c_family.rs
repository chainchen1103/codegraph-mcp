//! C / C++ / CUDA 抽取器。
//!
//! 三個語言共用這一套走訪邏輯。C++ 的語法樹是 C 的超集，CUDA 又是 C++ 的
//! 超集——kernel 啟動 `f<<<a, b>>>(x)` 只是在 `call_expression` 底下多掛一個
//! `kernel_call_syntax`，被呼叫者仍然在 `function` 欄位。三者的差別只有文
//! 法、副檔名，以及裸名可不可以是方法，所以下面三個抽取器只是外殼。
//!
//! 這一族的特別之處是**宣告與定義分居兩個檔案**：header 寫 `void f(void);`，
//! `.c` / `.cpp` 寫本體。兩邊都留成獨立的符號，靠 `has_body` 與限定名接起
//! 來（見 [`crate::resolve::definitions`]）。限定名因此必須在三種寫法底下
//! 都算出同一個答案：
//!
//! ```cpp
//! namespace a { class W { void draw(); }; }   // 容器 a::W
//! namespace a { void W::draw() {} }           // 容器 a，宣告器上的範圍 W
//! void a::W::draw() {}                        // 範圍全寫在宣告器上
//! ```
//!
//! 所以命名空間與類別都算容器，而宣告器上寫出來的範圍接在容器後面。
//!
//! 命名空間本身不留成符號：它在每個檔案裡被重新打開，留下來等於每個檔案
//! 都多一個橫跨全檔的巨大符號，查詢時只會洗掉真正的答案。結構的資料欄位
//! 同理——`x`、`len`、`next` 這種名字留成符號沒有幫助。

use tree_sitter::{Language, Node};

use super::super::ts;
use super::super::{Extractor, FileParse, Import, ImportTarget};
use super::bindings::Bindings;
use super::common::{self, Declaration, TypeShapes};
use crate::extract::moniker;
use crate::model::{Kind, RawRef, Rel};

/// 文件註解的前綴。`//`、`///`、`/** */` 在語法樹裡都是 `comment`。
const DOC_PREFIXES: &[&str] = &["///", "/**", "//"];

/// 夾在文件註解與宣告之間、不打斷註解的節點。
const DOC_SKIP: &[&str] = &["attribute_declaration"];

/// 型別名在這一族的語法樹裡長什麼樣。
///
/// 只認 `type_identifier`：`int` 這類內建型別是 `primitive_type`，本來就不
/// 該收。`std::vector<Widget>` 不當成單一節點處理，而是讓走訪穿進去，收到
/// `vector` 與 `Widget` 兩個名字——前者解析不到，會被當成外部丟掉，後者正
/// 是這個宣告真正依賴的東西。
const TYPES: TypeShapes = TypeShapes {
    leaves: &["type_identifier"],
    scoped: &[],
    opaque: &[],
};

/// 宣告器外面可以包著的東西，剝掉才看得到名字。
const WRAPPERS: &[&str] = &[
    "pointer_declarator",
    "reference_declarator",
    "parenthesized_declarator",
    "array_declarator",
    "init_declarator",
];

/// 預處理指令裡還有宣告，要繼續往裡面走。
///
/// header 幾乎都整份包在 `#ifndef GUARD` 裡，不穿進去就一個符號都抽不到。
const PREPROC_BLOCKS: &[&str] = &[
    "preproc_ifdef",
    "preproc_if",
    "preproc_else",
    "preproc_elif",
    "preproc_elifdef",
];

/// C、C++、CUDA 共用符號：同一份 header 三者都 include 得到。
const FAMILY: &str = "c";

pub struct CExtractor;

impl Extractor for CExtractor {
    fn language(&self) -> &'static str {
        "c"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["c"]
    }

    fn extract(&self, rel_path: &str, source: &str) -> FileParse {
        extract_with(tree_sitter_c::LANGUAGE.into(), rel_path, source)
    }

    /// C 沒有由路徑決定的模組樹，`#include` 寫的是檔案路徑。
    fn module_path(&self, _rel_path: &str) -> String {
        String::new()
    }

    fn family(&self) -> &'static str {
        FAMILY
    }
}

pub struct CppExtractor;

impl Extractor for CppExtractor {
    fn language(&self) -> &'static str {
        "cpp"
    }

    /// `.h` 歸 C++：副檔名本身分不出這個 header 是給誰用的，而 C++ 的文法
    /// 涵蓋 C 的宣告。實測在純 C 的 header 上兩個文法的錯誤數一樣。
    fn extensions(&self) -> &'static [&'static str] {
        &[
            "cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "h", "ipp", "tpp", "inl",
        ]
    }

    fn extract(&self, rel_path: &str, source: &str) -> FileParse {
        extract_with(tree_sitter_cpp::LANGUAGE.into(), rel_path, source)
    }

    fn module_path(&self, _rel_path: &str) -> String {
        String::new()
    }

    /// 成員函數裡寫 `helper()` 就是 `this->helper()`。
    fn implicit_receiver(&self) -> bool {
        true
    }

    fn family(&self) -> &'static str {
        FAMILY
    }
}

pub struct CudaExtractor;

impl Extractor for CudaExtractor {
    fn language(&self) -> &'static str {
        "cuda"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["cu", "cuh"]
    }

    fn extract(&self, rel_path: &str, source: &str) -> FileParse {
        extract_with(tree_sitter_cuda::LANGUAGE.into(), rel_path, source)
    }

    fn module_path(&self, _rel_path: &str) -> String {
        String::new()
    }

    fn implicit_receiver(&self) -> bool {
        true
    }

    fn family(&self) -> &'static str {
        FAMILY
    }
}

/// 用指定的文法抽取一個 C 家族的檔案。
fn extract_with(language: Language, rel_path: &str, source: &str) -> FileParse {
    let Some(tree) = ts::parse(&language, source) else {
        return FileParse {
            errors: vec![format!("{rel_path}：tree-sitter 無法解析")],
            ..Default::default()
        };
    };

    let mut out = FileParse::default();
    let root = tree.root_node();
    if root.has_error() {
        // 巨集展開前的程式碼本來就解析不完整，抽到的部分照樣有用。
        out.errors
            .push(format!("{rel_path}：有語法錯誤，結果可能不完整"));
    }

    let path = moniker::normalize_path(rel_path);
    walk(root, source, &path, &[], false, &mut out);
    out
}

/// 走訪一層節點。
///
/// `container` 是祖先鏈上的名字，`in_type` 表示現在在類別或結構的本體裡——
/// 那裡的函數是方法。
fn walk(
    node: Node<'_>,
    source: &str,
    path: &str,
    container: &[String],
    in_type: bool,
    out: &mut FileParse,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "preproc_include" => include(child, source, out),
            // 物件巨集是常數，函數巨集寫起來、用起來都跟函數一樣。
            "preproc_def" => macro_symbol(child, source, path, container, Kind::Const, out),
            "preproc_function_def" => {
                macro_symbol(child, source, path, container, Kind::Function, out);
            }
            kind if PREPROC_BLOCKS.contains(&kind) => {
                walk(child, source, path, container, in_type, out);
            }
            // `namespace a { ... }` 加一層容器；`extern "C" { ... }` 與
            // `template<...>` 只是把宣告包起來，什麼都不加。
            "namespace_definition" => namespace(child, source, path, container, out),
            "linkage_specification" | "declaration_list" | "template_declaration" => {
                walk(child, source, path, container, in_type, out);
            }
            "class_specifier" => type_decl(child, source, path, container, Kind::Class, out),
            "struct_specifier" | "union_specifier" => {
                type_decl(child, source, path, container, Kind::Struct, out);
            }
            "enum_specifier" => type_decl(child, source, path, container, Kind::Enum, out),
            "type_definition" => typedef(child, source, path, container, out),
            "alias_declaration" => alias(child, source, path, container, out),
            "function_definition" => function(child, source, path, container, in_type, out),
            // 有 `function_declarator` 的是函數宣告，其餘是變數。
            "declaration" | "field_declaration" => {
                if function_declarator_of(child).is_some() {
                    function(child, source, path, container, in_type, out);
                } else if !in_type {
                    variables(child, source, path, container, out);
                }
            }
            _ => {}
        }
    }
}

/// `#include "util.h"` 與 `#include <a/b.h>`。
///
/// 引號版相對於發出 include 的檔案，角括號版從專案根算起——後者對不上時
/// 解析階段還會試結尾比對，`<mylib/foo.h>` 因此找得到 `include/mylib/foo.h`。
///
/// `local` 記的是 include 的原文而不是某個名字：C 沒有「這一行引入了哪個
/// 名字」這回事，整個 header 的符號都看得見。這一欄只是主鍵的一部分。
fn include(node: Node<'_>, source: &str, out: &mut FileParse) {
    let Some(path_node) = node.child_by_field_name("path") else {
        return;
    };
    let spec = ts::text(path_node, source)
        .trim_matches(|c| c == '"' || c == '<' || c == '>')
        .to_string();
    if spec.is_empty() {
        return;
    }

    let target = if path_node.kind() == "string_literal" {
        ImportTarget::Relative(spec.clone())
    } else {
        ImportTarget::Rooted(spec.split('/').map(str::to_string).collect())
    };

    out.imports.push(Import {
        local: spec,
        target,
        line: ts::line_of(node),
    });
}

/// `#define MAX 64` 與 `#define SQUARE(x) ((x)*(x))`。
fn macro_symbol(
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
    // `#ifndef UTIL_H / #define UTIL_H` 的那一個只是重複引入的開關，不是
    // 任何人會查的東西。沒有值的物件巨集一律當成它。
    if kind == Kind::Const && node.child_by_field_name("value").is_none() {
        return;
    }

    common::push(
        node,
        path,
        Declaration {
            kind,
            name,
            container,
            signature: common::signature(node, source, &["value"], &[]),
            // 巨集就地展開，沒有「另一半」。
            has_body: true,
            docstring: docs(node, source),
        },
        out,
    );
}

/// `namespace a { ... }`、`namespace a::b { ... }`、匿名命名空間。
fn namespace(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let mut nested = container.to_vec();
    if let Some(name) = node.child_by_field_name("name") {
        // `namespace a::b` 的名字是一個節點，裡面兩段各算一層容器。
        for segment in ts::text(name, source).split("::") {
            let segment = segment.trim();
            if !segment.is_empty() {
                nested.push(segment.to_string());
            }
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        walk(body, source, path, &nested, false, out);
    }
}

/// `class`、`struct`、`union`、`enum`。
///
/// 沒有名字的（`typedef struct { ... } Foo;` 裡那個）由 [`typedef`] 處理，
/// 只有它知道要叫什麼。
fn type_decl(
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
    // 沒有本體的是前向宣告（`class Fwd;`），真正的定義在別處。
    let moniker = push(
        node,
        source,
        path,
        Symbol {
            kind,
            name,
            container,
            has_body: common::has_body(node, &["body"]),
        },
        out,
    );

    let mut found = Vec::new();
    // 基底類別是這個型別最實在的依賴。那個節點沒有欄位名，照種類找。
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "base_class_clause" {
            common::gather_types(child, source, TYPES, &[], &mut found);
        }
    }
    // 欄位名不收，但欄位的型別要收。
    if let Some(body) = node.child_by_field_name("body") {
        gather_field_types(body, source, &mut found);
    }
    common::emit_types(&moniker, found, out);

    let mut nested = container.to_vec();
    nested.push(name.to_string());
    if let Some(body) = node.child_by_field_name("body") {
        walk(body, source, path, &nested, true, out);
    }
}

/// 本體裡資料欄位的型別。
fn gather_field_types(body: Node<'_>, source: &str, found: &mut Vec<(String, u32)>) {
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if child.kind() != "field_declaration" || function_declarator_of(child).is_some() {
            continue;
        }
        if let Some(annotation) = child.child_by_field_name("type") {
            common::gather_types(annotation, source, TYPES, &[], found);
        }
    }
}

/// `typedef struct { ... } Size;`、`typedef struct node node_t;`、
/// `typedef int (*cmp_fn)(const void*, const void*);`
///
/// 匿名的結構借用 typedef 的名字直接記成結構——C 裡那就是宣告結構的寫法。
/// 有名字的結構則各自成立，typedef 只是它的別名。
fn typedef(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let Some(declarator) = node.child_by_field_name("declarator") else {
        return;
    };
    let Some(named) = declarator_name(declarator, source) else {
        return;
    };
    let inner = node.child_by_field_name("type");

    let anonymous = inner.filter(|t| {
        matches!(
            t.kind(),
            "struct_specifier" | "union_specifier" | "enum_specifier"
        ) && t.child_by_field_name("name").is_none()
            && t.child_by_field_name("body").is_some()
    });

    if let Some(owner) = anonymous {
        let kind = if owner.kind() == "enum_specifier" {
            Kind::Enum
        } else {
            Kind::Struct
        };
        let moniker = push(
            node,
            source,
            path,
            Symbol {
                kind,
                name: &named.name,
                container,
                has_body: true,
            },
            out,
        );

        let mut found = Vec::new();
        if let Some(body) = owner.child_by_field_name("body") {
            gather_field_types(body, source, &mut found);
        }
        common::emit_types(&moniker, found, out);

        // 本體裡可能還有東西：C 的結構常常拿函數指標當方法表。
        if let Some(body) = owner.child_by_field_name("body") {
            let mut nested = container.to_vec();
            nested.push(named.name.clone());
            walk(body, source, path, &nested, true, out);
        }
        return;
    }

    // `typedef struct TSNode { ... } TSNode;` 的兩個名字指的是同一個東西。
    // 各記一次會讓每一處 `TSNode` 都有兩個候選，然後全部變成有歧義。
    let same_name = inner
        .and_then(|t| common::field_text(t, "name", source))
        .is_some_and(|inner_name| inner_name == named.name);

    // 有名字的結構本身也是一個宣告。
    if let Some(inner) = inner.filter(|t| t.child_by_field_name("name").is_some()) {
        let kind = match inner.kind() {
            "enum_specifier" => Some(Kind::Enum),
            "struct_specifier" | "union_specifier" => Some(Kind::Struct),
            "class_specifier" => Some(Kind::Class),
            _ => None,
        };
        if let Some(kind) = kind {
            type_decl(inner, source, path, container, kind, out);
        }
    }

    if same_name {
        return;
    }

    let moniker = push(
        node,
        source,
        path,
        Symbol {
            kind: Kind::TypeAlias,
            name: &named.name,
            container,
            has_body: true,
        },
        out,
    );
    if let Some(inner) = inner {
        let mut found = Vec::new();
        common::gather_types(inner, source, TYPES, &[], &mut found);
        common::emit_types(&moniker, found, out);
    }
}

/// `using Alias = std::vector<Widget>;`
fn alias(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    let Some(name) = common::field_text(node, "name", source) else {
        return;
    };
    let moniker = push(
        node,
        source,
        path,
        Symbol {
            kind: Kind::TypeAlias,
            name,
            container,
            has_body: true,
        },
        out,
    );

    if let Some(target) = node.child_by_field_name("type") {
        let mut found = Vec::new();
        common::gather_types(target, source, TYPES, &[], &mut found);
        common::emit_types(&moniker, found, out);
    }
}

/// 函數：宣告與定義走同一條路，差別只在有沒有本體。
///
/// 宣告器上寫出來的範圍（`void a::W::draw()` 的 `a::W`）接在容器後面，限定名
/// 才會跟 header 裡那一份一致。
fn function(
    node: Node<'_>,
    source: &str,
    path: &str,
    container: &[String],
    in_type: bool,
    out: &mut FileParse,
) {
    let Some(declarator) = function_declarator_of(node) else {
        return;
    };
    let Some(inner) = declarator.child_by_field_name("declarator") else {
        return;
    };
    let Some(named) = declarator_name(inner, source) else {
        return;
    };

    let mut scope = container.to_vec();
    scope.extend(named.scopes.iter().cloned());

    // 寫在型別裡的是方法；寫在外面的要看範圍——`a::b::free_fn` 的範圍是命名
    // 空間，`a::W::draw` 的是類別。分得出來的只有命名慣例。
    let kind = if in_type || named.scopes.last().is_some_and(|s| looks_like_type(s)) {
        Kind::Method
    } else {
        Kind::Function
    };
    let moniker = push(
        node,
        source,
        path,
        Symbol {
            kind,
            name: &named.name,
            container: &scope,
            has_body: common::has_body(node, &["body"]),
        },
        out,
    );

    // 型別只看簽名：回傳型別與參數。本體裡的是實作細節。
    let mut found = Vec::new();
    if let Some(returns) = node.child_by_field_name("type") {
        common::gather_types(returns, source, TYPES, &[], &mut found);
    }
    if let Some(parameters) = declarator.child_by_field_name("parameters") {
        common::gather_types(parameters, source, TYPES, &[], &mut found);
    }
    common::emit_types(&moniker, found, out);

    let Some(body) = node.child_by_field_name("body") else {
        return;
    };

    let mut bindings = Bindings::new();
    // `this->draw()` 指的是所屬型別的方法，而所屬型別就寫在範圍裡。
    if let Some(owner) = scope.last().filter(|s| looks_like_type(s)) {
        bindings.insert("this", owner);
    }
    if let Some(parameters) = declarator.child_by_field_name("parameters") {
        bind_parameters(parameters, source, &mut bindings);
    }
    collect_calls(body, source, &moniker, &mut bindings, out);
}

/// 檔案或命名空間層級的變數。
///
/// `extern int x;` 只是說「別處有這個東西」，留下來會跟真正的定義變成兩個
/// 同名符號。
fn variables(node: Node<'_>, source: &str, path: &str, container: &[String], out: &mut FileParse) {
    if ts::text(node, source).trim_start().starts_with("extern") {
        return;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !is_declarator(child) {
            continue;
        }
        let Some(named) = declarator_name(child, source) else {
            continue;
        };
        let moniker = push(
            node,
            source,
            path,
            Symbol {
                kind: Kind::Const,
                name: &named.name,
                container,
                has_body: true,
            },
            out,
        );

        let mut found = Vec::new();
        if let Some(annotation) = node.child_by_field_name("type") {
            common::gather_types(annotation, source, TYPES, &[], &mut found);
        }
        common::emit_types(&moniker, found, out);
    }
}

/// 這個節點是宣告器而不是型別或修飾字。
fn is_declarator(node: Node<'_>) -> bool {
    matches!(node.kind(), "identifier" | "init_declarator") || WRAPPERS.contains(&node.kind())
}

/// 這一族要記下的一個符號。簽名與文件註解由 [`push`] 統一取。
struct Symbol<'a> {
    kind: Kind,
    name: &'a str,
    container: &'a [String],
    has_body: bool,
}

/// 收下一個符號，補上這一族的簽名與註解取法。
fn push(
    node: Node<'_>,
    source: &str,
    path: &str,
    symbol: Symbol<'_>,
    out: &mut FileParse,
) -> String {
    common::push(
        node,
        path,
        Declaration {
            kind: symbol.kind,
            name: symbol.name,
            container: symbol.container,
            signature: common::signature(node, source, &["body"], &[';', '=', '{']),
            has_body: symbol.has_body,
            docstring: docs(node, source),
        },
        out,
    )
}

fn docs(node: Node<'_>, source: &str) -> Option<String> {
    ts::leading_line_comments(node, source, "comment", DOC_PREFIXES, DOC_SKIP)
}

/// 這個名字看起來是型別而不是命名空間。
///
/// C++ 的語法樹分不出 `a::b::f` 裡哪一段是命名空間、哪一段是類別——兩者在
/// 節點上都是 `namespace_identifier`。慣例倒是一致：命名空間小寫（`std`、
/// `llvm`、`detail`），類別大寫開頭。猜錯的代價只是種類記成函數而不是方法。
fn looks_like_type(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

/// 宣告器裡的 `function_declarator`，外面包著的指標與括號都剝掉。
fn function_declarator_of(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = node.child_by_field_name("declarator")?;
    loop {
        if current.kind() == "function_declarator" {
            return Some(current);
        }
        if !WRAPPERS.contains(&current.kind()) {
            return None;
        }
        current = match current.child_by_field_name("declarator") {
            Some(inner) => inner,
            // 括號裡的宣告器沒有欄位名。
            None => current.named_child(0)?,
        };
    }
}

/// 宣告器上寫出來的名字與範圍。
struct Named {
    /// `void a::W::draw()` 的 `a`、`W`。
    scopes: Vec<String>,
    name: String,
}

/// 剝開宣告器，取出名字與它前面寫出來的範圍。
fn declarator_name(node: Node<'_>, source: &str) -> Option<Named> {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" => Some(Named {
            scopes: Vec::new(),
            name: ts::text(node, source).to_string(),
        }),
        // `~Widget` 與 `operator==`：原文就是名字。
        "destructor_name" | "operator_name" => Some(Named {
            scopes: Vec::new(),
            name: ts::collapse_whitespace(ts::text(node, source)),
        }),
        // `a::b::draw`：範圍逐層往內收。
        "qualified_identifier" => {
            let scope = node.child_by_field_name("scope")?;
            let inner = node.child_by_field_name("name")?;
            let mut named = declarator_name(inner, source)?;
            let mut scopes = vec![base_name(scope, source)];
            scopes.append(&mut named.scopes);
            Some(Named {
                scopes,
                name: named.name,
            })
        }
        // `Box<T>::get` 的 `Box<T>`、`f<int>` 的 `f`。
        "template_function" | "template_type" | "template_method" => {
            declarator_name(node.child_by_field_name("name")?, source)
        }
        "function_declarator" => declarator_name(node.child_by_field_name("declarator")?, source),
        // `int (*cmp_fn)(...)` 的括號裡沒有欄位名，取第一個具名子節點。
        "parenthesized_declarator" => declarator_name(node.named_child(0)?, source),
        kind if WRAPPERS.contains(&kind) => {
            declarator_name(node.child_by_field_name("declarator")?, source)
        }
        _ => None,
    }
}

/// 範圍那一段的名字，泛型參數去掉。
fn base_name(node: Node<'_>, source: &str) -> String {
    match node.child_by_field_name("name") {
        Some(inner) => ts::text(inner, source).to_string(),
        None => ts::collapse_whitespace(ts::text(node, source)),
    }
}

/// 記下參數的型別，`w.draw()` 才知道 `w` 是什麼。
fn bind_parameters(parameters: Node<'_>, source: &str, bindings: &mut Bindings) {
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if parameter.kind() != "parameter_declaration" {
            continue;
        }
        let Some(type_node) = parameter.child_by_field_name("type") else {
            continue;
        };
        let Some(declarator) = parameter.child_by_field_name("declarator") else {
            continue;
        };
        if let Some(named) = declarator_name(declarator, source) {
            bindings.insert(&named.name, &type_base_name(type_node, source));
        }
    }
}

/// 記下區域變數的型別。
///
/// `auto` 沒有寫出型別，但 `auto w = Widget();` 的右邊寫了。
fn bind_declaration(node: Node<'_>, source: &str, bindings: &mut Bindings) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    let declared = type_base_name(type_node, source);

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !is_declarator(child) {
            continue;
        }
        let Some(named) = declarator_name(child, source) else {
            continue;
        };
        let inferred = if type_node.kind() == "placeholder_type_specifier" {
            child
                .child_by_field_name("value")
                .and_then(|v| constructed_type(v, source))
        } else {
            None
        };
        bindings.insert(&named.name, inferred.as_deref().unwrap_or(&declared));
    }
}

/// 初始式建出來的型別：`Widget()`、`new Widget()`。
fn constructed_type(value: Node<'_>, source: &str) -> Option<String> {
    match value.kind() {
        "call_expression" => {
            let function = value.child_by_field_name("function")?;
            matches!(function.kind(), "identifier" | "template_function")
                .then(|| base_name(function, source))
        }
        "new_expression" => Some(base_name(value.child_by_field_name("type")?, source)),
        _ => None,
    }
}

/// 型別運算式的基底名字。`Widget*`、`const Widget&`、`Box<T>` 都是同一個型別。
fn type_base_name(node: Node<'_>, source: &str) -> String {
    match node.kind() {
        "template_type" | "qualified_identifier" => node
            .child_by_field_name("name")
            .map(|n| type_base_name(n, source))
            .unwrap_or_else(|| ts::collapse_whitespace(ts::text(node, source))),
        _ => ts::collapse_whitespace(ts::text(node, source)),
    }
}

/// 走遍節點底下所有的呼叫，記到 `from` 名下。
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

    // `new Widget()` 的目標是型別，不是函數。
    if node.kind() == "new_expression"
        && let Some(created) = node.child_by_field_name("type")
    {
        let mut found = Vec::new();
        common::gather_types(created, source, TYPES, &[], &mut found);
        common::emit_types(from, found, out);
    }

    let opens_block = matches!(node.kind(), "compound_statement" | "lambda_expression");
    if opens_block {
        bindings.enter();
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, source, from, bindings, out);
    }

    // 綁定在初始式走完之後才生效：`auto x = x.wrap();` 右邊的 `x` 是舊的。
    if node.kind() == "declaration" {
        bind_declaration(node, source, bindings);
    }

    if opens_block {
        bindings.leave();
    }
}

/// 被呼叫者在原始碼裡的寫法。
///
/// 接收者的型別查得到就改寫成 `Type::method`，解析階段可以拿限定名去驗證。
/// 查不到就保留原文，寫法裡的句點會讓解析階段知道這是對某個值呼叫方法。
fn callee_name(call: Node<'_>, source: &str, bindings: &Bindings) -> Option<String> {
    callee_name_of(call.child_by_field_name("function")?, source, bindings)
}

fn callee_name_of(function: Node<'_>, source: &str, bindings: &Bindings) -> Option<String> {
    match function.kind() {
        "identifier" => Some(ts::text(function, source).to_string()),
        // `a::b::f()`、`Widget::make()`
        "qualified_identifier" => {
            let named = declarator_name(function, source)?;
            let mut parts = named.scopes;
            parts.push(named.name);
            Some(parts.join("::"))
        }
        // `w.draw()`、`p->draw()`、`this->draw()`
        "field_expression" => {
            let field = function.child_by_field_name("field")?;
            let method = ts::text(field, source);
            let argument = function.child_by_field_name("argument")?;
            let receiver = ts::collapse_whitespace(ts::text(argument, source));

            if matches!(argument.kind(), "identifier" | "this")
                && let Some(type_name) = bindings.get(&receiver)
            {
                return Some(format!("{type_name}::{method}"));
            }
            Some(format!("{receiver}.{method}"))
        }
        // `max_of<int>()`
        "template_function" => {
            callee_name_of(function.child_by_field_name("name")?, source, bindings)
        }
        // 函數指標的呼叫寫不出名字。
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_c(src: &str) -> FileParse {
        CExtractor.extract("src/a.c", src)
    }

    fn parse_cpp(src: &str) -> FileParse {
        CppExtractor.extract("src/a.cpp", src)
    }

    fn parse_cuda(src: &str) -> FileParse {
        CudaExtractor.extract("src/a.cu", src)
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
        let p = parse_c(
            "int add(int a, int b);\n\
             struct point { int x; };\n\
             union u { int a; };\n\
             enum colour { RED };\n\
             int counter = 0;\n",
        );

        assert_eq!(names(&p), ["add", "point", "u", "colour", "counter"]);
        let kinds: Vec<Kind> = p.symbols.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Function,
                Kind::Struct,
                Kind::Struct,
                Kind::Enum,
                Kind::Const
            ]
        );
    }

    /// 宣告沒有本體，定義有——兩者是同一件東西的兩面。
    #[test]
    fn a_prototype_has_no_body_but_a_definition_does() {
        let p = parse_c("int add(int a, int b);\nint add(int a, int b) { return a + b; }\n");

        let bodies: Vec<bool> = p.symbols.iter().map(|s| s.has_body).collect();
        assert_eq!(bodies, [false, true]);
    }

    /// header 幾乎都整份包在引入保護裡，不穿進去就一個符號都抽不到。
    #[test]
    fn declarations_inside_an_include_guard_are_found() {
        let p = parse_c("#ifndef UTIL_H\n#define UTIL_H\nint add(int a, int b);\n#endif\n");

        assert_eq!(names(&p), ["add"], "引入保護本身不該留成符號");
    }

    #[test]
    fn conditional_branches_are_both_walked() {
        let p = parse_c("#ifdef _WIN32\nvoid win(void);\n#else\nvoid posix(void);\n#endif\n");

        assert_eq!(names(&p), ["win", "posix"]);
    }

    #[test]
    fn macros_are_symbols() {
        let p = parse_c("#define MAX_LEN 64\n#define SQUARE(x) ((x)*(x))\n");

        assert_eq!(names(&p), ["MAX_LEN", "SQUARE"]);
        assert_eq!(p.symbols[0].kind, Kind::Const);
        assert_eq!(p.symbols[1].kind, Kind::Function, "函數巨集用起來就是函數");
    }

    /// 匿名的結構借用 typedef 的名字，那是 C 宣告結構的常見寫法。
    #[test]
    fn an_anonymous_struct_takes_the_typedef_name() {
        let p = parse_c("typedef struct { int w; int h; } size2;\n");

        assert_eq!(names(&p), ["size2"]);
        assert_eq!(p.symbols[0].kind, Kind::Struct);
        assert!(p.symbols[0].has_body);
    }

    /// 有名字的結構各自成立，typedef 只是它的別名。
    #[test]
    fn a_named_struct_and_its_alias_are_both_kept() {
        let p = parse_c("typedef struct node { int v; } node_t;\n");

        assert_eq!(names(&p), ["node", "node_t"]);
        assert_eq!(p.symbols[1].kind, Kind::TypeAlias);
    }

    /// `typedef struct X { ... } X;` 的兩個名字是同一個東西。
    ///
    /// 各記一次的話，每一處 `X` 都有兩個候選，型別引用會整批變成有歧義。
    /// 這是在真實的 C 專案上量出來的：修掉之後待解析從 781 降到 301。
    #[test]
    fn a_typedef_that_repeats_the_struct_name_is_one_symbol() {
        let p = parse_c("typedef struct TSNode { int v; } TSNode;\n");

        assert_eq!(names(&p), ["TSNode"]);
        assert_eq!(p.symbols[0].kind, Kind::Struct);
    }

    /// C 的結構常常拿函數指標當方法表，`lexer->advance()` 就是在呼叫它。
    #[test]
    fn a_function_pointer_field_is_a_method_of_its_struct() {
        let p = parse_c("typedef struct { void (*advance)(int); } TSLexer;\n");

        assert_eq!(names(&p), ["TSLexer", "TSLexer::advance"]);
        assert_eq!(p.symbols[1].kind, Kind::Method);
    }
    #[test]
    fn a_function_pointer_typedef_is_an_alias() {
        let p = parse_c("typedef int (*cmp_fn)(const void*, const void*);\n");

        assert_eq!(names(&p), ["cmp_fn"]);
        assert_eq!(p.symbols[0].kind, Kind::TypeAlias);
    }

    /// `extern int x;` 只是說別處有這個東西，留下來會跟定義撞成兩個符號。
    #[test]
    fn an_extern_declaration_is_not_a_symbol() {
        let p = parse_c("extern int counter;\nint counter = 0;\n");

        assert_eq!(names(&p), ["counter"]);
    }

    #[test]
    fn a_pointer_return_type_does_not_hide_the_name() {
        let p = parse_c("struct point *make_point(void) { return 0; }\n");

        assert_eq!(names(&p), ["make_point"]);
        assert_eq!(p.symbols[0].kind, Kind::Function);
    }

    #[test]
    fn calls_are_recorded_against_the_caller() {
        let p = parse_c(
            "int add(int a, int b) { return a + b; }\nint use(void) { return add(1, 2); }\n",
        );

        assert_eq!(calls(&p, "use"), ["add"]);
    }

    /// 函數指標的呼叫寫不出名字，不硬掰一個。
    #[test]
    fn a_call_through_a_function_pointer_is_not_named() {
        let p = parse_c("void use(int (*fp)(int)) { (*fp)(1); }\n");

        assert!(calls(&p, "use").is_empty());
    }

    #[test]
    fn a_struct_records_the_types_of_its_fields() {
        let p = parse_c("struct box { struct point origin; };\n");

        assert_eq!(refs_by(&p, "box", Rel::UsesType), ["point"]);
    }

    // ── C++ ────────────────────────────────────────────────

    /// 命名空間與類別都算容器，命名空間本身不留成符號。
    #[test]
    fn namespaces_qualify_but_do_not_become_symbols() {
        let p = parse_cpp("namespace a { namespace b { class W { void draw(); }; } }\n");

        assert_eq!(names(&p), ["a::b::W", "a::b::W::draw"]);
    }

    #[test]
    fn a_nested_namespace_specifier_counts_as_two_containers() {
        let p = parse_cpp("namespace a::b { void free_fn(); }\n");

        assert_eq!(names(&p), ["a::b::free_fn"]);
        assert_eq!(p.symbols[0].kind, Kind::Function, "命名空間裡的是函數");
    }

    /// 三種寫法算出同一個限定名，`.cpp` 的定義才接得上 header 的宣告。
    #[test]
    fn an_out_of_line_definition_keeps_the_declarations_qualified_name() {
        let inside = parse_cpp("namespace a { void W::draw() const {} }\n");
        let outside = parse_cpp("void a::W::draw() const {}\n");

        assert_eq!(names(&inside), ["a::W::draw"]);
        assert_eq!(names(&outside), ["a::W::draw"]);
        assert_eq!(outside.symbols[0].kind, Kind::Method);
    }

    #[test]
    fn members_declared_in_a_class_are_methods() {
        let p = parse_cpp(
            "class W {\npublic:\n  W();\n  ~W();\n  virtual int area() = 0;\n  int inline_m() { return 1; }\n};\n",
        );

        assert_eq!(names(&p), ["W", "W::W", "W::~W", "W::area", "W::inline_m"]);
        let bodies: Vec<bool> = p.symbols.iter().map(|s| s.has_body).collect();
        assert_eq!(bodies, [true, false, false, false, true]);
        assert!(p.symbols[1..].iter().all(|s| s.kind == Kind::Method));
    }

    #[test]
    fn a_forward_declaration_has_no_body() {
        let p = parse_cpp("class Fwd;\nclass Fwd { int x; };\n");

        let bodies: Vec<bool> = p.symbols.iter().map(|s| s.has_body).collect();
        assert_eq!(bodies, [false, true]);
    }

    #[test]
    fn base_classes_are_type_references() {
        let p = parse_cpp("class D : public Base {};\n");

        assert_eq!(refs_by(&p, "D", Rel::UsesType), ["Base"]);
    }

    #[test]
    fn an_alias_declaration_is_a_type_alias() {
        let p = parse_cpp("using Alias = std::vector<Widget>;\n");

        assert_eq!(names(&p), ["Alias"]);
        assert_eq!(p.symbols[0].kind, Kind::TypeAlias);
        assert_eq!(refs_by(&p, "Alias", Rel::UsesType), ["vector", "Widget"]);
    }

    #[test]
    fn a_template_declaration_does_not_hide_what_is_inside() {
        let p = parse_cpp(
            "template<typename T> class Box { T get() const; };\n\
             template<typename T> T Box<T>::get() const { return value; }\n",
        );

        assert_eq!(names(&p), ["Box", "Box::get", "Box::get"]);
    }

    #[test]
    fn an_extern_c_block_does_not_add_a_container() {
        let p = parse_cpp("extern \"C\" {\nvoid c_api(void);\n}\n");

        assert_eq!(names(&p), ["c_api"]);
    }

    /// 接收者的型別查得到就改寫成限定名，解析階段才驗證得了。
    #[test]
    fn a_receiver_with_a_known_type_becomes_a_qualified_call() {
        let p = parse_cpp("void use() { Widget w; w.draw(); }\n");

        assert_eq!(calls(&p, "use"), ["Widget::draw"]);
    }

    #[test]
    fn a_pointer_receiver_is_resolved_the_same_way() {
        let p = parse_cpp("void use(Widget* p) { p->draw(); }\n");

        assert_eq!(calls(&p, "use"), ["Widget::draw"]);
    }

    #[test]
    fn auto_takes_the_type_from_what_is_constructed() {
        let p = parse_cpp("void use() { auto w = Widget(); w.draw(); }\n");

        assert!(calls(&p, "use").contains(&"Widget::draw".to_string()));
    }

    #[test]
    fn new_records_the_type_and_the_receiver_after_it() {
        let p = parse_cpp("void use() { Widget* p = new Widget(); p->draw(); }\n");

        assert_eq!(refs_by(&p, "use", Rel::UsesType), ["Widget"]);
        assert_eq!(calls(&p, "use"), ["Widget::draw"]);
    }

    /// `this->draw()` 指的是所屬型別的方法，而所屬型別寫在範圍裡。
    #[test]
    fn this_resolves_to_the_enclosing_type() {
        let p = parse_cpp("void a::W::draw() const { this->area(); }\n");

        assert_eq!(calls(&p, "draw"), ["W::area"]);
    }

    #[test]
    fn a_qualified_call_keeps_its_scopes() {
        let p = parse_cpp("void use() { a::b::free_fn(); W::make(); }\n");

        assert_eq!(calls(&p, "use"), ["a::b::free_fn", "W::make"]);
    }

    #[test]
    fn template_arguments_are_dropped_from_the_callee() {
        let p = parse_cpp("void use() { max_of<int>(1, 2); }\n");

        assert_eq!(calls(&p, "use"), ["max_of"]);
    }

    /// 型別查不到的接收者保留原文，句點讓解析階段知道那是對值的呼叫。
    #[test]
    fn an_unknown_receiver_keeps_the_written_form() {
        let p = parse_cpp("void use() { other.draw(); }\n");

        assert_eq!(calls(&p, "use"), ["other.draw"]);
    }

    #[test]
    fn a_block_scoped_binding_does_not_leak_outwards() {
        let p = parse_cpp("void use() { { Widget w; } w.draw(); }\n");

        assert_eq!(calls(&p, "use"), ["w.draw"]);
    }

    #[test]
    fn includes_become_imports() {
        let p = parse_cpp("#include \"util.h\"\n#include <a/b.h>\n");

        let targets: Vec<&ImportTarget> = p.imports.iter().map(|i| &i.target).collect();
        assert_eq!(
            targets,
            [
                &ImportTarget::Relative("util.h".to_string()),
                &ImportTarget::Rooted(vec!["a".to_string(), "b.h".to_string()]),
            ]
        );
        assert_eq!(p.imports[0].local, "util.h");
    }

    #[test]
    fn a_doc_comment_above_a_declaration_is_kept() {
        let p = parse_c("/** 把兩個數字加起來。 */\nint add(int a, int b);\n");

        assert!(
            p.symbols[0]
                .docstring
                .as_deref()
                .is_some_and(|d| d.contains("加起來")),
            "{:?}",
            p.symbols[0].docstring
        );
    }

    #[test]
    fn a_signature_stops_before_the_body() {
        let p = parse_c("int add(int a, int b) { return a + b; }\n");

        assert_eq!(
            p.symbols[0].signature.as_deref(),
            Some("int add(int a, int b)")
        );
    }

    #[test]
    fn an_anonymous_enum_also_takes_the_typedef_name() {
        let p = parse_c("typedef enum { A, B } mode;\n");

        assert_eq!(names(&p), ["mode"]);
        assert_eq!(p.symbols[0].kind, Kind::Enum);
    }

    #[test]
    fn auto_takes_the_type_from_new_as_well() {
        let p = parse_cpp("void use() { auto p = new Widget(); p->draw(); }\n");

        assert_eq!(calls(&p, "use"), ["Widget::draw"]);
    }

    /// 泛型參數的接收者收斂到基底型別，`Box<int>` 與 `Box<T>` 是同一個。
    #[test]
    fn a_generic_parameter_binds_to_its_base_type() {
        let p = parse_cpp("void use(Box<int> b) { b.get(); }\n");

        assert_eq!(calls(&p, "use"), ["Box::get"]);
    }

    #[test]
    fn a_template_specialisation_keeps_the_plain_name() {
        let p = parse_cpp("template<> void f<int>(int v) {}\n");

        assert_eq!(names(&p), ["f"]);
    }
    // ── CUDA ───────────────────────────────────────────────

    #[test]
    fn cuda_qualifiers_do_not_hide_the_function() {
        let p = parse_cuda(
            "__device__ float sq(float x) { return x * x; }\n\
             __global__ void scale(float* y) { y[0] = sq(y[0]); }\n",
        );

        assert_eq!(names(&p), ["sq", "scale"]);
        assert_eq!(calls(&p, "scale"), ["sq"]);
    }

    /// kernel 啟動只是多掛一個 `kernel_call_syntax`，被呼叫者還在原位。
    #[test]
    fn a_kernel_launch_is_a_call() {
        let p = parse_cuda("void launch(float* y) { scale<<<1, 32>>>(y); }\n");

        assert_eq!(calls(&p, "launch"), ["scale"]);
    }

    #[test]
    fn cuda_reads_cpp_declarations_too() {
        let p = parse_cuda("namespace at { class Tensor { void fill(); }; }\n");

        assert_eq!(names(&p), ["at::Tensor", "at::Tensor::fill"]);
    }

    // ── 抽取器本身 ─────────────────────────────────────────

    #[test]
    fn the_three_languages_share_one_family() {
        assert_eq!(CExtractor.family(), CppExtractor.family());
        assert_eq!(CppExtractor.family(), CudaExtractor.family());
        assert_eq!(CExtractor.module_path("src/a.c"), "");
        assert_eq!(CppExtractor.module_path("src/a.cpp"), "");
        assert_eq!(CudaExtractor.module_path("src/a.cu"), "");
    }

    /// C 必須寫出接收者，C++ 與 CUDA 可以省略。
    #[test]
    fn only_the_cpp_side_has_an_implicit_receiver() {
        assert!(!CExtractor.implicit_receiver());
        assert!(CppExtractor.implicit_receiver());
        assert!(CudaExtractor.implicit_receiver());
    }

    #[test]
    fn a_file_with_syntax_errors_still_yields_symbols() {
        let p = parse_c("int add(int a) { return a; }\nint broken(\n");

        assert!(names(&p).contains(&"add"));
        assert!(!p.errors.is_empty(), "語法錯誤要回報");
    }

    #[test]
    fn an_empty_file_is_empty() {
        let p = parse_c("");

        assert!(p.is_empty());
        assert!(p.errors.is_empty());
    }
}
