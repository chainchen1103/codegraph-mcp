//! CLI 表面的契約（整合測試）。
//!
//! 直接跑編譯出來的 binary，不是呼叫 lib——`main.rs` 的參數解析、
//! 結束碼、說明文字都是使用者真正會碰到的介面。
//!
//! Stage 0 的子命令還是 `todo!()`，所以這裡只驗證「解析層」的行為。
//! 子命令真正的行為測試會隨著 Stage 1 / Stage 3 一起加。

use std::path::PathBuf;
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_codegraph"))
}

/// 帶 repo 邊界（`.git`）的暫存目錄——避免往上找索引時撞到
/// 家目錄可能存在的 `.codegraph/`。
fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("codegraph-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    dir
}

fn run(args: &[&str]) -> (bool, String, String) {
    let out = bin().args(args).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn help_lists_every_subcommand() {
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success(), "--help 應該正常結束");

    let stdout = String::from_utf8_lossy(&out.stdout);
    for sub in ["init", "index", "status"] {
        assert!(stdout.contains(sub), "說明裡少了子命令 `{sub}`：{stdout}");
    }
}

#[test]
fn version_is_reported() {
    let out = bin().arg("--version").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "--version 沒有印出 Cargo.toml 的版本：{stdout}"
    );
}

/// 沒帶子命令是使用錯誤，不是崩潰。clap 用結束碼 2 表示這件事。
#[test]
fn missing_subcommand_is_a_usage_error_not_a_panic() {
    let out = bin().output().unwrap();
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Usage"), "錯誤訊息應該附上用法：{stderr}");
    assert!(
        !stderr.contains("panicked"),
        "參數錯誤不該是 panic：{stderr}"
    );
}

#[test]
fn unknown_subcommand_is_rejected() {
    let out = bin().arg("frobnicate").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// 每個子命令都接受一個選擇性的路徑參數。
#[test]
fn subcommands_accept_an_optional_path() {
    for sub in ["init", "index", "status"] {
        let out = bin().args([sub, "--help"]).output().unwrap();
        assert!(out.status.success(), "`{sub} --help` 失敗");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("[PATH]"),
            "`{sub}` 沒有選擇性的 PATH 參數：{stdout}"
        );
    }
}

/// 使用者真正會走的第一段路：init 之後 status 要看得到一個空索引。
#[test]
fn init_then_status_is_the_happy_path() {
    let root = tmpdir("happy");
    let path = root.to_str().unwrap();

    let (ok, stdout, stderr) = run(&["init", path]);
    assert!(ok, "init 失敗：{stderr}");
    assert!(stdout.contains("已建立索引目錄"), "{stdout}");
    assert!(root.join(".codegraph").join("graph.db").is_file());
    assert!(root.join(".codegraph").join("config.toml").is_file());

    let (ok, stdout, stderr) = run(&["status", path]);
    assert!(ok, "status 失敗：{stderr}");
    assert!(stdout.contains("符號      0"), "{stdout}");
    assert!(stdout.contains("索引是空的"), "{stdout}");

    std::fs::remove_dir_all(&root).ok();
}

/// 重跑 init 是安全的，而且要說明自己什麼都沒做。
#[test]
fn init_twice_is_safe() {
    let root = tmpdir("twice");
    let path = root.to_str().unwrap();

    assert!(run(&["init", path]).0);
    let (ok, stdout, _) = run(&["init", path]);
    assert!(ok);
    assert!(stdout.contains("索引已存在"), "{stdout}");

    std::fs::remove_dir_all(&root).ok();
}

/// 沒有索引的目錄：status 要以錯誤結束，但訊息必須是可行動的引導，
/// 不是 panic 或 SQLite 的原始錯誤。
#[test]
fn status_without_an_index_explains_itself() {
    let root = tmpdir("bare");
    let (ok, _, stderr) = run(&["status", root.to_str().unwrap()]);

    assert!(!ok, "沒有索引時 status 不該回報成功");
    assert!(stderr.contains(".codegraph"), "訊息要指出缺什麼：{stderr}");
    assert!(!stderr.contains("panicked"), "不該是 panic：{stderr}");

    std::fs::remove_dir_all(&root).ok();
}
