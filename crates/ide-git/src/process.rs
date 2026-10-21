//! git 子进程调用：WSL 桥接 + 环境变量覆盖。

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

/// shell 引号（命令拼装用）。
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 运行 git 子命令，返回 (success, stdout)。
///
/// `LITEIDE_GIT_CMD` 整条覆盖（测试用），否则
/// `wsl.exe -d <distro> -- bash -lc "cd <root_wsl> && git <args...>"`。
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

    let root_wsl = win_to_wsl_path(root_win);
    let distro = distro();
    let mut argv = String::from("cd ");
    argv.push_str(&shell_quote(&root_wsl));
    argv.push_str(" && git");
    for a in args {
        argv.push(' ');
        argv.push_str(&shell_quote(a));
    }

    let mut c = Command::new(WSL_EXE);
    c.args(["-d", &distro, "--", "bash", "-lc", &argv]);
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

/// 同步地在 WSL 里跑一条 shell 命令（阻塞）。
///
/// 只在「切换工作区、发现仓库」这类一次性场合使用；
/// 日常的 git 操作走 `run_git`（异步）。
pub fn run_wsl_blocking(distro: &str, script: &str) -> Result<String, String> {
    let mut c = std::process::Command::new(WSL_EXE);
    c.args(["-d", distro, "--", "bash", "-lc", script]);
    #[cfg(windows)]
    {
        c.creation_flags(CREATE_NO_WINDOW);
    }
    let out = c
        .output()
        .map_err(|e| format!("wsl 命令失败: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
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
}
