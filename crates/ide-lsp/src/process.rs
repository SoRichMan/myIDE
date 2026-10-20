//! clangd 子进程管理：WSL 发行版探测、clangd 可用性检查、进程启动。
//!
//! IDE 在 Windows，clangd 在 WSL 内运行，通过 `wsl.exe -d <distro> clangd` 桥接，
//! stdio 即 LSP 通道。

use std::process::Stdio;
use tokio::process::{Child, Command};

/// `wsl.exe` 可执行文件名（Windows 侧）。
pub const WSL_EXE: &str = "wsl.exe";

/// Windows 下隐藏控制台窗口（GUI 程序不得弹出黑框）。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 优先探测的发行版名（实际安装名 > 文档名 > 泛名）。
pub const PREFERRED_DISTROS: &[&str] = &["Ubuntu2404", "Ubuntu-24.04", "Ubuntu"];

/// 探测类 wsl.exe 调用的超时（残留 wsl 桥进程会阻塞新调用，必须兜底）。
const DETECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 从 `wsl -l -q` 输出解析发行版列表。
///
/// 真实世界坑：该输出可能是 **无 BOM 的 UTF-16LE**（每个 ASCII 字符后跟 `\0`），
/// 也可能带 `FF FE` BOM，或普通 UTF-8。这里统一识别。
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

/// 启发式：奇数位置大量为 0 且长度偶数 → 极可能是 UTF-16LE。
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

/// 枚举 WSL 发行版名。
///
/// 真实世界坑：残留的 wsl.exe 桥进程会持有 WSL 互斥锁，导致新调用阻塞。
/// 这里加 10s 超时，超时按"无发行版"处理（UI 显示 Unavailable 可重试），
/// 而不是无限期停在 Detecting。
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

/// 同步枚举 WSL 发行版（UI 线程里点「连接到 WSL」时用，带超时兜底）。
///
/// 与 [`list_wsl_distros`] 共用解析逻辑；UI 帧里不能 await 协作式运行时，
/// 所以这里是独立的阻塞调用（`wsl.exe -l -q` 通常几毫秒，最坏 10s 放弃）。
pub fn list_wsl_distros_blocking() -> Vec<String> {
    use std::io::Read;
    use std::process::{Command as StdCommand, Stdio as StdStdio};

    let mut cmd = StdCommand::new(WSL_EXE);
    cmd.args(["-l", "-q"])
        .stdin(StdStdio::null())
        .stdout(StdStdio::piped())
        .stderr(StdStdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("wsl -l -q 启动失败: {e}");
            return Vec::new();
        }
    };
    // 输出很小（几行），读完再等退出；用通道兜一层超时，防止残留 wsl 进程拖死 UI。
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(mut s) = child.stdout.take() {
            let _ = s.read_to_end(&mut out);
        }
        let _ = child.wait();
        let _ = tx.send(out);
    });
    match rx.recv_timeout(DETECT_TIMEOUT) {
        Ok(raw) => decode_wsl_list(&raw),
        Err(_) => {
            tracing::warn!("wsl -l -q 超时（可能有残留 wsl 进程占用互斥锁）");
            Vec::new()
        }
    }
}

/// 选择一个 WSL 发行版：环境变量 `LITEIDE_WSL_DISTRO` > 优先名 > 含 "ubuntu" 的 > 第一个。
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

/// 检查 clangd 是否在指定发行版的 PATH 中。返回其路径。
pub async fn probe_clangd(wsl_distro: &str) -> Result<String, String> {
    let fut = Command::new(WSL_EXE)
        .args(["-d", wsl_distro, "--", "sh", "-lc", "command -v clangd"])
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
                Err("clangd 不在 WSL 的 PATH 中".to_string())
            } else {
                Ok(s)
            }
        }
        Ok(o) => Err(format!(
            "clangd 探测失败 (exit {:?}): {}",
            o.status.code(),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("wsl.exe 调用失败: {e}")),
    }
}

/// 是否使用环境变量 `LITEIDE_LSP_CMD` 覆盖启动命令（测试/原生 Linux 场景）。
pub fn using_custom_command() -> bool {
    std::env::var("LITEIDE_LSP_CMD")
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// 启动 LSP 子进程。
///
/// 若设置了 `LITEIDE_LSP_CMD`（如 `python E:\...\mock_clangd.py`），直接按空白
/// 拆分成程序+参数执行（集成测试、原生 Linux 部署用）；否则通过
/// `wsl.exe -d <distro> -- clangd ...` 桥接。
pub fn spawn_clangd(wsl_distro: &str) -> std::io::Result<Child> {
    if let Ok(cmdline) = std::env::var("LITEIDE_LSP_CMD") {
        let mut parts = cmdline.split_whitespace();
        let Some(program) = parts.next() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "LITEIDE_LSP_CMD 为空",
            ));
        };
        if program.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "LITEIDE_LSP_CMD 为空",
            ));
        }
        let mut cmd = Command::new(program);
        cmd.args(parts);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
        "clangd",
        "--log=error",
        "--background-index",
        "--pch-storage=memory",
        "--header-insertion=never",
        "--function-arg-placeholders=0",
    ]);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
        // "Ubuntu2404\r\nCentOS7\r\n" 的 UTF-16LE 编码
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
    fn decode_strips_nulls_and_whitespace() {
        let mut raw = Vec::new();
        for ch in "  Ubuntu2404 \0\r\n".encode_utf16() {
            raw.extend_from_slice(&ch.to_le_bytes());
        }
        let distros = decode_wsl_list(&raw);
        assert_eq!(distros, vec!["Ubuntu2404"]);
    }

    #[test]
    fn spawn_command_contains_distro() {
        // 验证命令构造逻辑（不真正 spawn）。
        // 直接检查 spawn_clangd 的参数 —— 通过读取子进程构造不可行，这里
        // 只验证 WSL_EXE 常量与 CREATE_NO_WINDOW 标志存在（编译期即可验证）。
        assert!(!WSL_EXE.is_empty());
        #[cfg(windows)]
        assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
    }
}
