//! gdb 子进程管理（阶段 3）：WSL 发行版 / SSH 主机探测、gdb 可用性检查、进程启动。
//!
//! IDE 在 Windows，gdb 在远端运行：
//! - WSL：`wsl.exe -d <distro> --exec bash -l -c 'exec gdb ...'`；
//! - SSH：`ssh -T ... <user>@<host> "bash -l -c 'exec gdb ...'"`。
//!
//! stdio 即 MI 通道。与 ide-lsp 的 clangd 启动模式保持一致：
//! - `LITEIDE_GDB_CMD` 环境变量整条覆盖启动命令（mock/原生 Linux 场景，测试用）
//! - `LITEIDE_WSL_DISTRO` 指定发行版

use ide_core::remote::RemoteTarget;
use std::process::Stdio;
use tokio::process::{Child, Command};

/// `wsl.exe` 可执行文件名（Windows 侧）。
pub const WSL_EXE: &str = "wsl.exe";

/// 远端解析 gdb 的 shell 片段（devtoolset 等非 PATH 安装也能找到）。
fn gdb_resolver() -> String {
    ide_core::remote::RemoteTool::Gdb.resolver_shell()
}

/// 优先探测的发行版名（实际安装名 > 文档名 > 泛名）。
pub const PREFERRED_DISTROS: &[&str] = &["Ubuntu2404", "Ubuntu-24.04", "Ubuntu"];

/// 探测类 wsl.exe 调用的超时（残留 wsl 桥进程会阻塞新调用，必须兜底）。
const DETECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Windows 下隐藏控制台窗口（GUI 程序不得弹出黑框）。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 从 `wsl -l -q` 输出解析发行版列表（无 BOM UTF-16LE / 带 BOM / UTF-8 三态）。
fn decode_wsl_list(bytes: &[u8]) -> Vec<String> {
    let text = if starts_with_bom(bytes) {
        decode_utf16le(&bytes[2..]).unwrap_or_default()
    } else if looks_utf16le(bytes) {
        decode_utf16le(bytes).unwrap_or_default()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.lines()
        .map(|l| {
            l.trim()
                .trim_matches(|c: char| c == '\0' || c.is_whitespace())
                .to_string()
        })
        .filter(|l| !l.is_empty())
        .collect()
}

fn starts_with_bom(b: &[u8]) -> bool {
    b.len() >= 2 && b[0] == 0xff && b[1] == 0xfe
}

fn looks_utf16le(b: &[u8]) -> bool {
    if b.len() < 4 || !b.len().is_multiple_of(2) {
        return false;
    }
    let zeros = b
        .iter()
        .enumerate()
        .filter(|(i, x)| i % 2 == 1 && **x == 0)
        .count();
    zeros * 4 >= b.len() / 2
}

fn decode_utf16le(b: &[u8]) -> Option<String> {
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// 枚举 WSL 发行版名（带超时兜底，防止残留 wsl 进程持有互斥锁导致挂死）。
pub async fn list_wsl_distros() -> Vec<String> {
    let fut = Command::new(WSL_EXE).args(["-l", "-q"]).output();
    match tokio::time::timeout(DETECT_TIMEOUT, fut).await {
        Ok(Ok(o)) if o.status.success() => decode_wsl_list(&o.stdout),
        Ok(Ok(o)) => {
            tracing::warn!(
                "wsl -l -q 失败 (exit {:?}): {}",
                o.status.code(),
                String::from_utf8_lossy(&o.stderr).trim()
            );
            Vec::new()
        }
        Ok(Err(e)) => {
            tracing::warn!("wsl -l -q 调用失败: {e}");
            Vec::new()
        }
        Err(_) => {
            tracing::warn!("wsl -l -q 超时（可能有残留 wsl 进程占用互斥锁）");
            Vec::new()
        }
    }
}

/// 选择一个 WSL 发行版：环境变量 > 优先名 > 含 "ubuntu" 的 > 第一个。
pub async fn pick_wsl_distro() -> Option<String> {
    if let Ok(env) = std::env::var("LITEIDE_WSL_DISTRO") {
        if !env.trim().is_empty() {
            return Some(env.trim().to_string());
        }
    }
    let distros = list_wsl_distros().await;
    for pref in PREFERRED_DISTROS {
        if let Some(d) = distros.iter().find(|d| d.eq_ignore_ascii_case(pref)) {
            return Some(d.clone());
        }
    }
    distros
        .iter()
        .find(|d| d.to_ascii_lowercase().contains("ubuntu"))
        .cloned()
        .or_else(|| distros.first().cloned())
}

/// 检查 gdb 是否在指定发行版的 PATH 中。返回其路径。
pub async fn probe_gdb(wsl_distro: &str) -> Result<String, String> {
    let fut = Command::new(WSL_EXE)
        .args(["-d", wsl_distro, "--", "sh", "-lc", "command -v gdb"])
        .output();
    let out = match tokio::time::timeout(DETECT_TIMEOUT, fut).await {
        Ok(r) => r,
        Err(_) => {
            return Err(
                "wsl.exe 探测超时（可能有残留 wsl 进程占用互斥锁，重启 IDE 可恢复）".to_string(),
            );
        }
    };
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                Err("gdb 不在 WSL 的 PATH 中".to_string())
            } else {
                Ok(s)
            }
        }
        Ok(o) => Err(format!(
            "gdb 探测失败 (exit {:?}): {}",
            o.status.code(),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("wsl.exe 调用失败: {e}")),
    }
}

/// 是否使用环境变量 `LITEIDE_GDB_CMD` 覆盖启动命令（测试/原生 Linux 场景）。
pub fn using_custom_command() -> bool {
    std::env::var("LITEIDE_GDB_CMD")
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// 启动 gdb 子进程（MI2 解释器）。
///
/// 若设置了 `LITEIDE_GDB_CMD`，直接按空白拆分成程序+参数执行（mock/原生 Linux）；
/// 否则通过 `wsl.exe -d <distro> -- gdb -q -nx --interpreter=mi2` 桥接。
pub fn spawn_gdb(wsl_distro: &str) -> std::io::Result<Child> {
    if let Ok(cmdline) = std::env::var("LITEIDE_GDB_CMD") {
        let mut parts = cmdline.split_whitespace();
        let Some(program) = parts.next() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "LITEIDE_GDB_CMD 为空",
            ));
        };
        if program.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "LITEIDE_GDB_CMD 为空",
            ));
        }
        let mut cmd = Command::new(program);
        cmd.args(parts);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        return cmd.spawn();
    }

    let mut cmd = Command::new(WSL_EXE);
    cmd.args([
        "-d",
        wsl_distro,
        "--",
        "gdb",
        "-q",               // quiet：不打印版本横幅
        "-nx",              // 不加载 .gdbinit（确定性）
        "--interpreter=mi2",
    ]);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()
}

/// 检查 gdb 是否在 SSH 主机的 PATH（或常见安装位置）中。返回其路径。
pub async fn probe_gdb_remote(target: &RemoteTarget) -> Result<String, String> {
    let script = format!("{}printf '%s' \"$GDB\"", gdb_resolver());
    let (program, argv) = target.shell_argv(&script);
    let mut c = Command::new(&program);
    c.args(&argv);
    #[cfg(windows)]
    {
        c.creation_flags(CREATE_NO_WINDOW);
    }
    let out = match tokio::time::timeout(DETECT_TIMEOUT, c.output()).await {
        Ok(r) => r,
        Err(_) => {
            return Err(format!(
                "SSH 探测超时（{}，检查网络/免密登录）",
                target.label()
            ));
        }
    };
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                Err(format!("gdb 不在 {} 的 PATH 中", target.label()))
            } else {
                Ok(s)
            }
        }
        Ok(o) => Err(format!(
            "gdb 探测失败（{}）: {}",
            target.label(),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("ssh 调用失败: {e}")),
    }
}

/// 通过 SSH 启动 gdb（MI2 解释器）。
pub fn spawn_gdb_remote(target: &RemoteTarget) -> std::io::Result<Child> {
    // -q 不打印版本横幅；-nx 不加载 .gdbinit（确定性）。
    let script = format!("{}exec \"$GDB\" -q -nx --interpreter=mi2", gdb_resolver());
    let (program, argv) = target.shell_argv(&script);
    let mut cmd = Command::new(&program);
    cmd.args(&argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_utf16le_without_bom() {
        let mut raw = Vec::new();
        for ch in "Ubuntu2404\r\nCentOS7\r\n".encode_utf16() {
            raw.extend_from_slice(&ch.to_le_bytes());
        }
        assert!(looks_utf16le(&raw));
        let distros = decode_wsl_list(&raw);
        assert_eq!(distros, vec!["Ubuntu2404", "CentOS7"]);
    }

    #[test]
    fn decode_utf16le_with_bom() {
        let mut raw = vec![0xff, 0xfe];
        for ch in "Ubuntu2404\r\n".encode_utf16() {
            raw.extend_from_slice(&ch.to_le_bytes());
        }
        let distros = decode_wsl_list(&raw);
        assert_eq!(distros, vec!["Ubuntu2404"]);
    }

    #[test]
    fn decode_utf8_plain() {
        let raw = b"Ubuntu-24.04\nDebian\n";
        let distros = decode_wsl_list(raw);
        assert_eq!(distros, vec!["Ubuntu-24.04", "Debian"]);
    }

    #[test]
    fn constants_sane() {
        assert!(!WSL_EXE.is_empty());
        #[cfg(windows)]
        assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
    }

    #[test]
    fn ssh_spawn_builds_ssh_argv_with_gdb_script() {
        let target = RemoteTarget::ssh("fubin", "192.168.77.6", 22);
        let (program, argv) = target.shell_argv("exec gdb -q -nx");
        assert!(program.ends_with("ssh"));
        assert!(argv.contains(&"fubin@192.168.77.6".to_string()));
        assert!(argv.last().unwrap().contains("-nx"));
        assert!(gdb_resolver().contains("devtoolset"));
    }

    /// 真实 SSH 验收：远端解析 gdb（devtoolset 等非 PATH 安装也能找到）。
    ///
    /// `cargo test -p ide-dap ssh_probe_gdb -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "需要一台可免密登录的 SSH 主机（LITEIDE_SSH_TEST_HOST/USER）"]
    async fn ssh_probe_gdb_finds_binary() {
        let user = std::env::var("LITEIDE_SSH_TEST_USER").unwrap_or_else(|_| "fubin".to_string());
        let host = std::env::var("LITEIDE_SSH_TEST_HOST")
            .unwrap_or_else(|_| "192.168.77.6".to_string());
        let target = RemoteTarget::ssh(user, host, 22);
        let path = probe_gdb_remote(&target).await.expect("远端应能解析出 gdb");
        assert!(path.ends_with("gdb"), "应返回 gdb 路径: {path}");
    }
}
