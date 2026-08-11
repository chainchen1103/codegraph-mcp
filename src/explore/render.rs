//! 把挑選結果排版成文字，並依預算分配輸出額度。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use super::budget::{Budget, MIN_USEFUL_CHARS};
use super::select::{Hit, Selection};

/// 讀不到原始碼時顯示的說明。
const FALLBACK_NOTE: &str = "（讀不到原始碼，以下僅有簽名）";

/// 一個符號分配到的輸出。
#[derive(Debug)]
struct Allocated<'a> {
    hit: &'a Hit,
    lines: Vec<(u32, String)>,
    /// 因為額度不足而未列出的行數。
    clipped: usize,
}

/// 排版整份結果。
pub fn render(root: &Path, selection: &Selection, budget: Budget) -> String {
    if selection.hits.is_empty() {
        return not_found(selection);
    }

    let (allocated, omitted) = allocate(root, &selection.hits, budget);
    if allocated.is_empty() {
        return not_found(selection);
    }

    let mut out = String::new();
    writeln!(out, "## Source").ok();

    let mut current_file: Option<&str> = None;
    for item in &allocated {
        if current_file != Some(item.hit.file.as_str()) {
            writeln!(out).ok();
            writeln!(out, "{}", item.hit.file).ok();
            current_file = Some(item.hit.file.as_str());
        }
        render_one(&mut out, item);
    }

    if omitted > 0 {
        writeln!(out).ok();
        writeln!(
            out,
            "額度用完，另有 {omitted} 個符號未列出（共 {}）。用更明確的名字再查一次。",
            selection.hits.len()
        )
        .ok();
    }

    if !selection.unmatched.is_empty() {
        writeln!(out).ok();
        writeln!(out, "查無結果：{}", selection.unmatched.join("、")).ok();
    }

    out
}

/// 依預算決定每個符號能拿到多少輸出。
///
/// 先按來源的優先度分配，讓使用者指名的符號一定拿得到額度；實際列印
/// 時再回到固定的檔案與行號順序，同一個查詢的輸出才會每次相同。
fn allocate<'a>(root: &Path, hits: &'a [Hit], budget: Budget) -> (Vec<Allocated<'a>>, usize) {
    let mut order: Vec<&Hit> = hits.iter().collect();
    order.sort_by(|a, b| {
        a.origin
            .cmp(&b.origin)
            .then(a.file.cmp(&b.file))
            .then(a.start_line.cmp(&b.start_line))
    });

    let mut used_total = 0usize;
    let mut used_per_file: HashMap<&str, usize> = HashMap::new();
    let mut kept: Vec<Allocated<'a>> = Vec::new();
    let mut omitted = 0usize;

    for hit in order {
        let per_file_used = used_per_file.get(hit.file.as_str()).copied().unwrap_or(0);
        let allowance = budget
            .max_chars
            .saturating_sub(used_total)
            .min(budget.max_chars_per_file.saturating_sub(per_file_used));

        if allowance < MIN_USEFUL_CHARS {
            omitted += 1;
            continue;
        }

        let (lines, clipped) = match source_lines(root, hit) {
            Some(all) => clip(all, allowance),
            None => (Vec::new(), 0),
        };

        let cost = lines
            .iter()
            .map(|(_, t)| t.chars().count() + 9)
            .sum::<usize>();
        used_total += cost;
        *used_per_file.entry(hit.file.as_str()).or_insert(0) += cost;

        kept.push(Allocated {
            hit,
            lines,
            clipped,
        });
    }

    kept.sort_by(|a, b| {
        a.hit
            .file
            .cmp(&b.hit.file)
            .then(a.hit.start_line.cmp(&b.hit.start_line))
            .then(a.hit.name.cmp(&b.hit.name))
    });

    (kept, omitted)
}

/// 把行裁切到額度以內，回傳保留的行與裁掉的行數。
///
/// 以整行為單位裁切，半行程式碼沒有閱讀價值。
fn clip(lines: Vec<(u32, String)>, allowance: usize) -> (Vec<(u32, String)>, usize) {
    let total = lines.len();
    let mut used = 0usize;
    let mut kept = Vec::new();

    for (number, text) in lines {
        // 行號欄位與分隔符大約佔 9 個字元。
        let cost = text.chars().count() + 9;
        if used + cost > allowance {
            break;
        }
        used += cost;
        kept.push((number, text));
    }

    let clipped = total - kept.len();
    (kept, clipped)
}

fn render_one(out: &mut String, item: &Allocated<'_>) {
    let hit = item.hit;
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

    if item.lines.is_empty() {
        writeln!(out, "  {FALLBACK_NOTE}").ok();
        if let Some(sig) = &hit.signature {
            writeln!(out, "  {sig}").ok();
        }
        return;
    }

    for (number, text) in &item.lines {
        writeln!(out, "  {number:>5} | {text}").ok();
    }
    if item.clipped > 0 {
        writeln!(out, "  ... 另有 {} 行未列出", item.clipped).ok();
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

/// 查無結果時的說明與候選名稱。
pub fn not_found(selection: &Selection) -> String {
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
    use crate::explore::budget;
    use crate::explore::select::Origin;
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
            origin: Origin::Named,
        }
    }

    fn generous() -> Budget {
        budget::for_file_count(0)
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
        let out = render(&root, &selection, generous());

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
        let out = render(&root, &selection, generous());

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
        let out = render(&root, &selection, generous());

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
        let out = render(&root, &selection, generous());
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
        let out = render(&root, &selection, generous());

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
        let out = render(&root, &selection, generous());

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
        let out = render(&root, &selection, generous());

        assert!(out.contains("codegraph index"), "{out}");

        std::fs::remove_dir_all(&root).ok();
    }

    /// 單一符號超過單檔額度時，以整行為單位裁切並說明少了幾行。
    #[test]
    fn an_oversized_symbol_is_clipped_not_dropped() {
        let root = tmpdir("clip");
        let body: String = (0..500).map(|i| format!("    let x{i} = {i};\n")).collect();
        std::fs::write(root.join("src/a.rs"), format!("fn huge() {{\n{body}}}\n")).unwrap();

        let selection = Selection {
            hits: vec![hit("src/a.rs", 1, 502, "huge")],
            ..Default::default()
        };
        let out = render(&root, &selection, generous());

        assert!(out.contains("      1 | fn huge() {"), "{out}");
        assert!(out.contains("另有"), "沒有說明裁掉了多少：{out}");
        assert!(
            out.chars().count() < generous().max_chars_per_file + 500,
            "輸出超過單檔額度"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// 額度用完時，剩下的符號整段略過並回報數量。
    #[test]
    fn symbols_that_do_not_fit_are_reported_as_omitted() {
        let root = tmpdir("omit");
        let mut source = String::new();
        for i in 0..40 {
            source.push_str(&format!(
                "fn f{i}() {{\n{}}}\n",
                "    let y = 1;\n".repeat(20)
            ));
        }
        std::fs::write(root.join("src/a.rs"), &source).unwrap();

        let hits: Vec<Hit> = (0..40)
            .map(|i| {
                let start = i * 22 + 1;
                hit(
                    "src/a.rs",
                    start as u32,
                    start as u32 + 21,
                    &format!("f{i}"),
                )
            })
            .collect();
        let selection = Selection {
            hits,
            ..Default::default()
        };
        let out = render(&root, &selection, generous());

        assert!(out.contains("未列出"), "{out}");
        assert!(out.contains("共 40"), "{out}");
    }

    /// 使用者指名的符號優先拿到額度，全文檢索找到的排在後面。
    #[test]
    fn named_symbols_win_the_budget_over_search_results() {
        let root = tmpdir("priority");
        let filler = "    let y = 1;\n".repeat(80);
        std::fs::write(
            root.join("src/a.rs"),
            format!("fn from_search() {{\n{filler}}}\nfn wanted() {{\n    1\n}}\n"),
        )
        .unwrap();

        let mut searched = hit("src/a.rs", 1, 82, "from_search");
        searched.origin = Origin::Text;
        let named = hit("src/a.rs", 83, 85, "wanted");

        let selection = Selection {
            hits: vec![searched, named],
            ..Default::default()
        };
        let out = render(&root, &selection, generous());

        assert!(out.contains("fn wanted()"), "指名的符號被額度擠掉了：{out}");

        std::fs::remove_dir_all(&root).ok();
    }

    /// 輸出不得把呼叫端導向自行開檔案，那等於承認這個工具沒有用。
    #[test]
    fn the_output_never_tells_the_caller_to_read_files_directly() {
        let root = tmpdir("noread");
        std::fs::write(root.join("src/a.rs"), "fn one() {}\n").unwrap();

        let cases = [
            Selection {
                hits: vec![hit("src/a.rs", 1, 1, "one")],
                ..Default::default()
            },
            Selection {
                hits: vec![],
                unmatched: vec!["zzz".into()],
                suggestions: vec![],
            },
        ];

        for selection in cases {
            let out = render(&root, &selection, generous());
            for banned in ["Read", "自己打開", "開啟檔案", "cat "] {
                assert!(!out.contains(banned), "輸出出現了 `{banned}`：{out}");
            }
        }

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn allocation_order_does_not_change_the_printed_order() {
        let root = tmpdir("stable");
        std::fs::write(root.join("src/a.rs"), "fn one() {}\nfn two() {}\n").unwrap();

        let mut first = hit("src/a.rs", 1, 1, "one");
        first.origin = Origin::Text;
        let second = hit("src/a.rs", 2, 2, "two");

        let selection = Selection {
            hits: vec![first, second],
            ..Default::default()
        };
        let out = render(&root, &selection, generous());

        let pos_one = out.find("fn one()").unwrap();
        let pos_two = out.find("fn two()").unwrap();
        assert!(pos_one < pos_two, "輸出沒有依行號排序：{out}");

        std::fs::remove_dir_all(&root).ok();
    }
}
