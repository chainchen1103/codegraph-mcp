//! 單元測試共用的設置。
//!
//! 這個模組只在測試時編譯。多數測試需要的不外乎兩種東西：一個裝了
//! 幾份原始碼的索引，或一個落在暫存目錄裡的專案。

use std::path::PathBuf;

use crate::project::Project;
use crate::store::Store;
use crate::store::write::{Writer, rebuild_fts};
use crate::{extract, indexer, resolve};

/// 建立一個記憶體索引，內容是給定的原始碼。
///
/// 只寫入符號與待解析的引用，不做解析。要驗證解析行為的測試自己呼叫
/// [`crate::resolve::resolve_all`]，才看得到解析前後的差別。
pub fn indexed(files: &[(&str, &str)]) -> Store {
    let mut store = Store::in_memory().unwrap();
    let mut writer = Writer::new();

    store
        .with_transaction(|conn| {
            Writer::reset(conn)?;
            let unit = writer.unit(conn, "root")?;
            for (path, text) in files {
                let parse = extract::extract(path, text).unwrap();
                let module = crate::project::module_path(path);
                writer.write_file(conn, unit, path, &module, "hash", &parse)?;
            }
            rebuild_fts(conn)
        })
        .unwrap();
    store
}

/// 同 [`indexed`]，但引用已經解析成邊。
pub fn resolved(files: &[(&str, &str)]) -> Store {
    let mut store = indexed(files);
    resolve::resolve_all(&mut store).unwrap();
    store
}

/// 一個乾淨的暫存目錄。
///
/// 目錄裡放一個 `.git` 當作 repo 邊界。暫存目錄位在使用者家目錄底下，
/// 而家目錄可能自己就有索引目錄；沒有邊界的話，「找不到索引」的測試會
/// 撞到那一份。
pub fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "codegraph-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    dir
}

/// 一個建好索引目錄、並寫入給定原始碼的專案。尚未索引。
pub fn tmp_project(tag: &str, files: &[(&str, &str)]) -> Project {
    let dir = tmpdir(tag);
    let project = Project::create(&dir).unwrap();

    for (rel, body) in files {
        write(&project, rel, body);
    }
    project
}

/// 在專案裡寫一個檔案，必要時建立目錄。
pub fn write(project: &Project, rel: &str, body: &str) {
    let path = project.root().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// 同 [`tmp_project`]，並且已經完成一次全量索引。
pub fn indexed_project(tag: &str, files: &[(&str, &str)]) -> Project {
    let project = tmp_project(tag, files);
    let mut store = Store::open(&project.db_path()).unwrap();
    indexer::index_project(&project, &mut store).unwrap();
    project
}

/// 用完就刪掉整棵暫存目錄。
pub fn cleanup(project: &Project) {
    std::fs::remove_dir_all(project.root()).ok();
}
