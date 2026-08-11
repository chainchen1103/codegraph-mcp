//! `codegraph outline <file>` —— 單一檔案的結構骨架。
//!
//! 不需要索引：抽取層是純函數，讀檔就能跑。這是 Stage 2 的驗收工具，
//! 也是之後 `explore` 輸出的雛形。

use std::fmt::Write as _;
use std::path::Path;

use crate::error::{CgError, NotIndexedReason, Result};
use crate::extract;
use crate::project::Project;

pub fn run(file: &Path) -> Result<String> {
    if !file.is_file() {
        return Err(CgError::FileNotIndexed {
            path: file.to_path_buf(),
            reason: NotIndexedReason::NotYetIndexed,
        });
    }

    // moniker 用的是相對專案根目錄的路徑。不在任何專案裡（例如對著
    // 隨手一個檔案跑）就退回原本給的路徑——抽取本身不需要專案。
    let rel = relative_path(file);
    let source = std::fs::read_to_string(file)?;

    let Some(parse) = extract::extract(&rel, &source) else {
        return Err(CgError::FileNotIndexed {
            path: file.to_path_buf(),
            reason: NotIndexedReason::UnsupportedExtension,
        });
    };

    Ok(render(&rel, &parse))
}

fn relative_path(file: &Path) -> String {
    let absolute = file.canonicalize();
    let candidate = absolute.as_deref().unwrap_or(file);

    if let Some(dir) = candidate.parent()
        && let Ok(project) = Project::discover(dir)
        && let Some(rel) = project.relativize(candidate)
    {
        return extract::moniker::normalize_path(&rel.to_string_lossy());
    }
    extract::moniker::normalize_path(&file.to_string_lossy())
}

fn render(rel: &str, parse: &extract::FileParse) -> String {
    let mut out = String::new();
    writeln!(out, "{rel} — {} 個符號", parse.symbols.len()).ok();

    for e in &parse.errors {
        writeln!(out, "  ⚠ {e}").ok();
    }
    if !parse.errors.is_empty() {
        writeln!(out).ok();
    }

    let width = parse
        .symbols
        .iter()
        .map(|s| s.qualified.chars().count())
        .max()
        .unwrap_or(0)
        .min(48);

    for s in &parse.symbols {
        let lines = format!("{}-{}", s.start_line, s.end_line);
        writeln!(
            out,
            "  {:<9} {:<width$}  {:>9}  {}",
            s.kind.as_str(),
            s.qualified,
            lines,
            s.signature.as_deref().unwrap_or(""),
            width = width
        )
        .ok();
    }

    if parse.symbols.is_empty() {
        writeln!(out, "  （沒有抽到任何符號）").ok();
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpfile(tag: &str, name: &str, body: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("codegraph-outline-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn outline_lists_symbols_with_lines_and_signatures() {
        let f = tmpfile(
            "basic",
            "a.rs",
            "/// 說明\npub fn open(p: &str) -> u8 {\n    1\n}\n",
        );
        let out = run(&f).unwrap();

        assert!(out.contains("1 個符號"), "{out}");
        assert!(out.contains("function"), "{out}");
        assert!(out.contains("open"), "{out}");
        assert!(out.contains("2-4"), "行號不對：{out}");
        assert!(out.contains("pub fn open(p: &str) -> u8"), "{out}");

        std::fs::remove_dir_all(f.parent().unwrap()).ok();
    }

    #[test]
    fn an_empty_source_file_says_so_instead_of_printing_nothing() {
        let f = tmpfile("empty", "a.rs", "\n");
        let out = run(&f).unwrap();
        assert!(out.contains("沒有抽到任何符號"), "{out}");
        std::fs::remove_dir_all(f.parent().unwrap()).ok();
    }

    #[test]
    fn syntax_errors_are_surfaced_above_the_symbols() {
        let f = tmpfile("broken", "a.rs", "fn good() {}\nfn broken( {\n");
        let out = run(&f).unwrap();
        assert!(out.contains("⚠"), "{out}");
        assert!(out.contains("good"), "壞檔案不該讓好符號消失：{out}");
        std::fs::remove_dir_all(f.parent().unwrap()).ok();
    }

    #[test]
    fn an_unsupported_extension_is_a_recoverable_condition() {
        let f = tmpfile("md", "README.md", "# hi\n");
        let err = run(&f).unwrap_err();
        assert!(matches!(
            err,
            CgError::FileNotIndexed {
                reason: NotIndexedReason::UnsupportedExtension,
                ..
            }
        ));
        assert!(err.is_recoverable());
        std::fs::remove_dir_all(f.parent().unwrap()).ok();
    }

    #[test]
    fn a_missing_file_is_reported_without_reading_it() {
        let err = run(Path::new("這個檔案不存在-codegraph.rs")).unwrap_err();
        assert!(matches!(err, CgError::FileNotIndexed { .. }));
        assert!(err.is_recoverable());
    }

    /// 在專案裡跑時，路徑要是相對根目錄的——絕對路徑會把
    /// 「誰的機器」烙進 moniker。
    #[test]
    fn paths_are_reported_relative_to_the_project_root() {
        let out = run(Path::new("src/extract/lang/rust.rs")).unwrap();
        assert!(
            out.starts_with("src/extract/lang/rust.rs"),
            "路徑沒有相對化：{}",
            out.lines().next().unwrap_or("")
        );
    }
}
