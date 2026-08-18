//! 把 import 指向的位置對應到索引裡的檔案。
//!
//! 這一層**不認識任何語言**。抽取器已經把各自的 import 語法翻成
//! [`ImportTarget`]，剩下的是純粹的路徑比對：把路徑段接起來，配上該語言
//! 的副檔名與目錄模組檔名，去索引裡找。
//!
//! 這是刻意的分工。對照實作把每個語言的 import 規則寫在解析層，於是
//! 那個檔案長到兩千多行，而且加第 30 個語言還要再回去改它。規則屬於語
//! 言，就該住在語言的模組裡。
//!
//! 對不到不是錯誤。標準函式庫與第三方套件本來就不在索引裡，對不到就是
//! 專案外部，`target_id` 留空即可。

use std::collections::HashMap;

use rusqlite::Connection;

use crate::error::Result;
use crate::extract;

/// 一次對應的結果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// 對應到專案內檔案的 import 數。
    pub linked: usize,
    /// 對應不到、判定為專案外部的 import 數。
    pub external: usize,
}

/// 目標種類在資料庫裡的判別值，與 `store::write` 寫入時一致。
const KIND_RELATIVE: i64 = 0;
const KIND_ROOTED: i64 = 1;
const KIND_EXTERNAL: i64 = 2;

/// 把所有還沒對應的 import 接上目標檔案。
///
/// 必須在所有檔案都寫進索引之後才跑：import 常常指向排在後面才被掃到
/// 的檔案。
pub fn link(conn: &Connection) -> Result<ImportReport> {
    let files = file_index(conn)?;
    let pending = pending_imports(conn)?;

    let mut report = ImportReport::default();
    let mut stmt =
        conn.prepare_cached("UPDATE imports SET target_id = ?1 WHERE file_id = ?2 AND local = ?3")?;

    for row in pending {
        match resolve_one(&files, &row) {
            Some(target) => {
                stmt.execute(rusqlite::params![target, row.file_id, row.local])?;
                report.linked += 1;
            }
            None => report.external += 1,
        }
    }

    Ok(report)
}

/// 一條還沒對應的 import。
struct Pending {
    file_id: i64,
    /// 發出 import 的檔案路徑，相對路徑要以它為基準。
    from_path: String,
    local: String,
    kind: i64,
    spec: String,
}

/// 找出這條 import 指向的檔案。
///
/// 候選有多個時不猜：兩個檔案同時符合表示這個路徑本身就有歧義，接錯的
/// 代價比接不上高。
fn resolve_one(files: &FileIndex, row: &Pending) -> Option<i64> {
    if row.kind == KIND_EXTERNAL {
        return None;
    }

    let base = match row.kind {
        KIND_RELATIVE => join_relative(&row.from_path, &row.spec)?,
        _ => row.spec.clone(),
    };

    // 目錄模組的候選要試，`./components` 指的可能是 `components/index.ts`。
    let extractor = extract::extractor_for(std::path::Path::new(&row.from_path))?;
    let mut candidates: Vec<String> = Vec::new();
    for ext in extractor.extensions() {
        candidates.push(format!("{base}.{ext}"));
    }
    for module in extractor.directory_modules() {
        candidates.push(if base.is_empty() {
            module.to_string()
        } else {
            format!("{base}/{module}")
        });
    }

    for candidate in &candidates {
        if let Some(id) = files.exact.get(candidate.as_str()) {
            return Some(*id);
        }
    }
    // 從專案根算起的路徑常常少了一層來源目錄（`src/`、`app/`）。改用
    // 結尾比對補上，但只在唯一命中時才算數。
    if row.kind == KIND_ROOTED {
        for candidate in &candidates {
            let matches = files.suffix_matches(candidate);
            if matches.len() == 1 {
                return Some(matches[0]);
            }
        }
    }
    None
}

/// 把相對路徑接到來源檔案所在的目錄上，並化簡 `.` 與 `..`。
fn join_relative(from_path: &str, spec: &str) -> Option<String> {
    let mut segments: Vec<&str> = from_path.split('/').collect();
    // 去掉檔名，從它所在的目錄開始。
    segments.pop();

    let mut out: Vec<String> = segments.into_iter().map(str::to_string).collect();
    for part in spec.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            other => out.push(other.to_string()),
        }
    }
    Some(out.join("/"))
}

/// 索引裡所有檔案的路徑。
struct FileIndex {
    exact: HashMap<String, i64>,
}

impl FileIndex {
    /// 路徑結尾符合 `candidate` 的檔案。
    fn suffix_matches(&self, candidate: &str) -> Vec<i64> {
        let needle = format!("/{candidate}");
        self.exact
            .iter()
            .filter(|(path, _)| path.ends_with(&needle))
            .map(|(_, id)| *id)
            .collect()
    }
}

fn file_index(conn: &Connection) -> Result<FileIndex> {
    let mut stmt = conn.prepare("SELECT id, path FROM files")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(0)?)))?;

    let mut exact = HashMap::new();
    for row in rows {
        let (path, id) = row?;
        exact.insert(path, id);
    }
    Ok(FileIndex { exact })
}

fn pending_imports(conn: &Connection) -> Result<Vec<Pending>> {
    let mut stmt = conn.prepare(
        "SELECT i.file_id, f.path, i.local, i.kind, i.spec
         FROM imports i JOIN files f ON f.id = i.file_id
         WHERE i.target_id IS NULL
         ORDER BY i.file_id, i.local",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Pending {
            file_id: r.get(0)?,
            from_path: r.get(1)?,
            local: r.get(2)?,
            kind: r.get(3)?,
            spec: r.get(4)?,
        })
    })?;

    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// 這個檔案裡，`local` 這個名字是從哪個檔案 import 進來的。
pub fn source_of(conn: &Connection, file_id: i64, local: &str) -> Result<Option<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT target_id FROM imports WHERE file_id = ?1 AND local = ?2 AND target_id IS NOT NULL",
    )?;
    let mut rows = stmt.query(rusqlite::params![file_id, local])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(paths: &[(&str, i64)]) -> FileIndex {
        FileIndex {
            exact: paths.iter().map(|(p, id)| (p.to_string(), *id)).collect(),
        }
    }

    fn pending(from: &str, kind: i64, spec: &str) -> Pending {
        Pending {
            file_id: 1,
            from_path: from.to_string(),
            local: "x".to_string(),
            kind,
            spec: spec.to_string(),
        }
    }

    #[test]
    fn an_external_target_resolves_to_nothing() {
        let files = index(&[("web/utils.ts", 2)]);

        assert!(resolve_one(&files, &pending("web/app.ts", KIND_EXTERNAL, "")).is_none());
    }

    #[test]
    fn a_relative_target_finds_the_sibling_file() {
        let files = index(&[("web/utils.ts", 2)]);

        let found = resolve_one(&files, &pending("web/app.ts", KIND_RELATIVE, "./utils"));
        assert_eq!(found, Some(2));
    }

    /// 指向目錄時要試該語言的目錄模組檔名。
    #[test]
    fn a_directory_target_falls_back_to_its_module_file() {
        let files = index(&[("web/components/index.ts", 3)]);

        let found = resolve_one(
            &files,
            &pending("web/app.ts", KIND_RELATIVE, "./components"),
        );
        assert_eq!(found, Some(3));
    }

    /// 從根算起的路徑少一層來源目錄時，用結尾比對補上。
    #[test]
    fn a_rooted_target_may_match_by_suffix() {
        let files = index(&[("api/pkg/mod.py", 4)]);

        let found = resolve_one(&files, &pending("api/app.py", KIND_ROOTED, "pkg/mod"));
        assert_eq!(found, Some(4));
    }

    /// 結尾比對有兩個候選時不猜——接錯比接不上貴。
    #[test]
    fn an_ambiguous_suffix_resolves_to_nothing() {
        let files = index(&[("a/pkg/mod.py", 5), ("b/pkg/mod.py", 6)]);

        assert!(resolve_one(&files, &pending("app.py", KIND_ROOTED, "pkg/mod")).is_none());
    }

    /// 沒有抽取器認領的檔案發不出 import，也就無從決定副檔名候選。
    #[test]
    fn a_file_with_no_extractor_resolves_to_nothing() {
        let files = index(&[("web/utils.ts", 2)]);

        assert!(resolve_one(&files, &pending("notes.md", KIND_RELATIVE, "./utils")).is_none());
    }

    #[test]
    fn a_relative_path_is_joined_to_the_importing_directory() {
        assert_eq!(
            join_relative("web/src/app.ts", "./utils").unwrap(),
            "web/src/utils"
        );
        assert_eq!(
            join_relative("web/src/app.ts", "../lib/h").unwrap(),
            "web/lib/h"
        );
        assert_eq!(join_relative("web/src/app.ts", "./").unwrap(), "web/src");
    }

    /// 往上走超過專案根目錄是不可能的路徑，不該回一個看似合理的答案。
    #[test]
    fn climbing_above_the_project_root_fails() {
        assert!(join_relative("a.ts", "../../x").is_none());
    }
}
