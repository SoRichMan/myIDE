//! 集成测试：真实 git（Windows git，经 LITEIDE_GIT_CMD override 路径）
//! 执行 init → add → commit → log 全流程。
//!
//! 守护：非 Git 目录时 status 返回空状态而非 panic。

use ide_git::porcelain::parse_porcelain;
use ide_git::process::run_git;
use std::fs;
use std::path::Path;

fn temp_repo() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("liteide_git_it_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // 用 Windows git 初始化（LITEIDE_GIT_CMD=git 走 override）。
    std::env::set_var("LITEIDE_GIT_CMD", "git");
    git(&dir, &["init"]);
    git(&dir, &["config", "user.email", "it@liteide.dev"]);
    git(&dir, &["config", "user.name", "LiteIDE IT"]);
    dir
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git init 失败");
    assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test]
async fn init_add_commit_log_roundtrip() {
    let dir = temp_repo();
    let root = dir.to_string_lossy().to_string();

    fs::write(dir.join("hello.txt"), "hello\n").unwrap();

    // 1. status：应看到未跟踪文件。
    let (ok, out) = run_git(&root, &["status", "--porcelain=v2", "--branch"]).await.unwrap();
    assert!(ok, "status 失败: {out}");
    let st = parse_porcelain(&out);
    assert_eq!(st.branch.as_deref(), Some("master").or(Some("main")));
    assert!(st.entries.iter().any(|e| e.path == "hello.txt"), "entries: {:?}", st.entries);

    // 2. add + commit。
    run_git(&root, &["add", "hello.txt"]).await.unwrap();
    let (ok, out) = run_git(&root, &["commit", "-m", "initial hello"]).await.unwrap();
    assert!(ok, "commit 失败: {out}");

    // 3. log：应出现新提交。
    let (ok, out) = run_git(&root, &[
        "log",
        "--pretty=format:%h%x00%an%x00%ad%x00%s",
        "--date=short",
        "-n",
        "10",
    ]).await.unwrap();
    assert!(ok, "log 失败: {out}");
    let commits = ide_git::log::parse_log(&out);
    assert!(!commits.is_empty());
    assert!(commits[0].subject.contains("initial hello"));

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn non_git_dir_does_not_panic() {
    let dir = std::env::temp_dir().join(format!("liteide_nogit_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    std::env::set_var("LITEIDE_GIT_CMD", "git");
    // git status 在非仓库目录返回非零，不应 panic。
    let (ok, out) = run_git(&dir.to_string_lossy(), &["status", "--porcelain=v2"])
        .await
        .unwrap();
    assert!(!ok, "非仓库目录应返回失败状态");
    assert!(out.is_empty() || out.contains("not a git"));
    let _ = fs::remove_dir_all(&dir);
}
