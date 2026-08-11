//! 把挑選結果排版成文字。

use std::fmt::Write as _;
use std::path::Path;

use super::select::{Hit, Selection};

/// 讀不到原始碼時，最多顯示幾行後備資訊。
const FALLBACK_NOTE: &str = "（讀不到原始碼，以下僅有簽名）";

/// 排版整份結果。
pub fn render(root: &Path, selection: &Selection) -> String {
    let mut out = String::new();

    if selection.hits.is_empty() {
        return render_nothing_found(selection);
    }

    writeln!(out, "## Source").ok();

    let mut current_file: Option<&str> = None;
    for hit in &selection.hits {
        if current_file != Some(hit.file.as_str()) {
            writeln!(out).ok();
            writeln!(out, "{}", hit.file).ok();
            current_file = Some(hit.file.as_str());
        }
        render_hit(&mut out, root, hit);
    }

    if !selection.unmatched.is_empty() {
        writeln!(out).ok();
        writeln!(out, "查無結果：{}", selection.unmatched.join("、")).ok();
    }

    out
}

fn render_hit(out: &mut String, root: &Path, hit: &Hit) {
    writeln!(out).ok();
    writeln!(
        out,
        "  {} {}  {}:{}-{}",
        hit.kind.as_str(),
        hit.qualified,
        hit.file,
        hit.start_line,
        hit.end_line
    )
    .ok();

    match source_lines(root, hit) {
        Some(lines) => {
            for (number, text) in lines {
                writeln!(out, "  {number:>5} | {text}").ok();
            }
        }
        None => {
            writeln!(out, "  {FALLBACK_NOTE}").ok();
            if let Some(sig) = &hit.signature {
                writeln!(out, "  {sig}").ok();
            }
        }
    }
}

/// 取出符號涵蓋的原始碼，附上 1 起算的行號。
///
/// 原始碼一律從磁碟讀取而非資料庫：檔案可能在索引之後被修改過，回傳
/// 過期的內容會讓呼叫端據此做出錯誤的編輯。
fn source_lines(root: &Path, hit: &Hit) -> Option<Vec<(u32, String)>> {
    let text = std::fs::read_to_string(root.join(&hit.file)).ok()?;
    let start = hit.start_line.max(1) as usize;
    let end = hit.end_line.max(hit.start_line) as usize;

    let lines: Vec<(u32, String)> = text
        .lines()
        .enumerate()
        .skip(start - 1)
        .take(end - start + 1)
        .map(|(i, line)| (i as u32 + 1, line.to_string()))
        .collect();

    if lines.is_empty() { None } else { Some(lines) }
}

fn render_nothing_found(selection: &Selection) -> String {
    let mut out = String::new();
    writeln!(out, "查無結果：{}", selection.unmatched.join("、")).ok();

    if selection.suggestions.is_empty() {
        writeln!(out).ok();
        writeln!(
            out,
            "索引裡沒有相近的名稱。確認專案已經索引：codegraph index"
        )
        .ok();
    } else {
        writeln!(out).ok();
        writeln!(out, "相近的名稱：").ok();
        for name in &selection.suggestions {
            writeln!(out, "  {name}").ok();
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, SymbolId};

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("codegraph-render-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        dir
    }

    fn hit(file: &str, start: u32, end: u32, qualified: &str) -> Hit {
        Hit {
            id: SymbolId(start),
            name: qualified.rsplit("::").next().unwrap().to_string(),
            qualified: qualified.to_string(),
            kind: Kind::Function,
            file: file.to_string(),
            start_line: start,
            end_line: end,
            signature: Some(format!("fn {qualified}()")),
            docstring: None,
        }
    }

    #[test]
    fn source_is_printed_verbatim_with_line_numbers() {
        let root = tmpdir("verbatim");
        std::fs::write(
            root.join("src/a.rs"),
            "fn skip() {}\nfn target() {\n    let x = 1;\n}\n",
        )
        .unwrap();

        let selection = Selection {
            hits: vec![hit("src/a.rs", 2, 4, "target")],
            ..Default::default()
        };
        let out = render(&root, &selection);

        assert!(out.contains("## Source"), "{out}");
        assert!(out.contains("      2 | fn target() {"), "{out}");
        assert!(out.contains("      3 |     let x = 1;"), "{out}");
        assert!(out.contains("      4 | }"), "{out}");
        assert!(!out.contains("fn skip"), "印出了範圍外的行：{out}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn hits_are_grouped_under_their_file() {
        let root = tmpdir("grouped");
        std::fs::write(root.join("src/a.rs"), "fn one() {}\nfn two() {}\n").unwrap();

        let selection = Selection {
            hits: vec![hit("src/a.rs", 1, 1, "one"), hit("src/a.rs", 2, 2, "two")],
            ..Default::default()
        };
        let out = render(&root, &selection);

        assert_eq!(out.matches("src/a.rs\n").count(), 1, "檔名重複出現：{out}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_missing_file_degrades_to_the_signature() {
        let root = tmpdir("missing");
        let selection = Selection {
            hits: vec![hit("src/gone.rs", 1, 3, "vanished")],
            ..Default::default()
        };
        let out = render(&root, &selection);

        assert!(out.contains(FALLBACK_NOTE), "{out}");
        assert!(out.contains("fn vanished()"), "{out}");

        std::fs::remove_dir_all(&root).ok();
    }

    /// 檔案在索引之後被改短，行號可能超出檔尾。
    #[test]
    fn a_range_beyond_the_end_of_the_file_is_handled() {
        let root = tmpdir("truncated");
        std::fs::write(root.join("src/a.rs"), "fn one() {}\n").unwrap();

        let selection = Selection {
            hits: vec![hit("src/a.rs", 50, 60, "gone")],
            ..Default::default()
        };
        let out = render(&root, &selection);
        assert!(out.contains(FALLBACK_NOTE), "{out}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unmatched_tokens_are_listed_alongside_the_results() {
        let root = tmpdir("partial");
        std::fs::write(root.join("src/a.rs"), "fn one() {}\n").unwrap();

        let selection = Selection {
            hits: vec![hit("src/a.rs", 1, 1, "one")],
            unmatched: vec!["missing".into()],
            suggestions: vec![],
        };
        let out = render(&root, &selection);

        assert!(out.contains("      1 | fn one() {}"), "{out}");
        assert!(out.contains("查無結果：missing"), "{out}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn nothing_found_lists_the_suggestions() {
        let root = tmpdir("suggest");
        let selection = Selection {
            hits: vec![],
            unmatched: vec!["opne".into()],
            suggestions: vec!["Store::open".into(), "open".into()],
        };
        let out = render(&root, &selection);

        assert!(out.contains("查無結果：opne"), "{out}");
        assert!(out.contains("Store::open"), "{out}");
        assert!(!out.contains("## Source"), "沒有結果時不該有原始碼區塊");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn nothing_found_and_nothing_similar_points_at_indexing() {
        let root = tmpdir("noidea");
        let selection = Selection {
            hits: vec![],
            unmatched: vec!["zzz".into()],
            suggestions: vec![],
        };
        let out = render(&root, &selection);

        assert!(out.contains("codegraph index"), "{out}");

        std::fs::remove_dir_all(&root).ok();
    }
}
