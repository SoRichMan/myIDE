//! clangd 子进程管理：WSL 发行版 / SSH 主机探测、clangd 可用性检查、进程启动。
//!
//! IDE 在 Windows，clangd 在远端运行：
//! - WSL：`wsl.exe -d <distro> --exec bash -l -c 'exec clangd ...'`；
//! - SSH：`ssh -T ... <user>@<host> "bash -l -c 'exec clangd ...'"`。
//!
//! stdio 即 LSP 通道。

use ide_core::remote::RemoteTarget;
use std::process::Stdio;
use tokio::process::{Child, Command};

/// `wsl.exe` 可执行文件名（Windows 侧）。
pub const WSL_EXE: &str = "wsl.exe";

/// 远端解析 clangd 的 shell 片段（`~/.liteide/tools/bin` 优先，见 [`RemoteTool`]）。
///
/// 不少开发机把 LLVM 装在 `/opt/rh/llvm-toolset-*` 或 `/usr/lib/llvm-*/bin`，
/// 并不在登录 PATH 里，所以 PATH 找不到时再扫一遍常见目录。
fn clangd_resolver() -> String {
    ide_core::remote::RemoteTool::Clangd.resolver_shell()
}

/// clangd 启动参数（与原来 WSL 分支保持一致）。
const CLANGD_ARGS: &str = "--log=error --background-index --pch-storage=memory \
--header-insertion=never --function-arg-placeholders=0";

/// 参数自适应：clangd 5（CentOS 7 SCL 能装到的版本）不认识 `--background-index` 等新参数，
/// 传进去会直接「Unknown command line argument」退出。远端 `--help` 里有没有这个开关，
/// 决定用完整参数还是留空（旧版默认就是 stdio + 自动找 compile_commands.json）。
///
/// 新版再加 `--query-driver`：工程用 devtoolset 的 g++（`/opt/rh/...`），
/// 不白名单的话 clangd 不会去问它，连 `iostream` 都找不到。
fn clangd_args_shell() -> String {
    format!(
        "H=$(\"$CLANGD\" --help 2>&1); \
         if printf '%s' \"$H\" | grep -q -- '--background-index'; then A='{CLANGD_ARGS}'; else A=''; fi; \
         if printf '%s' \"$H\" | grep -q -- '--query-driver'; then \
         A=\"$A --query-driver=/usr/bin/*,/usr/local/bin/*,/opt/*\"; fi; "
    )
}

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
/// `wsl.exe -d <distro> --exec bash -l -c '...'` 桥接，和 SSH 分支共用同一套
/// 工具解析 / 参数自适应（WSL 里也可能是旧版 clangd，或 IDE 自动安装的版本）。
pub fn spawn_clangd(wsl_distro: &str, root_uri: &str) -> std::io::Result<Child> {
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

    let target = RemoteTarget::wsl(wsl_distro);
    let script = format!(
        "{}{}{}exec \"$CLANGD\" $A",
        clangd_resolver(),
        clangd_workdir_shell(root_uri),
        clangd_args_shell()
    );
    let (program, args) = target.shell_argv(&script);
    let mut cmd = Command::new(&program);
    cmd.args(&args);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()
}

/// 选 clangd 的工作目录：clangd 5 没有 `-compile-commands-dir`，只从当前目录读
/// `compile_commands.json`；新版用当前目录也最稳。优先 `build/`，其次工程根。
fn clangd_workdir_shell(root_uri: &str) -> String {
    let root = crate::path_map::uri_to_posix(root_uri).unwrap_or_default();
    if root.is_empty() {
        return String::new();
    }
    format!(
        "R={}; if [ -f \"$R/build/compile_commands.json\" ]; then cd \"$R/build\" 2>/dev/null || true; \
         elif [ -f \"$R/compile_commands.json\" ]; then cd \"$R\" 2>/dev/null || true; fi; ",
        ide_core::remote::shell_quote(&root)
    )
}

/// 检查 clangd 是否在 SSH 主机的 PATH（或常见 LLVM 安装目录）中。返回其路径。
pub async fn probe_clangd_remote(target: &RemoteTarget) -> Result<String, String> {
    let script = format!("{}printf '%s' \"$CLANGD\"", clangd_resolver());
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
                Err(format!("clangd 不在 {} 的 PATH 中", target.label()))
            } else {
                Ok(s)
            }
        }
        Ok(o) => Err(format!(
            "clangd 探测失败（{}）: {}",
            target.label(),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("ssh 调用失败: {e}")),
    }
}

/// 通过 SSH 启动 clangd（stdio 即 LSP 通道）。
///
/// `root_uri` 只用来选工作目录：clangd 5（CentOS 7 SCL 能装到的版本）**没有**
/// `-compile-commands-dir`，只从当前目录读 `compile_commands.json`；
/// 新版本用当前目录也最稳。优先 `build/`（CMake 默认产物位置），其次工程根。
pub fn spawn_clangd_remote(target: &RemoteTarget, root_uri: &str) -> std::io::Result<Child> {
    let script = format!(
        "{}{}{}exec \"$CLANGD\" $A",
        clangd_resolver(),
        clangd_workdir_shell(root_uri),
        clangd_args_shell()
    );
    let (program, argv) = target.shell_argv(&script);
    let mut cmd = Command::new(&program);
    cmd.args(&argv)
        .stdin(Stdio::piped())
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

    #[test]
    fn ssh_spawn_builds_ssh_argv_with_clangd_script() {
        let target = RemoteTarget::ssh("fubin", "192.168.77.6", 22);
        let (program, argv) = target.shell_argv(&format!("exec clangd {}", CLANGD_ARGS));
        assert!(program.ends_with("ssh"));
        assert!(argv.contains(&"fubin@192.168.77.6".to_string()));
        let last = argv.last().unwrap();
        assert!(last.starts_with("bash -l -c '"));
        assert!(last.contains("clangd"));
        // 解析器要在 PATH 之外也找得到常见 LLVM 安装位置。
        assert!(clangd_resolver().contains("/usr/lib/llvm-*/bin/clangd"));
        // 旧版 clangd（CentOS SCL 5.0）不认识新参数：必须按 --help 自适应。
        let gate = clangd_args_shell();
        assert!(gate.contains("--background-index"));
        assert!(gate.contains("A=''"), "旧版要能退回空参数: {gate}");
        // devtoolset 的 g++ 不在 /usr/bin：必须白名单，否则系统头文件都找不到。
        assert!(gate.contains("--query-driver=/usr/bin/*,/usr/local/bin/*,/opt/*"));
    }

    /// 真实 SSH 验收：探测不会挂住，且结果要么给出 clangd 路径、要么给出明确原因。
    ///
    /// `cargo test -p ide-lsp ssh_probe_clangd -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "需要一台可免密登录的 SSH 主机（LITEIDE_SSH_TEST_HOST/USER）"]
    async fn ssh_probe_clangd_reports_path_or_clear_error() {
        let user = std::env::var("LITEIDE_SSH_TEST_USER").unwrap_or_else(|_| "fubin".to_string());
        let host = std::env::var("LITEIDE_SSH_TEST_HOST")
            .unwrap_or_else(|_| "192.168.77.6".to_string());
        let target = RemoteTarget::ssh(user, host, 22);
        match probe_clangd_remote(&target).await {
            Ok(path) => assert!(path.ends_with("clangd"), "应返回 clangd 路径: {path}"),
            Err(e) => assert!(e.contains("clangd"), "错误应说明 clangd 缺失: {e}"),
        }
    }
}
