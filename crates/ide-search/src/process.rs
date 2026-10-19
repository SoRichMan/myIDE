//! ripgrep 子进程调用：WSL 桥接 + 环境变量覆盖。

use std::time::Duration;
use tokio::process::Command;

pub const WSL_EXE: &str = "wsl.exe";
const DETECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 单次搜索兜底超时（防止 rg 挂死拖死 UI）。
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Windows 路径 → WSL 挂载路径：`E:\foo\bar` → `/mnt/e/foo/bar`。
/// 非盘符路径原样返回（POSIX 路径原样透传，便于 mock/原生 Linux）。
pub fn win_to_wsl_path(p: &str) -> String {
    let bytes = p.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && (bytes[0].is_ascii_alphabetic()) {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let rest = &p[2..];
        let rest = rest.trim_start_matches(['\\', '/']);
        format!("/mnt/{drive}/{}", rest.replace('\\', "/"))
    } else {
        p.replace('\\', "/")
    }
}

fn distro() -> String {
    std::env::var("LITEIDE_WSL_DISTRO").unwrap_or_else(|_| "Ubuntu2404".to_string())
}

/// 构造一个带 CREATE_NO_WINDOW 的 Command。
fn base() -> Command {
    let mut c = Command::new(WSL_EXE);
    #[cfg(windows)]
    {
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// 以 WSL 调用 `rg`，工作目录为 `root_win`（Windows 路径）。
///
/// `LITEIDE_RG_CMD` 整条覆盖时按空白拆分（测试用），否则
/// `wsl.exe -d <distro> -- bash -lc "cd <root_wsl> && rg <args...>"`。
async fn run_rg(root_win: &str, extra_args: &[&str]) -> Result<String, String> {
    // 覆盖模式：直接执行（不经过 wsl）。
    if let Ok(cmdline) = std::env::var("LITEIDE_RG_CMD") {
        if !cmdline.trim().is_empty() {
            let mut parts = cmdline.split_whitespace();
            let prog = parts.next().unwrap_or("rg");
            let mut c = Command::new(prog);
            c.args(parts).args(extra_args);
            #[cfg(windows)]
            {
                c.creation_flags(CREATE_NO_WINDOW);
            }
            let out = c
                .output()
                .await
                .map_err(|e| format!("LITEIDE_RG_CMD 启动失败: {e}"))?;
            if out.status.success() {
                return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
            }
            return Err(format!(
                "rg 退出 {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }

    let root_wsl = win_to_wsl_path(root_win);
    let distro = distro();
    // 组装 shell 命令：cd 到项目根后跑 rg。参数用单引号包裹（rg 参数多为固定 flag，
    // 真正的搜索词由 manager 经 -- 传入并单独转义）。
    let mut argv = String::new();
    argv.push_str("cd ");
    argv.push_str(&shell_quote(&root_wsl));
    argv.push_str(" && rg --color=never --no-messages");
    for a in extra_args {
        argv.push(' ');
        argv.push_str(&shell_quote(a));
    }

    let fut = base()
        .args(["-d", &distro, "--", "bash", "-lc", &argv])
        .output();
    let out = match tokio::time::timeout(SEARCH_TIMEOUT, fut).await {
        Ok(r) => r,
        Err(_) => {
            return Err(format!(
                "rg 调用超时（{}s，项目过大或 WSL 繁忙）",
                SEARCH_TIMEOUT.as_secs()
            ))
        }
    };
    let out = match out {
        Ok(o) => o,
        Err(e) => return Err(format!("rg 调用失败: {e}")),
    };
    // rg 在"无匹配"时退出码 1，这不是错误；0=有匹配，1=无匹配，2=真错误。
    match out.status.code() {
        Some(0) | Some(1) => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        code => Err(format!(
            "rg 退出 {code:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// POSIX shell 单引号转义。
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 内容搜索选项（Ctrl+Shift+F 面板上的开关）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchOptions {
    /// 区分大小写（默认忽略大小写）。
    pub case_sensitive: bool,
    /// 正则模式（默认字面量 `--fixed-strings`）。
    pub regex: bool,
    /// 全词匹配（`-w`）。
    pub whole_word: bool,
    /// 文件 glob 过滤（`-g`，如 `*.cpp`）；空表示不过滤。
    ///
    /// 支持 VS Code 搜索框的写法：逗号分隔多个模式，`{}` 内的逗号不算分隔符
    /// （`*.{ts,tsx}, src/**` 是两个模式）。分隔与拆分见 [`split_globs`]。
    pub glob: Option<String>,
    /// 排除文件 glob（`-g !<glob>`，VS Code 搜索框的「排除的文件」）；空表示不排除。
    pub exclude_glob: Option<String>,
}

/// 按 VS Code 的 `splitGlobAware` 拆分逗号分隔的 glob 列表：
/// `{...}` 花括号内的逗号不当作分隔符，保证 `*.{ts,tsx}` 仍是一个模式。
pub fn split_globs(input: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut depth: usize = 0;
    let mut start = 0usize;
    for (i, ch) in input.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                let part = input[start..i].trim();
                if !part.is_empty() {
                    out.push(part);
                }
                start = i + ch.len_utf8();
            }
            _ => {}
        }
    }
    let part = input[start..].trim();
    if !part.is_empty() {
        out.push(part);
    }
    out
}

/// 组装内容搜索的 rg 参数（在 `run_rg` 的公共 flag 之后追加）。便于单元测试。
pub fn build_search_args(query: &str, opts: &SearchOptions) -> Vec<String> {
    let mut a: Vec<String> = vec!["--json".into()];
    if !opts.regex {
        a.push("--fixed-strings".into());
    }
    if !opts.case_sensitive {
        a.push("-i".into());
    }
    if opts.whole_word {
        a.push("-w".into());
    }
    if let Some(g) = opts.glob.as_deref() {
        for part in split_globs(g) {
            a.push("-g".into());
            a.push(part.to_string());
        }
    }
    if let Some(g) = opts.exclude_glob.as_deref() {
        for part in split_globs(g) {
            // rg 用 `!` 前缀表示排除（与 VS Code「排除的文件」语义一致）。
            a.push("-g".into());
            a.push(format!("!{part}"));
        }
    }
    a.push("--".into());
    a.push(query.to_string());
    // 必须显式给出搜索路径 `.`（= 上面 cd 到的项目根）：
    // `Command::output()` 会把子进程 stdin 接到空设备（不是 tty），而 rg 在
    // 「没有路径参数」且 stdin 不是 tty 时会改为搜 stdin —— 于是永远 0 命中。
    a.push(".".into());
    a
}

/// `rg --files`：列出项目内文件（尊重 .gitignore）。
pub async fn list_files(root_win: &str) -> Result<Vec<String>, String> {
    let out = run_rg(root_win, &["--files"]).await?;
    Ok(out.lines().map(|l| l.to_string()).filter(|l| !l.is_empty()).collect())
}

/// `rg --json -- <query>`：内容搜索原始 JSON Lines（按选项追加 flag）。
pub async fn search_content(
    root_win: &str,
    query: &str,
    opts: &SearchOptions,
) -> Result<String, String> {
    let args = build_search_args(query, opts);
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run_rg(root_win, &refs).await
}

/// 探测 rg 是否可用（IDE 启动时在状态栏提示）。
pub async fn probe_rg() -> Result<String, String> {
    if std::env::var("LITEIDE_RG_CMD").is_ok() {
        return Ok("custom".to_string());
    }
    let distro = distro();
    let fut = base()
        .args(["-d", &distro, "--", "sh", "-lc", "command -v rg"])
        .output();
    match tokio::time::timeout(DETECT_TIMEOUT, fut).await {
        Ok(Ok(o)) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                Err("rg 不在 WSL PATH 中".to_string())
            } else {
                Ok(s)
            }
        }
        Ok(o) => Err(format!(
            "rg 探测失败: {}",
            o.map(|x| String::from_utf8_lossy(&x.stderr).trim().to_string())
                .unwrap_or_else(|e| e.to_string())
        )),
        Err(_) => Err("rg 探测超时".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_to_wsl_drive() {
        assert_eq!(win_to_wsl_path(r"E:\TestDeepSeek\src\main.rs"), "/mnt/e/TestDeepSeek/src/main.rs");
    }

    #[test]
    fn win_to_wsl_unix_passthrough() {
        assert_eq!(win_to_wsl_path("/mnt/e/foo"), "/mnt/e/foo");
    }

    #[test]
    fn shell_quote_escapes_single_quote() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn path_non_drive_left_alone() {
        // 相对路径或 UNC 不强制转换。
        assert_eq!(win_to_wsl_path("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn build_args_default_is_literal_case_insensitive() {
        let a = build_search_args("foo", &SearchOptions::default());
        assert!(a.contains(&"--fixed-strings".to_string()));
        assert!(a.contains(&"-i".to_string()));
        assert!(!a.contains(&"-w".to_string()));
        assert_eq!(a.last().unwrap(), ".", "必须显式给搜索路径，否则 rg 会去搜 stdin");
        assert_eq!(a[a.len() - 2], "foo");
        assert_eq!(a[a.len() - 3], "--");
    }

    #[test]
    fn build_args_regex_case_and_glob() {
        let opts = SearchOptions {
            case_sensitive: true,
            regex: true,
            whole_word: true,
            glob: Some("*.cpp".to_string()),
            exclude_glob: None,
        };
        let a = build_search_args("a+b", &opts);
        assert!(!a.contains(&"--fixed-strings".to_string())); // 正则
        assert!(!a.contains(&"-i".to_string())); // 区分大小写
        assert!(a.contains(&"-w".to_string()));
        assert!(a.windows(2).any(|w| w == ["-g", "*.cpp"]));
    }

    #[test]
    fn build_args_ignores_blank_glob() {
        let opts = SearchOptions {
            glob: Some("   ".to_string()),
            ..Default::default()
        };
        assert!(!build_search_args("x", &opts).contains(&"-g".to_string()));
    }

    #[test]
    fn split_globs_keeps_brace_groups_together() {
        assert_eq!(
            split_globs("*.{ts,tsx}, src/**"),
            vec!["*.{ts,tsx}", "src/**"]
        );
        assert_eq!(split_globs(" *.cpp , *.h ,, "), vec!["*.cpp", "*.h"]);
        assert_eq!(split_globs(""), Vec::<&str>::new());
    }

    #[test]
    fn build_args_expands_include_and_exclude_globs() {
        let opts = SearchOptions {
            glob: Some("*.{ts,tsx}, src/**".to_string()),
            exclude_glob: Some("node_modules, vendor".to_string()),
            ..Default::default()
        };
        let a = build_search_args("x", &opts);
        let pairs: Vec<(String, String)> = a
            .windows(2)
            .filter(|w| w[0] == "-g")
            .map(|w| (w[0].clone(), w[1].clone()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("-g".to_string(), "*.{ts,tsx}".to_string()),
                ("-g".to_string(), "src/**".to_string()),
                ("-g".to_string(), "!node_modules".to_string()),
                ("-g".to_string(), "!vendor".to_string()),
            ]
        );
    }
}
