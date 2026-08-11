//! Rust 抽取器。
//!
//! 採用明確的遞迴走訪而非 tree-sitter query。函數與方法的區分取決於
//! 祖先節點，容器名稱也需要沿祖先鏈累積，這類帶上下文的判斷用宣告式
//! query 表達不便。

use tree_sitter::{Language, Node};

use super::super::ts;
use super::super::{Extractor, FileParse};
use crate::extract::moniker;
use crate::model::{Kind, RawRef, RawSymbol, Rel};

/// Rust 的文件註解前綴。
const DOC_PREFIXES: &[&str] = &["///", "//!"];

/// 夾在文件註解與宣告之間、不打斷註解的節點。
const DOC_SKIP: &[&str] = &["attribute_item"];

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
}

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

    fn qualify(&self, name: &str) -> String {
        if self.container.is_empty() {
            name.to_string()
        } else {
            format!("{}::{}", self.container.join("::"), name)
        }
    }
}

/// 走訪一層節點，把找到的宣告收進 `out`。
fn walk(node: Node<'_>, source: &str, path: &str, scope: &Scope, out: &mut FileParse) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "mod_item" => {
                let Some(name) = field_text(child, "name", source) else {
                    continue;
                };
                push(child, source, path, scope, Kind::Module, name, out);
                descend(child, source, path, &scope.child(name, false), out);
            }
            "trait_item" => {
                let Some(name) = field_text(child, "name", source) else {
                    continue;
                };
                push(child, source, path, scope, Kind::Trait, name, out);
                descend(child, source, path, &scope.child(name, true), out);
            }
            // impl 區塊本身不是符號，只提供方法所屬的型別。
            "impl_item" => {
                let name = child
                    .child_by_field_name("type")
                    .map(|n| type_base_name(n, source))
                    .unwrap_or_else(|| "impl".to_string());
                descend(child, source, path, &scope.child(&name, true), out);
            }
            // 不把本體裡的巢狀函數當成符號，它們無法從外部呼叫，但本體
            // 裡的呼叫仍要記錄下來。
            "function_item" | "function_signature_item" => {
                let Some(name) = field_text(child, "name", source) else {
                    continue;
                };
                let kind = if scope.in_type {
                    Kind::Method
                } else {
                    Kind::Function
                };
                let moniker = push(child, source, path, scope, kind, name, out);
                if let Some(body) = child.child_by_field_name("body") {
                    collect_calls(body, source, &moniker, out);
                }
            }
            "struct_item" => leaf(child, source, path, scope, Kind::Struct, out),
            "enum_item" => leaf(child, source, path, scope, Kind::Enum, out),
            "union_item" => leaf(child, source, path, scope, Kind::Struct, out),
            "type_item" => leaf(child, source, path, scope, Kind::TypeAlias, out),
            "const_item" | "static_item" => leaf(child, source, path, scope, Kind::Const, out),
            _ => {}
        }
    }
}

/// 走進容器的本體，沒有本體時不做任何事。
fn descend(node: Node<'_>, source: &str, path: &str, scope: &Scope, out: &mut FileParse) {
    if let Some(body) = node.child_by_field_name("body") {
        walk(body, source, path, scope, out);
    }
}

fn leaf(node: Node<'_>, source: &str, path: &str, scope: &Scope, kind: Kind, out: &mut FileParse) {
    if let Some(name) = field_text(node, "name", source) {
        push(node, source, path, scope, kind, name, out);
    }
}

/// 走遍節點底下所有的呼叫，記到 `from` 名下。
///
/// 只記下被呼叫者在原始碼裡寫成什麼樣子，不做任何解析。巢狀函數與
/// 閉包裡的呼叫都算在外層函數頭上，它們是同一段邏輯的一部分。
fn collect_calls(node: Node<'_>, source: &str, from: &str, out: &mut FileParse) {
    if node.kind() == "call_expression"
        && let Some(name) = callee_name(node, source)
    {
        out.refs.push(RawRef {
            from: from.to_string(),
            name,
            rel: Rel::Calls,
            line: ts::line_of(node),
        });
    }

    // 依原始碼順序遞迴，輸出才與檔案內容一致。
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, source, from, out);
    }
}

/// 被呼叫者在原始碼裡的寫法。
///
/// 保留原文，例如 `Store::open` 或 `store.stats`。解析階段依賴這兩點：
/// 路徑越完整越容易對到唯一的符號；而寫法裡有沒有句點，決定了這是
/// 對接收者呼叫方法，還是直接呼叫一個函數。
fn callee_name(call: Node<'_>, source: &str) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    callee_name_of(function, source)
}

fn callee_name_of(function: Node<'_>, source: &str) -> Option<String> {
    match function.kind() {
        // foo() 與 a::b::c()
        "identifier" | "scoped_identifier" => Some(ts::text(function, source).to_string()),
        // x.method()：連同接收者一起記下來。
        "field_expression" => {
            let field = function.child_by_field_name("field")?;
            let receiver = function
                .child_by_field_name("value")
                .map(|v| ts::collapse_whitespace(ts::text(v, source)))
                .unwrap_or_default();
            Some(format!("{receiver}.{}", ts::text(field, source)))
        }
        // foo::<T>()
        "generic_function" => function
            .child_by_field_name("function")
            .and_then(|inner| callee_name_of(inner, source)),
        _ => None,
    }
}

/// 收下一個符號，回傳它的 moniker。
fn push(
    node: Node<'_>,
    source: &str,
    path: &str,
    scope: &Scope,
    kind: Kind,
    name: &str,
    out: &mut FileParse,
) -> String {
    let start_line = ts::line_of(node);
    let qualified = scope.qualify(name);
    let moniker = moniker::build(path, kind, name, start_line);

    out.symbols.push(RawSymbol {
        moniker: moniker.clone(),
        name: name.to_string(),
        qualified,
        kind,
        start_line,
        end_line: ts::end_line_of(node),
        signature: signature(node, source),
        docstring: ts::leading_line_comments(node, source, "line_comment", DOC_PREFIXES, DOC_SKIP),
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

    fn refs_of(p: &FileParse, from_name: &str) -> Vec<String> {
        let from = p
            .symbols
            .iter()
            .find(|s| s.name == from_name || s.qualified == from_name)
            .unwrap_or_else(|| panic!("找不到 {from_name}"));
        p.refs
            .iter()
            .filter(|r| r.from == from.moniker)
            .map(|r| r.name.clone())
            .collect()
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

    /// 方法呼叫連同接收者一起記下。句點的存在讓解析階段知道這是對某個
    /// 值呼叫方法，而不是直接呼叫函數。
    #[test]
    fn method_calls_record_the_receiver_too() {
        let p = parse("fn caller(s: Store) {\n    s.open();\n    self.close();\n}\n");
        assert_eq!(refs_of(&p, "caller"), vec!["s.open", "self.close"]);
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
