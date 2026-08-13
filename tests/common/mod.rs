//! 整合測試共用的設置。
//!
//! 整合測試各自是獨立的 crate，用不到 lib 內部的 `#[cfg(test)]` 模組，
//! 因此另外放一份。
//!
//! 這個檔案會被編進每一個整合測試的 crate，而每個 crate 只用得到其中
//! 一部分，未被用到的部分在該 crate 裡就成了死碼。

#![allow(dead_code)]

use code_graph::indexer;
use code_graph::project::Project;
use code_graph::store::Store;

/// 一個落在暫存目錄裡的專案，離開作用域時整棵刪除。
pub struct Fixture {
    pub project: Project,
}

impl Fixture {
    /// 建立專案並寫入給定的原始碼。尚未索引。
    ///
    /// 目錄裡放一個 `.git` 當作 repo 邊界。暫存目錄位在使用者家目錄
    /// 底下，而家目錄可能自己就有索引目錄；沒有邊界的話，往上尋找會
    /// 撞到那一份。
    pub fn new(tag: &str, files: &[(&str, &str)]) -> Self {
        let dir = std::env::temp_dir().join(format!("codegraph-it-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();

        let fixture = Self {
            project: Project::create(&dir).unwrap(),
        };
        for (rel, body) in files {
            fixture.write(rel, body);
        }
        fixture
    }

    /// 建立專案、寫入原始碼並完成一次全量索引。
    pub fn indexed(tag: &str, files: &[(&str, &str)]) -> Self {
        let fixture = Self::new(tag, files);
        fixture.index();
        fixture
    }

    /// 寫一個檔案，必要時建立目錄。
    pub fn write(&self, rel: &str, body: &str) {
        let path = self.project.root().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// 重新索引，回傳報告與開啟中的索引。
    pub fn index(&self) -> (indexer::IndexReport, Store) {
        let mut store = Store::open(&self.project.db_path()).unwrap();
        let report = indexer::index_project(&self.project, &mut store).unwrap();
        (report, store)
    }

    /// 開啟索引。
    pub fn store(&self) -> Store {
        Store::open(&self.project.db_path()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.project.root()).ok();
    }
}

/// 取單一欄位的查詢結果。
pub fn query_one<T: rusqlite::types::FromSql>(store: &Store, sql: &str) -> T {
    store.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}
