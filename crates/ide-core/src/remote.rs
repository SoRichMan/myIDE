//! 远程目标（WSL / SSH）描述 + 进程级「当前远程连接」注册表。
//!
//! 连接远程工作区后，IDE 的读写文件、终端、git、rg、clangd、gdb 全部在远端执行。
//! 这两类远端只在「怎么把命令送过去」上不同：
//!
//! - WSL：`wsl.exe -d <distro> --exec bash -l -c <script>`（Windows 自带桥）；
//! - SSH：`ssh -T -p <port> ... <user>@<host> "bash -l -c <script>"`（OpenSSH 客户端）。
//!
//! 送过去之后的路径、环境、权限都是 Linux 语义，所以上层（[`crate::remote`] 的使用者）
//! 只需要一个 `(program, args)` 就能启动子进程，不必关心是哪种远端。
//!
//! 其他 crate（`ide-git` / `ide-search` / `ide-dap` / `ide-lsp`）只拿到一个工作区根路径，
//! 因此这里再提供一份进程级注册表：[`set_current_target`] / [`current_target`]。
//! 应用是单进程、单工作区的，切换远程连接时整体替换即可。

use std::sync::{Mutex, OnceLock};

/// SSH 默认端口。
pub const DEFAULT_SSH_PORT: u16 = 22;

/// 一次远程连接的目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteTarget {
    /// WSL 发行版（`distro` 是 `wsl -l -q` 里的名字）。
    Wsl { distro: String },
    /// 一台 SSH 主机（免密登录；不支持交互式输密码的自动探测）。
    Ssh { user: String, host: String, port: u16 },
}

impl RemoteTarget {
    /// WSL 目标（名字两端空白会被去掉）。
    pub fn wsl(distro: impl Into<String>) -> Self {
        Self::Wsl {
            distro: distro.into().trim().to_string(),
        }
    }

    /// SSH 目标（`port == 0` 时回退到 [`DEFAULT_SSH_PORT`]）。
    pub fn ssh(user: impl Into<String>, host: impl Into<String>, port: u16) -> Self {
        Self::Ssh {
            user: user.into().trim().to_string(),
            host: host.into().trim().to_string(),
            port: if port == 0 { DEFAULT_SSH_PORT } else { port },
        }
    }

    pub fn is_wsl(&self) -> bool {
        matches!(self, Self::Wsl { .. })
    }

    pub fn is_ssh(&self) -> bool {
        matches!(self, Self::Ssh { .. })
    }

    /// 设置文件里的种类名（`"wsl"` / `"ssh"`）。
    pub fn kind(&self) -> &'static str {
        if self.is_ssh() {
            "ssh"
        } else {
            "wsl"
        }
    }

    /// `user@host`（仅 SSH）。
    pub fn user_host(&self) -> Option<String> {
        match self {
            Self::Ssh { user, host, .. } => Some(format!("{user}@{host}")),
            Self::Wsl { .. } => None,
        }
    }

    /// 状态栏/菜单里的简称：`Ubuntu2404` / `fubin@192.168.77.6`。
    pub fn short_label(&self) -> String {
        match self {
            Self::Wsl { distro } => distro.clone(),
            Self::Ssh { user, host, .. } => format!("{user}@{host}"),
        }
    }

    /// 完整标签：`WSL: Ubuntu2404` / `SSH: fubin@192.168.77.6`。
    pub fn label(&self) -> String {
        format!("{}: {}", self.kind().to_ascii_uppercase(), self.short_label())
    }

    /// SSH 基础参数（`-T`/`-t`、端口、超时）。
    ///
    /// `interactive` = 要给用户一个能输入密码/确认 host key 的终端：
    /// 这时不加 `BatchMode`（否则密码提示会被直接拒绝），也不加
    /// `StrictHostKeyChecking`（首次连接允许在终端里确认指纹）。
    fn ssh_common(&self, interactive: bool) -> Vec<String> {
        let port = match self {
            Self::Ssh { port, .. } => *port,
            Self::Wsl { .. } => DEFAULT_SSH_PORT,
        };
        let mut a = vec![if interactive {
            "-t".to_string()
        } else {
            "-T".to_string()
        }];
        a.push("-p".to_string());
        a.push(port.to_string());
        if !interactive {
            // 非交互调用必须快速失败：否则 ssh 会卡在密码提示上，把 UI 拖死。
            a.push("-o".to_string());
            a.push("BatchMode=yes".to_string());
        }
        a.push("-o".to_string());
        a.push("ConnectTimeout=10".to_string());
        if !interactive {
            // 首次连接自动记下 host key（相当于 VS Code 帮你点了一次 yes）。
            a.push("-o".to_string());
            a.push("StrictHostKeyChecking=accept-new".to_string());
        }
        a
    }

    /// 在远端跑一段 shell 脚本（非交互，stdout 就是结果）。
    ///
    /// WSL：脚本是 `bash -c` 的独立 argv；SSH：整条命令拼成一个 argv 交给远端 shell，
    /// 所以脚本本身要再做一次单引号转义。
    pub fn shell_argv(&self, script: &str) -> (String, Vec<String>) {
        match self {
            Self::Wsl { distro } => (
                wsl_exe(),
                vec![
                    "-d".to_string(),
                    distro.clone(),
                    "--exec".to_string(),
                    "bash".to_string(),
                    "-l".to_string(),
                    "-c".to_string(),
                    script.to_string(),
                ],
            ),
            Self::Ssh { user, host, .. } => {
                let mut args = self.ssh_common(false);
                args.push(format!("{user}@{host}"));
                args.push(format!("bash -l -c {}", shell_quote(script)));
                (ssh_exe(), args)
            }
        }
    }

    /// 在远端直接执行一个程序（stdio 留给调用方：LSP / DAP 的管道）。
    pub fn exec_argv(&self, program: &str, args: &[&str]) -> (String, Vec<String>) {
        match self {
            Self::Wsl { distro } => {
                let mut v = vec![
                    "-d".to_string(),
                    distro.clone(),
                    "--exec".to_string(),
                    program.to_string(),
                ];
                v.extend(args.iter().map(|a| a.to_string()));
                (wsl_exe(), v)
            }
            Self::Ssh { user, host, .. } => {
                let mut v = self.ssh_common(false);
                v.push(format!("{user}@{host}"));
                let mut line = shell_quote(program);
                for a in args {
                    line.push(' ');
                    line.push_str(&shell_quote(a));
                }
                v.push(line);
                (ssh_exe(), v)
            }
        }
    }

    /// 交互式（分配 tty）跑一段脚本：集成终端 / 外部终端窗口用。
    pub fn interactive_argv(&self, script: &str) -> (String, Vec<String>) {
        match self {
            Self::Wsl { distro } => (
                wsl_exe(),
                vec![
                    "-d".to_string(),
                    distro.clone(),
                    "--exec".to_string(),
                    "bash".to_string(),
                    "-l".to_string(),
                    "-c".to_string(),
                    script.to_string(),
                ],
            ),
            Self::Ssh { user, host, .. } => {
                let mut args = self.ssh_common(true);
                args.push(format!("{user}@{host}"));
                args.push(script.to_string());
                (ssh_exe(), args)
            }
        }
    }

    /// 新建一个落到 `cwd` 的交互式登录 shell（集成终端）。
    pub fn terminal_argv(&self, cwd: &str) -> (String, Vec<String>) {
        match self {
            // `--cd` 是 wsl.exe 自己的能力，目录不经过 shell。
            Self::Wsl { distro } => (
                wsl_exe(),
                vec![
                    "-d".to_string(),
                    distro.clone(),
                    "--cd".to_string(),
                    normalize_posix(cwd),
                ],
            ),
            Self::Ssh { .. } => self.interactive_argv(&format!(
                "cd {} && exec bash -l",
                shell_quote(&normalize_posix(cwd))
            )),
        }
    }
}

/// IDE 自带的远端工具链目录（免 root 自动安装的便携工具放这里）。
///
/// 布局：
/// - `bin/<tool>`：可执行文件的符号链接（自动加进 PATH）；
/// - `<tool>/usr/bin/<tool>`：RPM 解包出来的真实前缀；
/// - `<tool>/usr/lib64`：该工具的共享库（解析器按需放进 `LD_LIBRARY_PATH`）。
pub const REMOTE_TOOLS_PATH: &str = "$HOME/.liteide/tools";

/// IDE 支持探测 / 自动安装的远端工具。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemoteTool {
    Clangd,
    Rg,
    Gdb,
}

impl RemoteTool {
    /// 全部工具（探测顺序 = 这里）。
    pub const ALL: [Self; 3] = [Self::Clangd, Self::Rg, Self::Gdb];

    /// 可执行文件名（也是 [`RemoteTool::id`]）。
    pub fn id(self) -> &'static str {
        match self {
            Self::Clangd => "clangd",
            Self::Rg => "rg",
            Self::Gdb => "gdb",
        }
    }

    /// 解析脚本里装结果的 shell 变量名。
    pub fn var(self) -> &'static str {
        match self {
            Self::Clangd => "CLANGD",
            Self::Rg => "RG",
            Self::Gdb => "GDB",
        }
    }

    /// 面向用户的作用说明（对话框里显示）。
    pub fn purpose(self) -> &'static str {
        match self {
            Self::Clangd => "代码跳转 / 补全 / 悬停",
            Self::Rg => "全文搜索",
            Self::Gdb => "调试（F5）",
        }
    }

    /// 能否由 IDE 免 root 自动安装。
    ///
    /// clangd 走 GitHub 官方预编译包（必要时自带 glibc 运行时），rg 走发行版 RPM；
    /// gdb 依赖系统库太多，只提示用户自己装。
    pub fn auto_installable(self) -> bool {
        !matches!(self, Self::Gdb)
    }

    /// 走 RPM 方式安装时的包名（clangd 用预编译包，不用这个）。
    pub fn rpm_package(self) -> Option<&'static str> {
        match self {
            Self::Clangd => None,
            Self::Rg => Some("ripgrep"),
            Self::Gdb => None,
        }
    }

    /// 额外的搜索目录（有些开发机把工具装在 PATH 之外）。
    fn extra_dirs(self) -> &'static str {
        match self {
            Self::Clangd => concat!(
                "/usr/lib/llvm-*/bin/clangd /usr/local/bin/clangd \"$HOME/.local/bin/clangd\" ",
                "/opt/rh/llvm-toolset-*/root/usr/bin/clangd \"$HOME/opt/llvm/bin/clangd\""
            ),
            Self::Rg => concat!(
                "\"$HOME/.cache/opencode/bin/rg\" \"$HOME/.local/bin/rg\" ",
                "/usr/local/bin/rg /usr/bin/rg \"$HOME/opt/rg/bin/rg\""
            ),
            Self::Gdb => {
                "/opt/rh/devtoolset-*/root/usr/bin/gdb /usr/local/bin/gdb \"$HOME/.local/bin/gdb\""
            }
        }
    }

    /// 解析工具的 shell 片段。
    ///
    /// 执行后 `$<VAR>` 是绝对路径；找不到时往 stderr 写原因并 `exit 127`。
    /// 顺序：`~/.liteide/tools/bin`（IDE 自动安装）→ `command -v` → 常见目录。
    /// 命中自动安装目录时把 `<tool>/usr/lib64` 放进 `LD_LIBRARY_PATH`
    /// （clangd 的 `libLLVM` 等库就在那里）。
    pub fn resolver_shell(self) -> String {
        // 用占位符替换而不是 `format!`：片段里 `${VAR:+...}` 的花括号太多。
        RESOLVER_TEMPLATE
            .replace("@VAR@", self.var())
            .replace("@BIN@", self.id())
            .replace("@DIRS@", self.extra_dirs())
    }
}

/// 解析模板：`@VAR@` / `@BIN@` / `@DIRS@` 由 [`RemoteTool::resolver_shell`] 填。
const RESOLVER_TEMPLATE: &str = concat!(
    "export PATH=\"$HOME/.liteide/tools/bin:$PATH\"; ",
    "@VAR@=$(command -v @BIN@ 2>/dev/null); ",
    "if [ -z \"$@VAR@\" ]; then ",
    "for c in @DIRS@; do [ -x \"$c\" ] && { @VAR@=$c; break; }; done; fi; ",
    "if [ -z \"$@VAR@\" ]; then echo '@BIN@ 不在远端 PATH 中' >&2; exit 127; fi; ",
    "R=$(readlink -f \"$@VAR@\" 2>/dev/null || echo \"$@VAR@\"); ",
    "case \"$R\" in \"$HOME/.liteide/tools/\"*) ",
    "L=$(dirname \"$(dirname \"$R\")\")/lib64; ",
    "[ -d \"$L\" ] && export LD_LIBRARY_PATH=\"$L${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}\";; ",
    "esac; "
);

/// `wsl.exe` 可执行文件名（测试/自定义部署可覆盖）。
pub fn wsl_exe() -> String {
    std::env::var("LITEIDE_WSL_EXE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "wsl.exe".to_string())
}

/// `ssh` 客户端可执行文件名（`LITEIDE_SSH_EXE` 可覆盖，测试/自带 ssh.exe 时用）。
pub fn ssh_exe() -> String {
    std::env::var("LITEIDE_SSH_EXE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "ssh".to_string())
}

/// POSIX shell 单引号转义（`a'b` → `'a'\''b'`）。
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 规范化 POSIX 目录：补前导 `/`、去多余尾部 `/`，空串视为 `/`。
pub fn normalize_posix(p: &str) -> String {
    let t = p.trim().replace('\\', "/");
    let t = t.trim_end_matches('/');
    if t.is_empty() {
        "/".to_string()
    } else if t.starts_with('/') {
        t.to_string()
    } else {
        format!("/{t}")
    }
}

/// 路径是否像远端 POSIX 路径（`/...`；盘符路径 `E:\...` 不算）。
pub fn is_posix_path(p: &str) -> bool {
    let t = p.trim_start();
    t.starts_with('/') && !t.starts_with("//")
}

// ---------------------------------------------------------------------------
// 进程级「当前远程连接」
// ---------------------------------------------------------------------------

fn registry() -> &'static Mutex<Option<RemoteTarget>> {
    static CELL: OnceLock<Mutex<Option<RemoteTarget>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

/// 设置当前远程连接（`None` = 本地工作区）。
///
/// `ide-git` / `ide-search` / `ide-dap` 这些只拿到根路径的模块据此选择传输方式。
pub fn set_current_target(target: Option<RemoteTarget>) {
    if let Ok(mut guard) = registry().lock() {
        *guard = target;
    }
}

/// 当前远程连接（未连接为 `None`）。
pub fn current_target() -> Option<RemoteTarget> {
    registry().lock().ok().and_then(|g| g.clone())
}

/// 当前连接是 SSH 且该路径是 POSIX 路径时返回目标。
///
/// 这是 `ide-git` / `ide-search` 判断「该走 SSH 还是保持原来的 WSL/本地行为」的
/// 唯一条件：本地工程（`E:\...`）即使连着 SSH 也仍然走 WSL 的 `/mnt/e/...`。
pub fn ssh_target_for_path(path: &str) -> Option<RemoteTarget> {
    let t = current_target()?;
    if t.is_ssh() && is_posix_path(path) {
        Some(t)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_target_argv_is_wsl_exe() {
        let t = RemoteTarget::wsl("Ubuntu2404");
        let (prog, args) = t.shell_argv("echo hi");
        assert!(prog.ends_with("wsl.exe"));
        assert_eq!(
            args,
            vec!["-d", "Ubuntu2404", "--exec", "bash", "-l", "-c", "echo hi"]
        );
        assert_eq!(t.label(), "WSL: Ubuntu2404");
        assert!(!t.is_ssh());
    }

    #[test]
    fn ssh_target_argv_has_safety_flags() {
        let t = RemoteTarget::ssh("fubin", "192.168.77.6", 2222);
        let (prog, args) = t.shell_argv("echo hi");
        assert!(prog.ends_with("ssh"));
        assert_eq!(args[0], "-T");
        assert!(args.windows(2).any(|w| w == ["-p", "2222"]));
        assert!(args.contains(&"BatchMode=yes".to_string()));
        assert!(args.contains(&"ConnectTimeout=10".to_string()));
        assert!(args.contains(&"StrictHostKeyChecking=accept-new".to_string()));
        assert!(args.contains(&"fubin@192.168.77.6".to_string()));
        // 脚本作为一个 argv 传给远端 shell，内部的单引号必须被转义掉。
        let last = args.last().unwrap();
        assert!(last.starts_with("bash -l -c '"));
        assert_eq!(t.label(), "SSH: fubin@192.168.77.6");
        assert_eq!(t.user_host().as_deref(), Some("fubin@192.168.77.6"));
    }

    #[test]
    fn ssh_exec_quotes_program_and_args() {
        let t = RemoteTarget::ssh("u", "h", 22);
        let (_, args) = t.exec_argv("find", &[".", "-name", "a b"]);
        assert_eq!(args.last().unwrap(), "'find' '.' '-name' 'a b'");
    }

    #[test]
    fn ssh_interactive_allows_password_prompt() {
        let t = RemoteTarget::ssh("u", "h", 22);
        let (_, args) = t.interactive_argv("exec bash -l");
        assert_eq!(args[0], "-t");
        assert!(!args.contains(&"BatchMode=yes".to_string()));
        assert!(args.contains(&"ConnectTimeout=10".to_string()));
    }

    #[test]
    fn terminal_argv_lands_in_cwd() {
        let wsl = RemoteTarget::wsl("Debian");
        let (prog, args) = wsl.terminal_argv("/home/x/../y/");
        assert!(prog.ends_with("wsl.exe"));
        assert_eq!(args.last().unwrap(), "/home/x/../y");

        let ssh = RemoteTarget::ssh("u", "h", 22);
        let (_, args) = ssh.terminal_argv("/home/u/proj");
        assert_eq!(args.last().unwrap(), "cd '/home/u/proj' && exec bash -l");
    }

    #[test]
    fn ssh_port_zero_falls_back_to_default() {
        match RemoteTarget::ssh("u", "h", 0) {
            RemoteTarget::Ssh { port, .. } => assert_eq!(port, DEFAULT_SSH_PORT),
            _ => panic!("应该是 SSH 目标"),
        }
    }

    #[test]
    fn registry_roundtrip_and_path_gate() {
        let prev = current_target();
        set_current_target(Some(RemoteTarget::ssh("u", "h", 22)));
        assert!(ssh_target_for_path("/home/u/proj").is_some());
        // 本地工程（盘符路径）不吃 SSH 规则，仍走原来的 WSL 桥。
        assert!(ssh_target_for_path(r"E:\proj").is_none());
        set_current_target(Some(RemoteTarget::wsl("Ubuntu2404")));
        assert!(ssh_target_for_path("/home/u/proj").is_none());
        set_current_target(prev);
    }

    #[test]
    fn remote_tool_resolver_prefers_installed_toolchain() {
        for tool in RemoteTool::ALL {
            let s = tool.resolver_shell();
            assert!(
                s.contains("$HOME/.liteide/tools/bin"),
                "{} 的解析器要优先用 IDE 自带的工具: {s}",
                tool.id()
            );
            assert!(
                s.contains(&format!("{}=$(command -v {}", tool.var(), tool.id())),
                "{} 的解析器要先查 PATH: {s}",
                tool.id()
            );
            assert!(!s.contains("@VAR@") && !s.contains("@BIN@") && !s.contains("@DIRS@"));
            // 自动安装的布局需要把同前缀的 lib64 加进 LD_LIBRARY_PATH。
            assert!(s.contains("LD_LIBRARY_PATH"));
            assert!(s.contains("exit 127"));
        }
        assert!(RemoteTool::Clangd
            .resolver_shell()
            .contains("/usr/lib/llvm-*/bin/clangd"));
        assert!(RemoteTool::Clangd.auto_installable());
        assert!(RemoteTool::Rg.auto_installable());
        // gdb 依赖系统库太多，只提示用户自己装。
        assert!(!RemoteTool::Gdb.auto_installable());
        assert_eq!(RemoteTool::Rg.rpm_package(), Some("ripgrep"));
    }

    #[test]
    fn posix_helpers() {
        assert_eq!(normalize_posix(""), "/");
        assert_eq!(normalize_posix("/a/b/"), "/a/b");
        assert_eq!(normalize_posix("a\\b"), "/a/b");
        assert!(is_posix_path("/home"));
        assert!(!is_posix_path(r"\\wsl.localhost\x"));
        assert!(!is_posix_path(r"E:\x"));
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }
}
