//! ide-remote: 在 WSL 发行版**内部**读写文件、启动终端。
//!
//! 连接 WSL 工作区后，IDE 不再用 `\\wsl.localhost\<distro>\...`（Windows 侧 9P 挂载）
//! 访问 Linux 文件，而是在发行版里常驻一个 `bash -l` 会话，所有读/写/列目录都按 POSIX
//! 路径在 WSL 内部完成。差别不只是路径写法：
//!
//! - 环境变量来自 WSL 用户自己的登录环境（PATH / LANG / CC / CXX …），不是 Windows 侧继承；
//! - 权限、符号链接、大小写、硬链接语义都是 Linux 的；
//! - 后续 Terminal 用同一套 `wsl.exe --cd <posix>` 启动，落点与编辑器一致。
//!
//! 协议：行式（TAB 分隔），参数与返回都走 base64，避免路径里的空格/引号/换行破坏分帧。
//! 常驻会话省掉了每次 `wsl.exe` 冷启动（实测单次调用约 120ms，会话内往返约 1ms）。

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

mod terminal;
pub use terminal::{TermOutput, WslTerminal};

/// 单次请求的等待上限：bash 会话卡住（例如被杀）时不能让 UI 无限等。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 常驻 agent：在 WSL 里跑一个 `bash -l`，从 stdin 读命令、往 stdout 写结果。
///
/// `-l` 是为了拿到用户登录环境（`~/.profile`、`/etc/profile` 里的 PATH）；
/// 副作用是启动时可能有 profile 输出，因此帧里只认「id 对得上」的响应行。
///
/// 刻意写成**单行**：脚本是作为 Windows 命令行参数传给 `wsl.exe` 的，
/// 换行在 argv 里会经过一层转义，换成单行可以彻底避开这类坑。
const AGENT_SCRIPT: &str = concat!(
    "export LC_ALL=C; ",
    "while IFS=$'\\t' read -r id op a1 a2; do ",
    "[ -z \"$id\" ] && continue; ",
    "st=0; out=\"\"; ",
    "case \"$op\" in ",
    // read：把文件按 base64 输出（-w0 不折行，便于当成单行帧）。
    "read) p=$(printf '%s' \"$a1\" | base64 -d); if out=$(base64 -w0 -- \"$p\" 2>&1); then st=0; else st=1; fi;; ",
    // write：内容从 a2 解 base64 后原样写入，不经过 shell 引号。
    "write) p=$(printf '%s' \"$a1\" | base64 -d); ",
    "if err=$( { printf '%s' \"$a2\" | base64 -d > \"$p\"; } 2>&1 ); then st=0; out=\"\"; else st=1; out=$err; fi;; ",
    // list：一层目录，输出 `类型\\t名字`。
    "list) p=$(printf '%s' \"$a1\" | base64 -d); ",
    "if out=$(cd -- \"$p\" 2>/dev/null && find . -mindepth 1 -maxdepth 1 -printf '%y\\t%f\\n' 2>&1); ",
    "then st=0; else st=1; out=\"无法读取目录: $p\"; fi;; ",
    "stat) p=$(printf '%s' \"$a1\" | base64 -d); ",
    "if [ -d \"$p\" ]; then out=d; elif [ -f \"$p\" ]; then out=f; elif [ -e \"$p\" ]; then out=o; else out=-; fi;; ",
    "mkdir) p=$(printf '%s' \"$a1\" | base64 -d); ",
    "if err=$( { mkdir -p -- \"$p\"; } 2>&1 ); then st=0; out=\"\"; else st=1; out=$err; fi;; ",
    "ping) out=ok;; ",
    "*) st=1; out=\"未知操作: $op\";; ",
    "esac; ",
    "printf '%s\\t%s\\t%s\\n' \"$id\" \"$st\" \"$(printf '%s' \"$out\" | base64 -w0)\"; ",
    "done"
);

/// 目录项（`name` 不带路径，`is_dir` 来自 `find -printf %y`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub is_dir: bool,
}

/// 文件类型探测结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Dir,
    File,
    Other,
    Missing,
}

/// 规范化 POSIX 目录：补前导 `/`、去多余尾部 `/`，空串视为 `/`。
///
/// 反斜杠一律当分隔符处理（`Path::join` 在 Windows 上拼出来的是 `\`）。
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

/// 常驻 WSL 文件会话。所有调用都是同步的（会话内往返约 1ms），由调用方串行使用。
pub struct WslFs {
    session: Option<Session>,
}

impl Default for WslFs {
    fn default() -> Self {
        Self::new()
    }
}

impl WslFs {
    pub fn new() -> Self {
        Self { session: None }
    }

    /// 读文本：按 UTF-8 无损解码，非 UTF-8 内容做 lossy 替换（编辑器只处理文本）。
    pub fn read_text(&mut self, distro: &str, posix: &str) -> Result<String, String> {
        let bytes = self.read_bytes(distro, posix)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// 读原始字节（`base64 -w0` 的逆过程）。
    pub fn read_bytes(&mut self, distro: &str, posix: &str) -> Result<Vec<u8>, String> {
        let path = normalize_posix(posix);
        let b64 = self.request(distro, "read", &path, "")?;
        b64_decode(&b64).ok_or_else(|| format!("读取 {path} 的响应不是合法 base64"))
    }

    /// 写文本：内容作为参数走 base64 上链，不经过 shell 引号，原样落盘（保留权限位）。
    ///
    /// 参数由 [`WslFs::request`] 统一做一次传输编码，所以这里直接传原文，
    /// 文件里的 TAB / 换行 / 引号都不会破坏分帧。
    pub fn write_text(&mut self, distro: &str, posix: &str, text: &str) -> Result<(), String> {
        let path = normalize_posix(posix);
        self.request(distro, "write", &path, text)?;
        Ok(())
    }

    /// 列目录（一层，文件 + 目录；隐藏项也在内）。
    pub fn list_dir(&mut self, distro: &str, posix: &str) -> Result<Vec<RemoteEntry>, String> {
        let path = normalize_posix(posix);
        let raw = self.request(distro, "list", &path, "")?;
        let mut out = Vec::new();
        for line in raw.split('\n') {
            let Some((kind, name)) = line.split_once('\t') else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            out.push(RemoteEntry {
                name: name.to_string(),
                is_dir: kind == "d",
            });
        }
        Ok(out)
    }

    pub fn path_kind(&mut self, distro: &str, posix: &str) -> Result<PathKind, String> {
        let path = normalize_posix(posix);
        Ok(match self.request(distro, "stat", &path, "")?.as_str() {
            "d" => PathKind::Dir,
            "f" => PathKind::File,
            "o" => PathKind::Other,
            _ => PathKind::Missing,
        })
    }

    pub fn is_dir(&mut self, distro: &str, posix: &str) -> bool {
        matches!(self.path_kind(distro, posix), Ok(PathKind::Dir))
    }

    pub fn is_file(&mut self, distro: &str, posix: &str) -> bool {
        matches!(self.path_kind(distro, posix), Ok(PathKind::File))
    }

    pub fn exists(&mut self, distro: &str, posix: &str) -> bool {
        matches!(
            self.path_kind(distro, posix),
            Ok(PathKind::Dir) | Ok(PathKind::File) | Ok(PathKind::Other)
        )
    }

    pub fn create_dir_all(&mut self, distro: &str, posix: &str) -> Result<(), String> {
        let path = normalize_posix(posix);
        self.request(distro, "mkdir", &path, "")?;
        Ok(())
    }

    /// 关闭常驻会话（切换发行版/退出时调用）。
    pub fn shutdown(&mut self) {
        self.session = None;
    }

    /// 发一条请求并等对应响应；会话失效时丢弃并允许下次重建。
    fn request(&mut self, distro: &str, op: &str, a1: &str, a2: &str) -> Result<String, String> {
        let distro = distro.trim();
        if distro.is_empty() {
            return Err("未指定 WSL 发行版".to_string());
        }
        // 会话不可用（首次/换发行版/上次失败）就重建；重建失败直接把错误抛给调用方。
        let need_new = match &self.session {
            Some(s) => s.distro != distro || s.closed,
            None => true,
        };
        if need_new {
            self.session = Some(Session::spawn(distro)?);
        }
        let result = match self.session.as_mut() {
            Some(s) => s.request(op, a1, a2),
            None => Err("WSL 会话未建立".to_string()),
        };
        if result.is_err() {
            // 超时/管道断：这个会话已经不可信，下次调用重建。
            self.session = None;
        }
        result
    }
}

/// 一次常驻会话（一个 `wsl.exe -- bash -l` 进程）。
struct Session {
    distro: String,
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    next_id: u64,
    closed: bool,
}

impl Session {
    fn spawn(distro: &str) -> Result<Self, String> {
        let mut cmd = Command::new("wsl.exe");
        // 必须用 `--exec`：走默认的 `--` 时 wsl.exe 会先套一层自己的解析器，
        // 实测会吞掉 stdin 的第一行（bash 的 `read` 直接拿到空串）。
        cmd.arg("-d")
            .arg(distro)
            .arg("--exec")
            .arg("bash")
            .arg("-l")
            .arg("-c")
            .arg(AGENT_SCRIPT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动 WSL 会话失败（{distro}）: {e}"))?;
        let stdin = child.stdin.take().ok_or("拿不到 WSL 会话 stdin")?;
        let stdout = child.stdout.take().ok_or("拿不到 WSL 会话 stdout")?;
        let stderr = child.stderr.take().ok_or("拿不到 WSL 会话 stderr")?;

        let (tx, rx) = mpsc::channel::<String>();
        std::thread::Builder::new()
            .name("wslfs-out".to_string())
            .spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("启动 WSL 读取线程失败: {e}"))?;
        // stderr 只记日志：agent 的错误都走协议返回，这里多半是 profile 噪音。
        let tag = distro.to_string();
        std::thread::Builder::new()
            .name("wslfs-err".to_string())
            .spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    tracing::debug!("wsl[{tag}] stderr: {line}");
                }
            })
            .map_err(|e| format!("启动 WSL 错误读取线程失败: {e}"))?;

        let mut s = Self {
            distro: distro.to_string(),
            child,
            stdin,
            rx,
            next_id: 0,
            closed: false,
        };
        // 握手：profile 可能先往 stdout 打东西，用 ping 把管道对齐到「协议开始」。
        s.request("ping", "", "")
            .map_err(|e| format!("WSL 会话握手失败（{distro}）: {e}"))?;
        Ok(s)
    }

    fn request(&mut self, op: &str, a1: &str, a2: &str) -> Result<String, String> {
        self.next_id += 1;
        let id = self.next_id;
        let line = format!("{id}\t{op}\t{}\t{}\n", b64_encode(a1.as_bytes()), b64_encode(a2.as_bytes()));
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.flush())
            .map_err(|e| {
                self.closed = true;
                format!("写入 WSL 会话失败: {e}")
            })?;

        let want = id.to_string();
        loop {
            let line = match self.rx.recv_timeout(REQUEST_TIMEOUT) {
                Ok(l) => l,
                Err(_) => {
                    self.closed = true;
                    return Err(format!(
                        "WSL 会话响应超时（{}s，op={op}）",
                        REQUEST_TIMEOUT.as_secs()
                    ));
                }
            };
            if let Some(rest) = line.strip_prefix(&want) {
                let Some(rest) = rest.strip_prefix('\t') else {
                    continue;
                };
                let (status, payload) = rest.split_once('\t').unwrap_or((rest, ""));
                let payload = b64_decode(payload).unwrap_or_default();
                if status == "0" {
                    return Ok(String::from_utf8_lossy(&payload).into_owned());
                }
                let msg = String::from_utf8_lossy(&payload).trim().to_string();
                return Err(if msg.is_empty() {
                    format!("WSL 操作失败（op={op}）")
                } else {
                    msg
                });
            }
            // 别的 id（超时后被丢弃的旧请求）或 profile 噪音：忽略继续等。
            tracing::trace!("wsl 会话丢弃一行: {line}");
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 在 WSL 内部打开终端，落点是工作区目录（`--cd` 走发行版内路径）。
///
/// 优先 Windows Terminal（`wt.exe`），没有就退回 `wsl.exe` 自带控制台——
/// 两者都是 `wsl.exe` 起的登录 shell，环境变量即 WSL 用户自己的。
pub fn open_terminal(distro: &str, posix_cwd: &str) -> Result<(), String> {
    let distro = distro.trim();
    if distro.is_empty() {
        return Err("未指定 WSL 发行版".to_string());
    }
    let cwd = normalize_posix(posix_cwd);
    let title = format!("{distro}: {cwd}");

    let mut wt = Command::new("wt.exe");
    wt.args([
        "new-tab",
        "--title",
        &title,
        "wsl.exe",
        "-d",
        distro,
        "--cd",
        &cwd,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    if wt.spawn().is_ok() {
        return Ok(());
    }

    let mut fallback = Command::new("wsl.exe");
    fallback
        .arg("-d")
        .arg(distro)
        .arg("--cd")
        .arg(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        fallback.creation_flags(CREATE_NO_WINDOW);
    }
    fallback
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("启动 WSL 终端失败: {e}"))
}

// ---------------------------------------------------------------------------
// base64（只为了分帧，不追求速度；实现留在 crate 内，避免多一个依赖）
// ---------------------------------------------------------------------------

const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(B64_ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64_ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\r' | b'\n' | b' ' | b'\t' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        for case in [
            "",
            "a",
            "ab",
            "abc",
            "line one\nline two\n",
            "中文 + emoji 🚀",
        ] {
            let enc = b64_encode(case.as_bytes());
            assert_eq!(b64_decode(&enc).unwrap(), case.as_bytes(), "case={case:?}");
        }
        // 与标准 base64 对齐（换行/空格应被忽略）。
        assert_eq!(b64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(b64_decode("aGVs\nbG8=").unwrap(), b"hello");
        assert!(b64_decode("!!!").is_none());
    }

    #[test]
    fn normalize_posix_paths() {
        assert_eq!(normalize_posix(""), "/");
        assert_eq!(normalize_posix("/"), "/");
        assert_eq!(normalize_posix("/home/work/"), "/home/work");
        assert_eq!(normalize_posix("home\\work"), "/home/work");
        assert_eq!(normalize_posix("  /home/work/x  "), "/home/work/x");
    }

    #[test]
    fn posix_detection() {
        assert!(is_posix_path("/home/work"));
        assert!(is_posix_path(" /home/work "));
        assert!(!is_posix_path(r"E:\proj"));
        assert!(!is_posix_path(r"\\wsl.localhost\Ubuntu2404\home"));
        assert!(!is_posix_path("relative/path"));
    }

    /// 常驻会话的手工验收（需要本机有 Ubuntu2404；默认不跑，避免 CI 依赖 WSL）：
    /// `cargo test -p ide-remote -- --ignored`
    #[test]
    #[ignore = "需要本机 WSL 发行版 Ubuntu2404"]
    fn wsl_session_roundtrip() {
        let distro = std::env::var("LITEIDE_WSL_DISTRO").unwrap_or_else(|_| "Ubuntu2404".to_string());
        let dir = "/tmp/liteide_remote_test";
        let file = format!("{dir}/hello.txt");
        let mut fs = WslFs::new();
        fs.create_dir_all(&distro, dir).expect("mkdir");
        assert!(fs.is_dir(&distro, dir));
        fs.write_text(&distro, &file, "line one\nline two\n").expect("write");
        assert!(fs.is_file(&distro, &file));
        assert_eq!(fs.read_text(&distro, &file).unwrap(), "line one\nline two\n");
        let entries = fs.list_dir(&distro, dir).expect("list");
        assert!(entries.iter().any(|e| e.name == "hello.txt" && !e.is_dir));
        // 写坏路径要报错，而不是静默成功。
        assert!(fs.write_text(&distro, "/proc/nope/x", "x").is_err());
        assert_eq!(
            fs.path_kind(&distro, "/tmp/liteide_remote_test/does-not-exist").unwrap(),
            PathKind::Missing
        );
    }
}
