//! JavaScript 抽取器。
//!
//! 走訪邏輯完全共用 [`super::typescript`]：兩者的語法樹在抽取用得到的
//! 部分是同一套節點，TypeScript 多出來的型別標註在 JavaScript 的樹裡
//! 不會出現，對應的分支自然不觸發。抄一份只會多一個要同步維護的地方。
//!
//! JSX 內建在同一份文法裡，`.jsx` 不需要另外的 `Language`。

use tree_sitter::Language;

use super::super::{Extractor, FileParse};
use super::typescript;

pub struct JavaScriptExtractor;

impl Extractor for JavaScriptExtractor {
    fn language(&self) -> &'static str {
        "javascript"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["js", "jsx", "mjs", "cjs"]
    }

    fn extract(&self, rel_path: &str, source: &str) -> FileParse {
        let language: Language = tree_sitter_javascript::LANGUAGE.into();
        typescript::extract_with(&language, rel_path, source)
    }

    fn directory_modules(&self) -> &'static [&'static str] {
        &["index.js", "index.jsx", "index.mjs", "index.cjs"]
    }

    /// 與 TypeScript 相同：import 寫的是相對路徑，不是模組名。
    fn module_path(&self, _rel_path: &str) -> String {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::ImportTarget;
    use crate::model::{Kind, Rel};

    fn parse(src: &str) -> FileParse {
        JavaScriptExtractor.extract("web/a.js", src)
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
        let p = parse("export function make() {}\nexport class Box {}\nconst LIMIT = 10;\n");

        assert_eq!(names(&p), ["make", "Box", "LIMIT"]);
        let kinds: Vec<Kind> = p.symbols.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [Kind::Function, Kind::Class, Kind::Const]);
    }

    #[test]
    fn methods_are_qualified_by_their_class() {
        let p = parse("class Box {\n  area() { return 1; }\n}\n");

        assert_eq!(names(&p), ["Box", "Box::area"]);
        assert_eq!(p.symbols[1].kind, Kind::Method);
    }

    #[test]
    fn an_arrow_binding_is_a_function() {
        let p = parse("const twice = (n) => n * 2;\n");

        assert_eq!(p.symbols[0].kind, Kind::Function);
    }

    #[test]
    fn calls_are_attributed_to_the_enclosing_declaration() {
        let p = parse("function outer() {\n  helper();\n  obj.method();\n}\n");

        assert_eq!(refs_by(&p, "outer", Rel::Calls), ["helper", "obj.method"]);
    }

    #[test]
    fn a_constructor_call_is_a_type_reference() {
        let p = parse("function make() {\n  return new Box();\n}\n");

        assert_eq!(refs_by(&p, "make", Rel::UsesType), ["Box"]);
    }

    #[test]
    fn imports_are_recorded_with_their_target() {
        let p = parse("import { greet } from './utils';\nimport React from 'react';\n");

        let targets: Vec<(String, ImportTarget)> = p
            .imports
            .iter()
            .map(|i| (i.local.clone(), i.target.clone()))
            .collect();
        assert_eq!(
            targets,
            [
                (
                    "greet".to_string(),
                    ImportTarget::Relative("./utils".to_string())
                ),
                ("React".to_string(), ImportTarget::External),
            ]
        );
    }

    /// JSX 內建在同一份文法裡，`.jsx` 不需要另外的 `Language`。
    #[test]
    fn jsx_files_parse_without_errors() {
        let p = JavaScriptExtractor.extract(
            "web/view.jsx",
            "export function View() {\n  return <div className=\"x\" />;\n}\n",
        );

        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert_eq!(names(&p), ["View"]);
    }

    /// JavaScript 沒有型別標註，型別引用只會來自 `new`。
    #[test]
    fn a_plain_function_has_no_type_references() {
        let p = parse("function make(w, n) {\n  return null;\n}\n");

        assert!(refs_by(&p, "make", Rel::UsesType).is_empty());
    }

    #[test]
    fn javascript_has_no_module_path() {
        assert_eq!(JavaScriptExtractor.module_path("web/a/b.js"), "");
    }

    #[test]
    fn a_syntax_error_still_yields_what_was_parsed() {
        let p = parse("function ok() {}\nfunction broken( {\n");

        assert!(!p.errors.is_empty());
        assert!(names(&p).contains(&"ok"), "{:?}", names(&p));
    }
}
