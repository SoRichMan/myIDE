//! git 子进程调用：WSL / SSH 桥接 + 环境变量覆盖。

use ide_core::remote::{self, RemoteTarget};
pub use ide_core::remote::shell_quote;
use std::time::Duration;
use tokio::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub const WSL_EXE: &str = "wsl.exe";
const GIT_TIMEOUT: Duration = Duration::from_secs(20);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn distro() -> String {
    std::env::var("LITEIDE_WSL_DISTRO").unwrap_or_else(|_| "Ubuntu2404".to_string())
}

/// Windows 路径 → WSL 视角路径。
///
/// - 盘符路径：`E:\\proj` → `/mnt/e/proj`
/// - WSL UNC（远程工作区）：`\\\\wsl.localhost\\Ubuntu2404\\home\\x` → `/home/x`
/// - 已是 POSIX：原样（只把反斜杠换成斜杠）
pub fn win_to_wsl_path(p: &str) -> String {
    let slashed = p.replace('\\', "/");
    for prefix in ["//wsl.localhost/", "//wsl$/"] {
        if let Some(rest) = slashed.strip_prefix(prefix) {
            if let Some((_distro, tail)) = rest.split_once('/') {
                return format!("/{tail}");
            }
        }
    }
    let bytes = p.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let rest = p[2..].trim_start_matches(['\\', '/']);
        format!("/mnt/{drive}/{}", rest.replace('\\', "/"))
    } else {
        slashed
    }
}

/// 组装 `cd <root> && git <args...>`（远端 shell 里执行）。
fn git_script(root_win: &str, args: &[&str]) -> String {
    let root = win_to_wsl_path(root_win);
    let mut argv = String::from("cd ");
    argv.push_str(&shell_quote(&root));
    argv.push_str(" && git");
    for a in args {
        argv.push(' ');
        argv.push_str(&shell_quote(a));
    }
    argv
}

/// 该走哪种传输：SSH 远程工作区（根是 POSIX 路径且当前连接是 SSH）走 `ssh`，
/// 其余一律保持原来的 WSL 行为（本地工程 `E:\proj` 也是 WSL 里的 `/mnt/e/proj`）。
fn transport(root_win: &str, script: &str) -> (String, Vec<String>) {
    match remote::ssh_target_for_path(root_win) {
        Some(t) => t.shell_argv(script),
        None => RemoteTarget::wsl(distro()).shell_argv(script),
    }
}

/// 运行 git 子命令，返回 (success, stdout)。
///
/// `LITEIDE_GIT_CMD` 整条覆盖（测试用），否则
/// `wsl.exe -d <distro> --exec bash -lc "cd <root_wsl> && git <args...>"`，
/// SSH 远程工作区下换成 `ssh ... bash -lc "cd <root> && git <args...>"`。
pub async fn run_git(root_win: &str, args: &[&str]) -> Result<(bool, String), String> {
    if let Ok(cmdline) = std::env::var("LITEIDE_GIT_CMD") {
        if !cmdline.trim().is_empty() {
            let mut parts = cmdline.split_whitespace();
            let prog = parts.next().unwrap_or("git");
            let mut c = Command::new(prog);
            c.args(parts).args(["-C", root_win]).args(args);
            #[cfg(windows)]
            {
                c.creation_flags(CREATE_NO_WINDOW);
            }
            let out = c
                .output()
                .await
                .map_err(|e| format!("LITEIDE_GIT_CMD 启动失败: {e}"))?;
            let ok = out.status.success();
            return Ok((ok, String::from_utf8_lossy(&out.stdout).into_owned()));
        }
    }

    let script = git_script(root_win, args);
    let (program, argv) = transport(root_win, &script);

    let mut c = Command::new(&program);
    c.args(&argv);
    #[cfg(windows)]
    {
        c.creation_flags(CREATE_NO_WINDOW);
    }
    let fut = c.output();
    let out = match tokio::time::timeout(GIT_TIMEOUT, fut).await {
        Ok(r) => r.map_err(|e| format!("git 调用失败: {e}"))?,
        Err(_) => return Err(format!("git 超时（{}s）", GIT_TIMEOUT.as_secs())),
    };
    let ok = out.status.success();
    Ok((ok, String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// 同步地在远端跑一条 shell 命令（阻塞）。
///
/// 只在「切换工作区、发现仓库」这类一次性场合使用；
/// 日常的 git 操作走 `run_git`（异步）。
pub fn run_remote_blocking(target: &RemoteTarget, script: &str) -> Result<String, String> {
    let (program, argv) = target.shell_argv(script);
    let mut c = std::process::Command::new(&program);
    c.args(&argv);
    #[cfg(windows)]
    {
        c.creation_flags(CREATE_NO_WINDOW);
    }
    let out = c
        .output()
        .map_err(|e| format!("远程命令失败（{}）: {e}", target.label()))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 兼容旧接口：在指定 WSL 发行版里跑一条 shell 命令。
pub fn run_wsl_blocking(distro: &str, script: &str) -> Result<String, String> {
    run_remote_blocking(&RemoteTarget::wsl(distro), script)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_to_wsl_drive() {
        assert_eq!(win_to_wsl_path(r"E:\repo"), "/mnt/e/repo");
    }

    #[test]
    fn win_to_wsl_maps_wsl_unc() {
        // 远程工作区（WSL 发行版内的目录）：git 在 WSL 里跑，路径要走 POSIX。
        assert_eq!(
            win_to_wsl_path(r"\\wsl.localhost\Ubuntu2404\home\work\proj"),
            "/home/work/proj"
        );
    }

    #[test]
    fn shell_quote_works() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn git_script_quotes_root_and_args() {
        let s = git_script("/home/u/my proj", &["commit", "-m", "it's ok"]);
        assert_eq!(
            s,
            "cd '/home/u/my proj' && git 'commit' '-m' 'it'\\''s ok'"
        );
    }

    // 传输选择依赖进程级注册表，串行执行避免互相污染。
    #[test]
    fn transport_uses_ssh_only_for_posix_roots() {
        let prev = remote::current_target();
        remote::set_current_target(Some(RemoteTarget::ssh("u", "h", 22)));
        let (prog, _) = transport("/home/u/proj", "echo hi");
        assert!(prog.ends_with("ssh"), "SSH 远程根应走 ssh: {prog}");
        // 本地盘符工程即使连着 SSH，也仍然走 WSL（原来的行为）。
        let (prog, args) = transport(r"E:\proj", "echo hi");
        assert!(prog.ends_with("wsl.exe"), "本地根应保持 WSL: {prog}");
        assert_eq!(args[0], "-d");
        remote::set_current_target(Some(RemoteTarget::wsl("Ubuntu2404")));
        let (prog, _) = transport("/home/u/proj", "echo hi");
        assert!(prog.ends_with("wsl.exe"));
        remote::set_current_target(prev);
    }

    /// 真实 SSH 验收：git 与仓库发现在远端跑（根是 POSIX 路径）。
    ///
    /// `cargo test -p ide-git ssh_remote_git -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "需要一台可免密登录的 SSH 主机（LITEIDE_SSH_TEST_HOST/USER）"]
    async fn ssh_remote_git_and_repo_discovery() {
        let user = std::env::var("LITEIDE_SSH_TEST_USER").unwrap_or_else(|_| "fubin".to_string());
        let host = std::env::var("LITEIDE_SSH_TEST_HOST")
            .unwrap_or_else(|_| "192.168.77.6".to_string());
        let root = std::env::var("LITEIDE_SSH_TEST_ROOT")
            .unwrap_or_else(|_| "/home/fubin/work/candanza".to_string());
        let target = RemoteTarget::ssh(user, host, 22);
        let prev = remote::current_target();
        remote::set_current_target(Some(target.clone()));

        let (ok, out) = run_git(&root, &["rev-parse", "--show-toplevel"])
            .await
            .expect("git 应能启动");
        assert!(ok, "git rev-parse 失败: {out}");
        assert_eq!(out.trim(), root.trim_end_matches('/'));

        let (ok, porcelain) = run_git(&root, &["status", "--porcelain"])
            .await
            .expect("git status 应能启动");
        assert!(ok, "git status 失败: {porcelain}");

        let repos = crate::repos::discover_repos_remote(&root, &target);
        assert!(
            repos.iter().any(|r| r.trim_end_matches('/') == root.trim_end_matches('/')),
            "工作区根应被识别为 git 仓库: {repos:?}"
        );

        remote::set_current_target(prev);
    }
}
