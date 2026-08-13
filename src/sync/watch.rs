//! 監看檔案變更並持續同步索引。
//!
//! 編輯器存檔往往在幾毫秒內連續觸發多次事件，格式化工具與版本控制的
//! 操作更會一次動到成批的檔案。事件先進佇列，靜下來之後才一起處理。

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use notify::{Event, RecursiveMode, Watcher};

use super::{Outcome, SyncReport};
use crate::error::Result;
use crate::extract;
use crate::project::{DIR_NAME, Project};
use crate::store::Store;

/// 收到事件後再等這麼久，期間的事件併入同一批。
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// 一批變更同步完成後的通知。
pub struct Batch {
    /// 這一批處理到的檔案，相對專案根目錄。
    pub paths: Vec<String>,
    pub report: SyncReport,
}

/// 持續監看專案並同步索引，直到 `on_batch` 回傳 `false`。
///
/// 每處理完一批就呼叫一次 `on_batch`，讓呼叫端決定要輸出什麼、以及要
/// 不要繼續。
pub fn run(
    project: &Project,
    store: &mut Store,
    mut on_batch: impl FnMut(&Batch) -> bool,
) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        if let Ok(event) = event {
            // 送不出去代表接收端已經結束，這裡沒有別的處置方式。
            let _ = tx.send(event);
        }
    })
    .map_err(watch_failed)?;

    watcher
        .watch(project.root(), RecursiveMode::Recursive)
        .map_err(watch_failed)?;

    while let Some(pending) = next_batch(project, &rx) {
        if pending.is_empty() {
            continue;
        }

        let batch = apply(project, store, pending)?;
        if !on_batch(&batch) {
            break;
        }
    }

    Ok(())
}

/// 取出下一批要處理的路徑，佇列關閉時回 `None`。
///
/// 先等到有事件為止，再等到靜下來為止。一次存檔在幾毫秒內連續觸發的
/// 事件因此併成一批，只重新解析一次。
fn next_batch(project: &Project, rx: &mpsc::Receiver<Event>) -> Option<BTreeSet<String>> {
    let first = rx.recv().ok()?;

    let mut pending = BTreeSet::new();
    collect(project, &first, &mut pending);

    while let Ok(next) = rx.recv_timeout(DEBOUNCE) {
        collect(project, &next, &mut pending);
    }
    Some(pending)
}

/// 同步一批檔案。
fn apply(project: &Project, store: &mut Store, paths: BTreeSet<String>) -> Result<Batch> {
    let mut report = SyncReport::default();
    let mut touched = Vec::new();

    for path in paths {
        let (outcome, one) = super::file(project, store, &path)?;
        report.updated += one.updated;
        report.removed += one.removed;
        report.unchanged += one.unchanged;
        report.resolved += one.resolved;
        report.requeued += one.requeued;
        report.warnings.extend(one.warnings);
        report.elapsed += one.elapsed;

        if outcome != Outcome::Unchanged && outcome != Outcome::Skipped {
            touched.push(path);
        }
    }

    Ok(Batch {
        paths: touched,
        report,
    })
}

/// 把事件裡值得處理的路徑放進佇列。
fn collect(project: &Project, event: &Event, out: &mut BTreeSet<String>) {
    for path in &event.paths {
        if let Some(rel) = indexable(project, path) {
            out.insert(rel);
        }
    }
}

/// 這個路徑是否該進索引，是的話回傳相對路徑。
///
/// 已刪除的檔案在磁碟上查不到副檔名以外的資訊，因此判斷只看副檔名與
/// 位置，不看檔案是否存在——刪除事件正是要處理的情況之一。
fn indexable(project: &Project, path: &Path) -> Option<String> {
    if path.components().any(|c| c.as_os_str() == DIR_NAME) {
        return None;
    }
    extract::extractor_for(path)?;

    let rel = project.relativize(path)?;
    Some(extract::moniker::normalize_path(&rel.to_string_lossy()))
}

/// 監看失敗時的錯誤。
fn watch_failed(e: notify::Error) -> crate::error::CgError {
    crate::error::CgError::Io(std::io::Error::other(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{cleanup, indexed_project};

    #[test]
    fn source_files_inside_the_project_are_indexable() {
        let p = indexed_project("watch-indexable", &[("src/a.rs", "fn one() {}\n")]);

        assert_eq!(
            indexable(&p, &p.root().join("src/a.rs")),
            Some("src/a.rs".to_string())
        );

        cleanup(&p);
    }

    /// 索引目錄自己的變動不能觸發同步，否則寫入索引會再喚醒一次同步。
    #[test]
    fn changes_inside_the_index_directory_are_ignored() {
        let p = indexed_project("watch-selfloop", &[("src/a.rs", "fn one() {}\n")]);

        assert_eq!(
            indexable(&p, &p.root().join(DIR_NAME).join("graph.db")),
            None
        );

        cleanup(&p);
    }

    #[test]
    fn files_with_no_extractor_are_ignored() {
        let p = indexed_project("watch-other", &[("src/a.rs", "fn one() {}\n")]);

        assert_eq!(indexable(&p, &p.root().join("README.md")), None);

        cleanup(&p);
    }

    /// 刪除事件送來的路徑在磁碟上已經不存在，仍然要處理。
    #[test]
    fn a_deleted_file_is_still_indexable() {
        let p = indexed_project("watch-deleted", &[("src/a.rs", "fn one() {}\n")]);
        std::fs::remove_file(p.root().join("src/a.rs")).unwrap();

        assert_eq!(
            indexable(&p, &p.root().join("src/a.rs")),
            Some("src/a.rs".to_string())
        );

        cleanup(&p);
    }

    #[test]
    fn paths_outside_the_project_are_ignored() {
        let p = indexed_project("watch-outside", &[("src/a.rs", "fn one() {}\n")]);

        assert_eq!(indexable(&p, Path::new("/somewhere/else/x.rs")), None);

        cleanup(&p);
    }

    #[test]
    fn an_event_contributes_every_indexable_path_it_carries() {
        let p = indexed_project("watch-collect", &[("src/a.rs", "fn one() {}\n")]);

        let event = Event {
            kind: notify::EventKind::Any,
            paths: vec![
                p.root().join("src/a.rs"),
                p.root().join("README.md"),
                p.root().join("src/a.rs"),
            ],
            attrs: Default::default(),
        };

        let mut out = BTreeSet::new();
        collect(&p, &event, &mut out);
        assert_eq!(out.len(), 1, "同一個檔案被排了兩次：{out:?}");

        cleanup(&p);
    }

    /// 一個帶著給定路徑的事件。
    fn event(paths: Vec<std::path::PathBuf>) -> Event {
        Event {
            kind: notify::EventKind::Any,
            paths,
            attrs: Default::default(),
        }
    }

    /// 一次存檔的連續事件要併成一批，只重新解析一次。
    #[test]
    fn events_arriving_together_form_a_single_batch() {
        let p = indexed_project("watch-batch", &[("src/a.rs", "fn one() {}\n")]);
        let (tx, rx) = mpsc::channel();

        tx.send(event(vec![p.root().join("src/a.rs")])).unwrap();
        tx.send(event(vec![p.root().join("src/b.rs")])).unwrap();
        drop(tx);

        let batch = next_batch(&p, &rx).unwrap();
        assert_eq!(batch.len(), 2, "{batch:?}");
        assert!(next_batch(&p, &rx).is_none(), "佇列關閉之後還在等");

        cleanup(&p);
    }

    /// 靜下來之後才動手，隔得夠遠的兩次存檔各自成批。
    #[test]
    fn events_separated_by_a_pause_form_separate_batches() {
        let p = indexed_project("watch-pause", &[("src/a.rs", "fn one() {}\n")]);
        let (tx, rx) = mpsc::channel();

        let root = p.root().to_path_buf();
        let sender = std::thread::spawn(move || {
            tx.send(event(vec![root.join("src/a.rs")])).unwrap();
            std::thread::sleep(DEBOUNCE * 3);
            tx.send(event(vec![root.join("src/b.rs")])).unwrap();
        });

        let first = next_batch(&p, &rx).unwrap();
        let second = next_batch(&p, &rx).unwrap();
        sender.join().unwrap();

        assert_eq!(first.len(), 1, "{first:?}");
        assert_eq!(second.len(), 1, "{second:?}");
        assert_ne!(first, second);

        cleanup(&p);
    }

    /// 事件全部落在索引目錄裡時，這一批沒有東西要做。
    #[test]
    fn a_batch_of_ignored_paths_is_empty() {
        let p = indexed_project("watch-empty", &[("src/a.rs", "fn one() {}\n")]);
        let (tx, rx) = mpsc::channel();

        tx.send(event(vec![p.root().join(DIR_NAME).join("graph.db")]))
            .unwrap();
        drop(tx);

        assert!(next_batch(&p, &rx).unwrap().is_empty());

        cleanup(&p);
    }

    #[test]
    fn applying_a_batch_reindexes_every_file_in_it() {
        let p = indexed_project(
            "watch-apply",
            &[("src/a.rs", "fn one() {}\n"), ("src/b.rs", "fn two() {}\n")],
        );
        let mut store = Store::open(&p.db_path()).unwrap();

        std::fs::write(p.root().join("src/a.rs"), "fn one_renamed() {}\n").unwrap();

        let paths = BTreeSet::from(["src/a.rs".to_string(), "src/b.rs".to_string()]);
        let batch = apply(&p, &mut store, paths).unwrap();

        assert_eq!(batch.paths, vec!["src/a.rs"], "沒變的檔案也被列進去了");
        assert_eq!(batch.report.updated, 1);
        assert_eq!(batch.report.unchanged, 1);

        drop(store);
        cleanup(&p);
    }

    /// 存檔之後索引要跟著更新，這是整個監看模式存在的理由。
    ///
    /// 監看在另一條執行緒上跑，主執行緒等它回報；等不到就讓測試失敗，
    /// 不能無限期地卡住。
    #[test]
    fn saving_a_file_updates_the_index() {
        let p = indexed_project("watch-live", &[("src/a.rs", "fn before() {}\n")]);
        let (done, wait) = mpsc::channel();

        let watched = Project::discover(p.root()).unwrap();
        std::thread::spawn(move || {
            let mut store = Store::open(&watched.db_path()).unwrap();
            run(&watched, &mut store, |batch| {
                let _ = done.send(batch.paths.clone());
                false
            })
            .ok();
        });

        // 監看器註冊需要一點時間，太早寫入會漏掉事件。
        std::thread::sleep(DEBOUNCE);
        std::fs::write(p.root().join("src/a.rs"), "fn after() {}\n").unwrap();

        let touched = wait
            .recv_timeout(Duration::from_secs(10))
            .expect("監看沒有回報任何變更");
        assert!(touched.contains(&"src/a.rs".to_string()), "{touched:?}");

        let store = Store::open(&p.db_path()).unwrap();
        let name: String = store
            .conn()
            .query_row("SELECT name FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "after", "存檔之後索引還是舊的");

        drop(store);
        cleanup(&p);
    }
}
