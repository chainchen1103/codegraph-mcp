//! 列舉專案中要索引的原始碼檔案。

use std::path::{Path, PathBuf};

use crate::extract;
use crate::project::{DIR_NAME, Project};

/// 單一檔案的大小上限。超過的檔案多半是產生出來的資料，解析它們的
/// 成本遠高於價值。
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// 一個待索引的檔案。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceFile {
    /// 相對專案根目錄的路徑，以斜線分隔。
    pub rel_path: String,
    /// 位元組數。
    pub size: u64,
}

impl SourceFile {
    /// 檔案在磁碟上的完整路徑。
    pub fn absolute(&self, project: &Project) -> PathBuf {
        project.root().join(&self.rel_path)
    }
}

/// 列出專案中所有支援的原始碼檔案。
///
/// 遵守 `.gitignore` 與 `.ignore`，跳過隱藏檔案、索引目錄本身、副檔名
/// 不支援的檔案，以及超過 [`MAX_FILE_BYTES`] 的檔案。
///
/// 結果依路徑排序，讓索引的產出與檔案系統的列舉順序無關。
pub fn source_files(project: &Project) -> Vec<SourceFile> {
    let root = project.root();
    let mut out = Vec::new();

    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .build();

    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if is_inside_index_dir(root, path) {
            continue;
        }
        if extract::extractor_for(path).is_none() {
            continue;
        }
        let Some(rel) = project.relativize(path) else {
            continue;
        };
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if size > MAX_FILE_BYTES {
            continue;
        }

        out.push(SourceFile {
            rel_path: extract::moniker::normalize_path(&rel.to_string_lossy()),
            size,
        });
    }

    out.sort();
    out
}

/// 路徑是否位於索引目錄底下。
///
/// 索引目錄以點開頭，一般會被隱藏檔規則排除，但使用者可能自行調整
/// 設定，所以另外擋一次。
fn is_inside_index_dir(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .is_ok_and(|rel| rel.components().any(|c| c.as_os_str() == DIR_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{cleanup, tmp_project, write};

    fn paths(project: &Project) -> Vec<String> {
        source_files(project)
            .into_iter()
            .map(|f| f.rel_path)
            .collect()
    }

    #[test]
    fn only_supported_extensions_are_listed() {
        let p = tmp_project("walk-ext", &[]);
        write(&p, "src/a.rs", "fn a() {}\n");
        write(&p, "README.md", "# hi\n");
        write(&p, "Cargo.toml", "[package]\n");

        assert_eq!(paths(&p), vec!["src/a.rs"]);

        cleanup(&p);
    }

    #[test]
    fn results_are_sorted_by_path() {
        let p = tmp_project("walk-sorted", &[]);
        for rel in ["src/z.rs", "src/a.rs", "src/m/b.rs"] {
            write(&p, rel, "fn f() {}\n");
        }

        assert_eq!(paths(&p), vec!["src/a.rs", "src/m/b.rs", "src/z.rs"]);

        cleanup(&p);
    }

    #[test]
    fn gitignored_files_are_skipped() {
        let p = tmp_project("walk-ignore", &[]);
        write(&p, ".gitignore", "generated/\n");
        write(&p, "src/a.rs", "fn a() {}\n");
        write(&p, "generated/big.rs", "fn g() {}\n");

        assert_eq!(paths(&p), vec!["src/a.rs"]);

        cleanup(&p);
    }

    #[test]
    fn the_index_directory_is_never_walked() {
        let p = tmp_project("walk-selfindex", &[]);
        write(&p, "src/a.rs", "fn a() {}\n");
        std::fs::write(p.dir().join("leftover.rs"), "fn x() {}\n").unwrap();

        assert_eq!(paths(&p), vec!["src/a.rs"]);

        cleanup(&p);
    }

    #[test]
    fn oversized_files_are_skipped() {
        let p = tmp_project("walk-big", &[]);
        write(&p, "src/small.rs", "fn a() {}\n");
        let big = format!("fn b() {{}}\n{}", "// x\n".repeat(500_000));
        assert!(big.len() as u64 > MAX_FILE_BYTES);
        write(&p, "src/big.rs", &big);

        assert_eq!(paths(&p), vec!["src/small.rs"]);

        cleanup(&p);
    }

    #[test]
    fn sizes_are_reported_for_each_file() {
        let p = tmp_project("walk-size", &[]);
        let body = "fn a() {}\n";
        write(&p, "src/a.rs", body);

        let files = source_files(&p);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].size, body.len() as u64);
        assert!(files[0].absolute(&p).is_file());

        cleanup(&p);
    }

    #[test]
    fn an_empty_project_yields_nothing() {
        let p = tmp_project("walk-empty", &[]);
        assert!(source_files(&p).is_empty());
        cleanup(&p);
    }
}
