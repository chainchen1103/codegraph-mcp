//! CLI 表面的契約（整合測試）。
//!
//! 直接跑編譯出來的 binary，不是呼叫 lib——`main.rs` 的參數解析、
//! 結束碼、說明文字都是使用者真正會碰到的介面。
//!
//! Stage 0 的子命令還是 `todo!()`，所以這裡只驗證「解析層」的行為。
//! 子命令真正的行為測試會隨著 Stage 1 / Stage 3 一起加。

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_codegraph"))
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
/// 這裡只檢查參數解析接受它——實際行為在後續 Stage。
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
