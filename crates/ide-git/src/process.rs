//! git 子进程调用：WSL 桥接 + 环境变量覆盖。

use std::time::Duration;
use tokio::process::Command;

pub const WSL_EXE: &str = "wsl.exe";
const GIT_TIMEOUT: Duration = Duration::from_secs(20);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn distro() -> String {
    std::env::var("LITEIDE_WSL_DISTRO").unwrap_or_else(|_| "Ubuntu2404".to_string())
}

/// Windows 路径 → WSL 挂载路径。
pub fn win_to_wsl_path(p: &str) -> String {
    let bytes = p.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let rest = p[2..].trim_start_matches(['\\', '/']);
        format!("/mnt/{drive}/{}", rest.replace('\\', "/"))
    } else {
        p.replace('\\', "/")
    }
}

fn shell_quote(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_to_wsl_drive() {
        assert_eq!(win_to_wsl_path(r"E:\repo"), "/mnt/e/repo");
    }

    #[test]
    fn shell_quote_works() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }
}
