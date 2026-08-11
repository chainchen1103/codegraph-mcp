//! Rust 抽取器。
//!
//! 用明確的遞迴走訪而不是 tree-sitter query（`.scm`）。原因：判斷
//! 一個 `function_item` 是 function 還是 method，取決於它的祖先是不是
//! `impl_item`／`trait_item`；容器名稱也要沿著祖先鏈累積（`a::b::c`）。
//! 這種帶上下文的走訪用宣告式 query 表達很彆扭，一次明確的走訪反而
//! 短、快、且好推理。

use tree_sitter::{Language, Node};

use super::super::ts;
use super::super::{Extractor, FileParse};
use crate::extract::moniker;
use crate::model::{Kind, RawSymbol};

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
            // 不放棄已抽到的符號：編輯到一半的檔案本來就常常是壞的，
            // 這時候能給出上半部的結構仍然有用。
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
    /// 祖先鏈上的名字（`["Store"]`、`["net", "http"]`），
    /// 用來組出 `Store::open` 這種限定名。
    container: Vec<String>,
    /// 直屬容器是不是型別（`impl` / `trait`）。
    ///
    /// **這個旗標決定 function 還是 method。** 不能只看「有沒有容器」——
    /// `mod tests { fn helper() }` 裡的 `helper` 是普通函數，不是方法。
    /// 把它標成 method 會讓「這個型別有哪些方法」的查詢混進一堆模組層函數。
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

/// 走訪一層，把找到的宣告收進 `out`。
fn walk(node: Node<'_>, source: &str, path: &str, scope: &Scope, out: &mut FileParse) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            // ---- 會產生符號、而且要往下走的容器 ----
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
            // ---- 只提供上下文、本身不是符號 ----
            "impl_item" => {
                // `impl Foo` 與 `impl Trait for Foo` 都取 `type`，
                // 也就是「方法掛在誰身上」。
                let name = child
                    .child_by_field_name("type")
                    .map(|n| type_base_name(n, source))
                    .unwrap_or_else(|| "impl".to_string());
                descend(child, source, path, &scope.child(&name, true), out);
            }
            // ---- 葉節點 ----
            "function_item" | "function_signature_item" => {
                let Some(name) = field_text(child, "name", source) else {
                    continue;
                };
                // **不往 body 裡走**：巢狀 fn 是實作細節，外面叫不到它，
                // 收進圖裡只會稀釋查詢結果。
                let kind = if scope.in_type {
                    Kind::Method
                } else {
                    Kind::Function
                };
                push(child, source, path, scope, kind, name, out);
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

/// 走進容器的 body。沒有 body（例如 `mod foo;`）就什麼都不做。
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

fn push(
    node: Node<'_>,
    source: &str,
    path: &str,
    scope: &Scope,
    kind: Kind,
    name: &str,
    out: &mut FileParse,
) {
    let start_line = ts::line_of(node);
    let qualified = scope.qualify(name);

    out.symbols.push(RawSymbol {
        moniker: moniker::build(path, kind, name, start_line),
        name: name.to_string(),
        qualified,
        kind,
        start_line,
        end_line: ts::end_line_of(node),
        signature: signature(node, source),
        docstring: ts::leading_line_comments(node, source, "line_comment", DOC_PREFIXES, DOC_SKIP),
    });
}

/// 宣告的簽名：從宣告開頭到 body 之前。
///
/// 只回宣告部分是為了**簽名**這個用途，不是為了省 token——完整 body
/// 由 `explore` 直接讀原始碼給出（DESIGN.md §5.1）。
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

/// 型別運算式剝到只剩基底名字。
///
/// `impl<T> Widget<T>`、`impl Display for Widget<u8>`、`impl crate::a::Widget`
/// 講的都是同一個型別。不剝掉泛型參數與模組路徑的話，同一個型別的方法
/// 會被拆成 `Widget<T>::new`、`Widget<u8>::fmt`、`crate::a::Widget::x`
/// 三組互不相干的限定名，「這個型別有哪些方法」就永遠問不到完整答案。
fn type_base_name(node: Node<'_>, source: &str) -> String {
    match node.kind() {
        // Widget<T> → Widget
        "generic_type" => node
            .child_by_field_name("type")
            .map(|n| type_base_name(n, source))
            .unwrap_or_else(|| ts::text(node, source).to_string()),
        // a::b::Widget → Widget
        "scoped_type_identifier" => node
            .child_by_field_name("name")
            .map(|n| ts::text(n, source).to_string())
            .unwrap_or_else(|| ts::text(node, source).to_string()),
        // &T / *const T / [T; N] 之類的包裝。
        // 欄位名依節點種類而異：參考與指標是 `type`，陣列是 `element`。
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
        // impl 區塊本身不是符號——它只是「這些方法掛在誰身上」的資訊。
        assert!(!names(&p).contains(&"impl"));
    }

    /// 泛型參數與模組路徑都要剝掉，否則同一個型別的方法會被拆成好幾組。
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

    /// `impl Trait for &T` / `for [T; N]` 這些包裝型別也要剝到基底名字。
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

    /// 匿名 impl（`impl Trait for (u8, u8)` 這種沒有基底名字的目標）
    /// 不能讓走訪爆掉，方法照樣要抽到。
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
        // 問「S 有哪些方法」比問「Display 有哪些實作」常見得多。
        assert!(names(&p).contains(&"S::fmt"), "{:?}", names(&p));
    }

    #[test]
    fn nested_modules_accumulate_qualified_names() {
        let p = parse("mod a {\n    mod b {\n        fn c() {}\n    }\n}\n");
        assert!(names(&p).contains(&"a::b::c"), "{:?}", names(&p));
    }

    /// 模組裡的函數是**函數**，不是方法。只有 impl／trait 底下的才是方法。
    /// 混淆的話，「這個型別有哪些方法」會混進一堆模組層的函數——
    /// `mod tests` 底下那幾十個測試就是最明顯的受害者。
    #[test]
    fn functions_in_modules_are_not_methods() {
        let p =
            parse("mod tests {\n    fn helper() {}\n}\nstruct S;\nimpl S {\n    fn m() {}\n}\n");
        assert_eq!(find(&p, "tests::helper").kind, Kind::Function);
        assert_eq!(find(&p, "S::m").kind, Kind::Method);
    }

    /// `impl` 區塊裡的模組（罕見但合法）不該把方法身分傳染下去。
    #[test]
    fn a_module_inside_a_type_resets_the_method_context() {
        let p = parse("trait T {\n    fn required(&self);\n}\nmod m {\n    fn plain() {}\n}\n");
        assert_eq!(find(&p, "T::required").kind, Kind::Method);
        assert_eq!(find(&p, "m::plain").kind, Kind::Function);
    }

    /// `mod foo;` 沒有 body，走訪不能因此爆掉。
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

    /// 巢狀 fn 是實作細節，外面叫不到——收進圖只會稀釋查詢結果。
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

    /// `#[derive(...)]` 長在文件註解與宣告之間。不跳過的話，
    /// 所有帶屬性的型別都會抓不到文件——也就是大部分的型別。
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

    /// 語法壞掉的檔案要回報問題，但**仍然交出抽得到的符號**。
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

    #[test]
    fn extraction_order_is_deterministic() {
        // 確定性排序是 prompt caching 命中率的前提（DESIGN.md §5.5）。
        let src = "fn b() {}\nfn a() {}\nstruct C;\n";
        let first: Vec<String> = parse(src).symbols.into_iter().map(|s| s.moniker).collect();
        let second: Vec<String> = parse(src).symbols.into_iter().map(|s| s.moniker).collect();
        assert_eq!(first, second);
        // 依原始碼出現順序，不是字母序。
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
