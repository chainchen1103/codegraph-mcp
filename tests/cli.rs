//! 命令列介面的整合測試。
//!
//! 直接執行編譯出來的執行檔，涵蓋參數解析、結束碼與輸出內容。

use std::path::PathBuf;
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_codegraph"))
}

/// 建立帶有 repo 邊界的暫存目錄，避免往上找到家目錄的索引。
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
    for sub in ["init", "index", "status", "outline"] {
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

/// 沒帶子命令屬於使用錯誤，clap 以結束碼 2 表示。
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

/// 沒有索引時 status 以錯誤結束，訊息必須指出缺少什麼。
#[test]
fn status_without_an_index_explains_itself() {
    let root = tmpdir("bare");
    let (ok, _, stderr) = run(&["status", root.to_str().unwrap()]);

    assert!(!ok, "沒有索引時 status 不該回報成功");
    assert!(stderr.contains(".codegraph"), "訊息要指出缺什麼：{stderr}");
    assert!(!stderr.contains("panicked"), "不該是 panic：{stderr}");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn index_then_status_reports_real_numbers() {
    let root = tmpdir("index");
    let path = root.to_str().unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "fn one() {}\nfn two() {}\n").unwrap();

    assert!(run(&["init", path]).0);

    let (ok, stdout, stderr) = run(&["index", path]);
    assert!(ok, "index 失敗：{stderr}");
    assert!(stdout.contains("檔案      1"), "{stdout}");
    assert!(stdout.contains("符號      2"), "{stdout}");

    let (ok, stdout, _) = run(&["status", path]);
    assert!(ok);
    assert!(stdout.contains("符號      2"), "{stdout}");
    assert!(!stdout.contains("索引是空的"), "{stdout}");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn index_without_an_index_directory_explains_itself() {
    let root = tmpdir("index-bare");
    let (ok, _, stderr) = run(&["index", root.to_str().unwrap()]);

    assert!(!ok);
    assert!(stderr.contains(".codegraph"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn outline_prints_the_structure_of_a_file() {
    let root = tmpdir("outline");
    let file = root.join("a.rs");
    std::fs::write(&file, "/// 說明\npub fn open() -> u8 {\n    1\n}\n").unwrap();

    let (ok, stdout, stderr) = run(&["outline", file.to_str().unwrap()]);
    assert!(ok, "outline 失敗：{stderr}");
    assert!(stdout.contains("open"), "{stdout}");
    assert!(stdout.contains("2-4"), "{stdout}");

    std::fs::remove_dir_all(&root).ok();
}
