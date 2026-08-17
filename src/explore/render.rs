//! 把挑選結果排版成文字，並依預算分配輸出額度。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use super::budget::{Budget, MIN_USEFUL_CHARS};
use super::select::{Blast, Hit, Selection};
use crate::graph::path::Path as CallPath;
use crate::model::{Provenance, SymbolId};

/// 受影響範圍裡每個符號最多列幾個檔案。
const BLAST_FILES: usize = 5;

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

/// 一個確實把完整原始碼送出去的符號。
///
/// 只有整段都送出的才算數：讀不到原始碼、或因額度被裁掉一部分的，都不
/// 算送過。呼叫端據此判斷下一次能不能省略，寧可重送也不能少送。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Emitted {
    pub id: SymbolId,
}

/// 排版整份結果。
pub fn render(root: &Path, selection: &Selection, budget: Budget) -> String {
    reporting(root, selection, budget).0
}

/// 排版，並回報實際完整送出了哪些符號。
pub fn reporting(root: &Path, selection: &Selection, budget: Budget) -> (String, Vec<Emitted>) {
    if selection.hits.is_empty() {
        return (not_found(selection), Vec::new());
    }

    let (allocated, omitted) = allocate(root, &selection.hits, budget);
    if allocated.is_empty() {
        return (not_found(selection), Vec::new());
    }

    let emitted: Vec<Emitted> = allocated
        .iter()
        .filter(|a| !a.lines.is_empty() && a.clipped == 0)
        .map(|a| Emitted { id: a.hit.id })
        .collect();

    let mut out = String::new();
    render_flow(&mut out, &selection.flows);
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

    render_blast(&mut out, &selection.blast);

    if !selection.unmatched.is_empty() {
        writeln!(out).ok();
        writeln!(out, "查無結果：{}", selection.unmatched.join("、")).ok();
    }

    (out, emitted)
}

/// 排版受影響範圍。
///
/// 依檔案彙總而不是逐條列。專案裡光是一個 `Result` 就有近百個入邊，攤
/// 開會把原始碼擠出畫面，而原始碼才是呼叫端要的東西。
fn render_blast(out: &mut String, blast: &[Blast]) {
    if blast.is_empty() {
        return;
    }

    writeln!(out).ok();
    writeln!(out, "## Blast radius").ok();

    for item in blast {
        writeln!(out).ok();
        writeln!(
            out,
            "  {} {}  {} 處",
            item.kind.as_str(),
            item.qualified,
            item.impact.total
        )
        .ok();

        let width = item
            .impact
            .files
            .iter()
            .take(BLAST_FILES)
            .map(|u| u.file.chars().count())
            .max()
            .unwrap_or(0)
            .min(48);

        for users in item.impact.files.iter().take(BLAST_FILES) {
            writeln!(
                out,
                "    {:<width$}  {}",
                users.file,
                users.count,
                width = width
            )
            .ok();
        }

        let rest = item.impact.files.len().saturating_sub(BLAST_FILES);
        if rest > 0 {
            writeln!(out, "    另有 {rest} 個檔案").ok();
        }
    }
}

/// 排版呼叫路徑。
///
/// 每一跳的位置是它呼叫下一跳的地方，因此順著讀下來就是一串呼叫點；
/// 最後一跳沒有下一跳，只列出它所在的檔案。
fn render_flow(out: &mut String, flows: &[CallPath]) {
    if flows.is_empty() {
        return;
    }

    writeln!(out, "## Flow").ok();

    for path in flows {
        writeln!(out).ok();
        let width = path
            .hops
            .iter()
            .map(|h| h.qualified.chars().count())
            .max()
            .unwrap_or(0)
            .min(48);

        for hop in &path.hops {
            let site = match hop.line {
                Some(line) => format!("{}:{line}", hop.file),
                None => hop.file.clone(),
            };
            // 合成的邊要標出來，呼叫端才判斷得了這一跳可不可信。
            let note = match hop.provenance {
                Provenance::Heuristic => "  [heuristic]",
                Provenance::Static => "",
            };
            writeln!(
                out,
                "  {:<width$}  {site}{note}",
                hop.qualified,
                width = width
            )
            .ok();
        }
    }

    writeln!(out).ok();
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
    use crate::graph::impact::{Impact, Users};
    use crate::graph::path::Hop;
    use crate::model::{Kind, SymbolId};

    /// 一個帶 `src/` 的暫存目錄，測試在裡面放要被讀取的原始碼。
    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let dir = crate::testing::tmpdir(&format!("render-{tag}"));
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
            flows: Vec::new(),
            blast: Vec::new(),
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
            flows: Vec::new(),
            blast: Vec::new(),
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
            flows: Vec::new(),
            blast: Vec::new(),
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
                flows: Vec::new(),
                blast: Vec::new(),
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

    fn hop(qualified: &str, file: &str, line: Option<u32>, provenance: Provenance) -> Hop {
        Hop {
            id: SymbolId(1),
            qualified: qualified.to_string(),
            kind: Kind::Function,
            file: file.to_string(),
            line,
            provenance,
        }
    }

    fn flow(hops: Vec<Hop>) -> CallPath {
        CallPath { hops }
    }

    /// 路徑排在原始碼前面：先知道怎麼走到，再看每一站的內容。
    #[test]
    fn the_flow_section_comes_before_the_source() {
        let root = tmpdir("flow");
        std::fs::write(root.join("src/a.rs"), "fn one() {}\n").unwrap();

        let selection = Selection {
            hits: vec![hit("src/a.rs", 1, 1, "one")],
            flows: vec![flow(vec![
                hop("entry", "src/a.rs", Some(7), Provenance::Static),
                hop("one", "src/a.rs", None, Provenance::Static),
            ])],
            ..Default::default()
        };
        let out = render(&root, &selection, generous());

        let flow_at = out.find("## Flow").expect("沒有 Flow 區塊");
        let source_at = out.find("## Source").unwrap();
        assert!(flow_at < source_at, "Flow 排在 Source 後面：{out}");
        assert!(out.contains("entry"), "{out}");
    }

    /// 每一跳帶的是它呼叫下一跳的位置，終點沒有下一跳因此只列檔案。
    #[test]
    fn every_hop_carries_a_location() {
        let root = tmpdir("flowsites");
        std::fs::write(root.join("src/a.rs"), "fn one() {}\n").unwrap();

        let selection = Selection {
            hits: vec![hit("src/a.rs", 1, 1, "one")],
            flows: vec![flow(vec![
                hop("entry", "src/cli.rs", Some(7), Provenance::Static),
                hop("middle", "src/mid.rs", Some(21), Provenance::Static),
                hop("one", "src/a.rs", None, Provenance::Static),
            ])],
            ..Default::default()
        };
        let out = render(&root, &selection, generous());

        assert!(out.contains("src/cli.rs:7"), "{out}");
        assert!(out.contains("src/mid.rs:21"), "{out}");
        assert!(
            !out.contains("src/a.rs:0"),
            "沒有位置時不該編一個出來：{out}"
        );
    }

    /// 合成的跳點要攤開標示，呼叫端才判斷得了這一跳可不可信。
    #[test]
    fn a_synthesised_hop_is_flagged_in_the_output() {
        let root = tmpdir("flowheuristic");
        std::fs::write(root.join("src/a.rs"), "fn one() {}\n").unwrap();

        let selection = Selection {
            hits: vec![hit("src/a.rs", 1, 1, "one")],
            flows: vec![flow(vec![
                hop("entry", "src/cli.rs", Some(7), Provenance::Static),
                hop("one", "src/a.rs", None, Provenance::Heuristic),
            ])],
            ..Default::default()
        };
        let out = render(&root, &selection, generous());

        assert_eq!(out.matches("[heuristic]").count(), 1, "{out}");
    }

    /// 受影響範圍依檔案彙總，排在原始碼之後。
    #[test]
    fn the_blast_radius_summarises_by_file() {
        let root = tmpdir("blast");
        std::fs::write(root.join("src/a.rs"), "fn one() {}\n").unwrap();

        let selection = Selection {
            hits: vec![hit("src/a.rs", 1, 1, "one")],
            blast: vec![Blast {
                qualified: "Widget".into(),
                kind: Kind::Struct,
                impact: Impact {
                    files: vec![
                        Users {
                            file: "src/b.rs".into(),
                            count: 4,
                        },
                        Users {
                            file: "src/c.rs".into(),
                            count: 1,
                        },
                    ],
                    total: 5,
                },
            }],
            ..Default::default()
        };
        let out = render(&root, &selection, generous());

        assert!(out.contains("## Blast radius"), "{out}");
        assert!(out.contains("struct Widget  5 處"), "{out}");
        assert!(out.contains("src/b.rs"), "{out}");
        assert!(
            out.find("## Source") < out.find("## Blast radius"),
            "影響範圍該排在原始碼之後：{out}"
        );
    }

    /// 檔案太多時只列前幾個，其餘用一行帶過。
    #[test]
    fn a_widely_used_symbol_lists_only_the_heaviest_files() {
        let root = tmpdir("blastmany");
        std::fs::write(root.join("src/a.rs"), "fn one() {}\n").unwrap();

        let files: Vec<Users> = (0..BLAST_FILES + 3)
            .map(|i| Users {
                file: format!("src/f{i}.rs"),
                count: 1,
            })
            .collect();
        let total = files.len();

        let selection = Selection {
            hits: vec![hit("src/a.rs", 1, 1, "one")],
            blast: vec![Blast {
                qualified: "Result".into(),
                kind: Kind::TypeAlias,
                impact: Impact { files, total },
            }],
            ..Default::default()
        };
        let out = render(&root, &selection, generous());

        assert_eq!(out.matches("src/f").count(), BLAST_FILES, "{out}");
        assert!(out.contains("另有 3 個檔案"), "{out}");
    }

    /// 沒有人依賴的符號不印空區塊。
    #[test]
    fn nothing_depends_on_it_means_no_section() {
        let root = tmpdir("noblast");
        std::fs::write(root.join("src/a.rs"), "fn one() {}\n").unwrap();

        let selection = Selection {
            hits: vec![hit("src/a.rs", 1, 1, "one")],
            ..Default::default()
        };
        let out = render(&root, &selection, generous());

        assert!(!out.contains("## Blast radius"), "{out}");
    }

    #[test]
    fn without_a_path_there_is_no_flow_section() {
        let root = tmpdir("noflow");
        std::fs::write(root.join("src/a.rs"), "fn one() {}\n").unwrap();

        let selection = Selection {
            hits: vec![hit("src/a.rs", 1, 1, "one")],
            ..Default::default()
        };
        let out = render(&root, &selection, generous());

        assert!(!out.contains("## Flow"), "{out}");
    }

    #[test]
    fn several_paths_are_listed_separately() {
        let root = tmpdir("flows");
        std::fs::write(root.join("src/a.rs"), "fn one() {}\n").unwrap();

        let path = flow(vec![
            hop("entry", "src/cli.rs", Some(7), Provenance::Static),
            hop("one", "src/a.rs", None, Provenance::Static),
        ]);
        let selection = Selection {
            hits: vec![hit("src/a.rs", 1, 1, "one")],
            flows: vec![path.clone(), path],
            ..Default::default()
        };
        let out = render(&root, &selection, generous());

        assert_eq!(out.matches("entry").count(), 2, "{out}");
        assert_eq!(out.matches("## Flow").count(), 1, "{out}");
    }
}
