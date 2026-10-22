//! 远端工具链：探测缺哪些工具 + 免 root 自动安装（RPM 解包到 `~/.liteide/tools`）。
//!
//! 真实痛点：SSH/WSL 主机上没有 clangd 时，代码跳转/补全全部不可用，而用户往往
//! **没有 root**（`sudo` 要密码、公司机器不让装包）。这里的自动安装：
//!
//! 1. `yumdownloader --resolve` 把 RPM（含依赖）下到 `~/.liteide/tools/tmp`；
//! 2. `rpm2cpio | cpio` 解包，把工具自己的前缀（`usr/`）复制到
//!    `~/.liteide/tools/<tool>/`；
//! 3. 在 `~/.liteide/tools/bin` 建符号链接，`--version` 自检。
//!
//! 解析器（[`ide_core::remote::RemoteTool::resolver_shell`]）会把这个 `bin` 放到
//! PATH 最前面，并按需设置 `LD_LIBRARY_PATH`——所以装完不用改任何配置。
//!
//! 只能覆盖「软件源里有 RPM」的工具（clangd / rg）。gdb 依赖系统库太多，
//! [`RemoteTool::rpm`] 返回 `None`，由界面提示用户自己装。

use ide_core::remote::{RemoteTarget, RemoteTool, REMOTE_TOOLS_PATH};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// 探测超时（一次 SSH 往返 + 三个 `command -v`，正常不到 1 秒）。
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// 自动安装超时：下载几十 MB + 解包，远端镜像慢时也给它足够时间。
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(900);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 一次探测的结果：找到了哪些（带路径）、缺哪些。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolchainProbe {
    pub found: Vec<(RemoteTool, String)>,
    pub missing: Vec<RemoteTool>,
}

impl ToolchainProbe {
    /// 工具齐了（可以正常跳转/搜索/调试）。
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }

    /// 某个工具的解析路径。
    pub fn path_of(&self, tool: RemoteTool) -> Option<&str> {
        self.found
            .iter()
            .find(|(t, _)| *t == tool)
            .map(|(_, p)| p.as_str())
    }

    /// 一行摘要（状态栏 / flash 用）。
    pub fn summary(&self) -> String {
        if self.is_complete() {
            return "远端工具链齐全".to_string();
        }
        let names: Vec<&str> = self.missing.iter().map(|t| t.id()).collect();
        format!("远端缺少工具：{}", names.join("、"))
    }
}

/// 三个工具都探测一遍（一次远端往返）。
pub fn probe(target: &RemoteTarget) -> Result<ToolchainProbe, String> {
    let out = run_script(target, &probe_script(), PROBE_TIMEOUT, &mut |_| {})?;
    let mut probe = ToolchainProbe {
        found: Vec::new(),
        missing: RemoteTool::ALL.to_vec(),
    };
    for line in out.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("ok ") {
            let Some((id, path)) = rest.split_once(' ') else {
                continue;
            };
            let Some(tool) = RemoteTool::ALL.iter().copied().find(|t| t.id() == id) else {
                continue;
            };
            probe.missing.retain(|t| *t != tool);
            probe.found.push((tool, path.trim().to_string()));
        }
    }
    if probe.found.is_empty() && !out.trim().is_empty() && probe.missing.len() == RemoteTool::ALL.len()
    {
        return Err(format!("无法解析探测结果：{}", out.trim()));
    }
    Ok(probe)
}

/// 探测脚本：对每个工具跑一遍真实解析器（子 shell，`exit 127` 不会打断整段）。
fn probe_script() -> String {
    let mut s = String::from("set +e; LC_ALL=C; ");
    for tool in RemoteTool::ALL {
        // `p=$( ... )`：解析器找不到时会 `exit 127`，只退出子 shell。
        s.push_str(&format!(
            "p=$({} printf '%s' \"${{{}}}\" 2>/dev/null) || p=\"\"; ",
            tool.resolver_shell(),
            tool.var()
        ));
        s.push_str(&format!(
            "if [ -n \"$p\" ]; then echo \"ok {} $p\"; else echo \"missing {}\"; fi; ",
            tool.id(),
            tool.id()
        ));
    }
    s
}

/// clangd 预编译包版本（GitHub `clangd/clangd` 的 release tag）。
const CLANGD_VERSION: &str = "18.1.3";

/// 自动安装某个工具的脚本（`None` = 该工具只能手动装）。
///
/// - clangd：下载官方预编译包；系统 glibc 太旧（CentOS 7 = 2.17）时再自带一份
///   2.27 运行时，用它的动态加载器启动——全程不需要 root，也不动系统库。
/// - rg：从发行版源下载 RPM 解包（CentOS 7 自带 rg 11 够用）。
pub fn install_script(tool: RemoteTool) -> Option<String> {
    match tool {
        RemoteTool::Clangd => Some(
            CLANGD_INSTALL_TEMPLATE
                .replace("@TOOLS@", REMOTE_TOOLS_PATH)
                .replace("@VER@", CLANGD_VERSION),
        ),
        RemoteTool::Rg => Some(
            RPM_INSTALL_TEMPLATE
                .replace("@TOOLS@", REMOTE_TOOLS_PATH)
                .replace("@BIN@", tool.id())
                .replace("@PKG@", tool.rpm_package().unwrap_or_default()),
        ),
        RemoteTool::Gdb => None,
    }
}

/// clangd 安装脚本（占位符替换，避免 `format!` 与 shell 的 `${...}` 打架）。
const CLANGD_INSTALL_TEMPLATE: &str = r#"set -u
T="@TOOLS@"
V="@VER@"
URL="https://github.com/clangd/clangd/releases/download/$V/clangd-linux-$V.zip"
mkdir -p "$T/bin" "$T/tmp"
chmod -R u+w "$T/tmp" 2>/dev/null || true
rm -rf "$T/tmp"
mkdir -p "$T/tmp"
D="$T/tmp/clangd.$$"
mkdir -p "$D"
if ! command -v curl >/dev/null 2>&1; then
  echo "远端没有 curl，无法下载 clangd"
  exit 3
fi
echo "正在下载 clangd $V（约 80 MB）…"
OK=0
for b in https://ghfast.top/ https://gh-proxy.com/ https://ghproxy.net/ ""; do
  if curl -sS -L --fail --max-time 600 -o "$D/clangd.zip" "$b$URL" 2>/dev/null; then
    OK=1
    break
  fi
  echo "镜像不可用：${b:-github 直连}"
done
if [ "$OK" != 1 ]; then
  echo "下载失败：远端连不上 GitHub（直连与镜像都失败），请手动安装 clangd"
  exit 4
fi
echo "正在解包…"
if command -v unzip >/dev/null 2>&1; then
  unzip -q -o "$D/clangd.zip" -d "$D/x" || exit 5
else
  PY=""
  if command -v python >/dev/null 2>&1; then PY=python
  elif command -v python3 >/dev/null 2>&1; then PY=python3
  fi
  if [ -z "$PY" ]; then
    echo "缺少解压工具（unzip / python）"
    exit 5
  fi
  "$PY" -c "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])" "$D/clangd.zip" "$D/x" || exit 5
fi
SRC=$(find "$D/x" -type f -path '*/bin/clangd' | head -n 1)
if [ -z "$SRC" ]; then
  echo "压缩包里找不到 bin/clangd"
  exit 5
fi
BINDIR=$(dirname "$SRC")
chmod -R u+x "$BINDIR"
PREFIX=$(dirname "$BINDIR")
echo "正在安装到 $T/clangd …"
chmod -R u+w "$T/clangd" 2>/dev/null || true
rm -rf "$T/clangd"
mkdir -p "$T/clangd"
cp -a "$PREFIX/." "$T/clangd/"
# 旧安装可能留下指向 SCL 的符号链接：先摘掉，否则 `cat >` 会顺着断链失败。
rm -f "$T/bin/clangd"
# 新 clangd 要求 glibc 2.18+（CentOS 7 只有 2.17）：自带一份 2.27 运行时。
if "$T/clangd/bin/clangd" --version >/dev/null 2>&1; then
  ln -sfn "../clangd/bin/clangd" "$T/bin/clangd"
else
  echo "系统 glibc 太旧，安装自带的 glibc 2.27 运行时…"
  if ! command -v ar >/dev/null 2>&1; then
    echo "缺少 ar（binutils），无法解包 glibc 运行时"
    exit 6
  fi
  if ! curl -sS -L --fail --max-time 300 -o "$D/libc6.deb" \
      http://mirrors.aliyun.com/ubuntu/pool/main/g/glibc/libc6_2.27-3ubuntu1_amd64.deb; then
    echo "下载 glibc 运行时失败（需要访问 mirrors.aliyun.com）"
    exit 6
  fi
  ( cd "$D" && ar x libc6.deb && for f in data.tar.*; do tar xf "$f"; done ) || {
    echo "解包 glibc 运行时失败"
    exit 6
  }
  mkdir -p "$T/clangd/glibc"
  cp -a "$D/lib" "$T/clangd/glibc/"
  cat > "$T/bin/clangd" <<'WRAPPER'
#!/bin/sh
# IDE 自动安装的 clangd：旧系统（CentOS 7 / glibc 2.17）跑不了新二进制，
# 用随包附带的 2.27 动态加载器启动（不 root、不动系统库）。
P="$HOME/.liteide/tools/clangd"
exec "$P/glibc/lib/x86_64-linux-gnu/ld-2.27.so" \
  --library-path "$P/glibc/lib/x86_64-linux-gnu" "$P/bin/clangd" "$@"
WRAPPER
  chmod +x "$T/bin/clangd"
fi
if ! OUT=$("$T/bin/clangd" --version 2>&1); then OUT=""; fi
V2=$(printf '%s\n' "$OUT" | head -n 1)
if [ -z "$V2" ]; then
  echo "clangd 安装后无法运行"
  exit 7
fi
chmod -R u+w "$D" 2>/dev/null || true
rm -rf "$D"
echo "版本：$V2"
echo "OK: $T/bin/clangd"
"#;

/// RPM 方式安装（rg）：下载 + 解包到 `~/.liteide/tools/<bin>`。
const RPM_INSTALL_TEMPLATE: &str = r#"set -u
T="@TOOLS@"
BIN="@BIN@"
PKG="@PKG@"
mkdir -p "$T/bin" "$T/tmp"
# 上次安装失败可能留下只读的解包目录（RPM 里目录是 555，rm 需要先去掉只读）。
chmod -R u+w "$T/tmp" 2>/dev/null || true
rm -rf "$T/tmp"
mkdir -p "$T/tmp"
D="$T/tmp/$BIN.$$"
mkdir -p "$D/rpms" "$D/root"
if ! command -v yumdownloader >/dev/null 2>&1; then
  if command -v apt-get >/dev/null 2>&1; then
    echo "该主机是 Debian/Ubuntu：自动安装需要 sudo（无法免交互），请手动执行 apt 安装后再点「重新检测」"
  else
    echo "远端缺少 yumdownloader（yum-utils），无法自动安装"
  fi
  exit 3
fi
echo "正在下载 $PKG（含依赖）…"
if ! yumdownloader --resolve --destdir="$D/rpms" "$PKG" >"$D/yum.log" 2>&1; then
  echo "下载失败，yumdownloader 日志："
  tail -n 15 "$D/yum.log"
  exit 4
fi
echo "正在解包…"
cd "$D/root" || exit 4
for f in "$D/rpms"/*.rpm; do
  rpm2cpio "$f" | cpio -idmu --quiet >/dev/null 2>&1 || true
done
SRC=$(find "$D/root" \( -type f -o -type l \) -path "*/usr/bin/$BIN" 2>/dev/null | head -n 1)
if [ -z "$SRC" ] || [ ! -x "$SRC" ]; then
  echo "解包后找不到 $BIN"
  exit 5
fi
PREFIX=${SRC%/usr/bin/$BIN}
echo "正在安装到 $T/$BIN …"
chmod -R u+w "$T/$BIN" 2>/dev/null || true
rm -rf "$T/$BIN"
mkdir -p "$T/$BIN"
cp -a "$PREFIX/." "$T/$BIN/"
chmod +x "$T/$BIN/usr/bin/$BIN"
ln -sfn "../$BIN/usr/bin/$BIN" "$T/bin/$BIN"
export LD_LIBRARY_PATH="$T/$BIN/usr/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
if ! OUT=$("$T/bin/$BIN" --version 2>&1); then OUT=""; fi
V=$(printf '%s\n' "$OUT" | head -n 1)
if [ -z "$V" ]; then
  echo "安装后无法运行"
  exit 6
fi
cd "$T" || true
chmod -R u+w "$D" 2>/dev/null || true
rm -rf "$D"
echo "版本：$V"
echo "OK: $T/bin/$BIN"
"#;

/// 自动安装一个工具，返回远端命令的完整输出。
///
/// `progress` 会在安装线程里被逐行调用（界面据此滚动日志）。
pub fn install(
    target: &RemoteTarget,
    tool: RemoteTool,
    progress: &mut dyn FnMut(&str),
) -> Result<String, String> {
    let script = install_script(tool)
        .ok_or_else(|| format!("{}：只能由用户自行安装（自动安装暂不支持）", tool.id()))?;
    progress(&format!("开始自动安装 {}…", tool.id()));
    run_script(target, &script, INSTALL_TIMEOUT, progress)
}

/// 手动安装提示（自动安装不了的工具）。
pub fn manual_hint(tool: RemoteTool) -> &'static str {
    match tool {
        RemoteTool::Clangd => "Debian/Ubuntu: sudo apt install clangd；RHEL/CentOS: sudo yum install llvm-toolset-7-clang-tools-extra",
        RemoteTool::Rg => "Debian/Ubuntu: sudo apt install ripgrep；RHEL/CentOS: sudo yum install ripgrep",
        RemoteTool::Gdb => "Debian/Ubuntu: sudo apt install gdb；RHEL/CentOS: sudo yum install gdb",
    }
}

/// 在远端跑一段脚本，逐行回调输出，带超时。
fn run_script(
    target: &RemoteTarget,
    script: &str,
    timeout: Duration,
    progress: &mut dyn FnMut(&str),
) -> Result<String, String> {
    let (program, args) = target.shell_argv(script);
    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("启动远程命令失败（{}）: {e}", target.label()))?;
    let stdout = child.stdout.take().ok_or("拿不到远程命令 stdout")?;
    let stderr = child.stderr.take().ok_or("拿不到远程命令 stderr")?;

    let (tx, rx) = mpsc::channel::<String>();
    let tx_err = tx.clone();
    let out_thread = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let err_thread = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx_err.send(format!("[stderr] {line}")).is_err() {
                break;
            }
        }
    });
    let mut lines: Vec<String> = Vec::new();
    let mut status = None;
    let mut timed_out = false;
    let deadline = Instant::now() + timeout;
    loop {
        while let Ok(line) = rx.try_recv() {
            push_line(&mut lines, &line, progress);
        }
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                break;
            }
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("等待远程命令失败: {e}"));
            }
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    // 子进程已退出：管道很快 EOF，给读取线程一点时间把最后几行送过来。
    let drain_until = Instant::now() + Duration::from_millis(400);
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => push_line(&mut lines, &line, progress),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= drain_until {
                    break;
                }
            }
        }
    }
    let _ = out_thread.join();
    let _ = err_thread.join();

    let tail: Vec<String> = lines.iter().rev().take(12).rev().cloned().collect();
    if timed_out {
        return Err(format!(
            "远程命令超时（{} 秒）\n{}",
            timeout.as_secs(),
            tail.join("\n")
        ));
    }
    match status {
        Some(s) if s.success() => Ok(lines.join("\n")),
        Some(s) => Err(format!(
            "远程命令失败（exit {}）\n{}",
            s.code().unwrap_or(-1),
            tail.join("\n")
        )),
        None => Err("远程命令没有返回状态".to_string()),
    }
}

fn push_line(lines: &mut Vec<String>, line: &str, progress: &mut dyn FnMut(&str)) {
    let line = line.trim_end();
    if line.trim().is_empty() {
        return;
    }
    progress(line.trim());
    lines.push(line.to_string());
    if lines.len() > 400 {
        lines.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_script_covers_every_tool() {
        let s = probe_script();
        for tool in RemoteTool::ALL {
            assert!(s.contains(&format!("command -v {}", tool.id())), "{s}");
            assert!(s.contains(&format!("echo \"ok {} $p\"", tool.id())), "{s}");
            assert!(s.contains(&format!("missing {}", tool.id())), "{s}");
        }
        // 子 shell 才能吞掉解析器的 `exit 127`。
        assert!(s.contains("p=$("));
    }

    #[test]
    fn install_script_is_rootless_and_lands_in_tools_dir() {
        let s = install_script(RemoteTool::Clangd).expect("clangd 支持自动安装");
        assert!(s.contains(REMOTE_TOOLS_PATH));
        // 官方预编译包 + 镜像回退（远端连不上 github.com 也能装）。
        assert!(s.contains("clangd-linux-$V.zip"));
        assert!(s.contains("ghfast.top"));
        assert!(s.contains("gh-proxy.com"));
        // 老系统（glibc 2.17）要自带运行时并用自带加载器启动。
        assert!(s.contains("ld-2.27.so"));
        assert!(s.contains("libc6_2.27"));
        assert!(s.contains("--library-path"));
        assert!(s.contains("--version"));
        // 提示文案里可能出现「需要 sudo」，但绝不能真的调用 sudo。
        assert!(
            !s.lines().any(|l| l.trim_start().starts_with("sudo")),
            "自动安装绝不能依赖 sudo：{s}"
        );

        let rg = install_script(RemoteTool::Rg).expect("rg 支持自动安装");
        assert!(rg.contains("yumdownloader --resolve"));
        assert!(rg.contains("ripgrep"));
        assert!(rg.contains("rpm2cpio"));
        assert!(rg.contains("cpio -idmu"));
        assert!(!rg.contains("yum install"), "不能真的装进系统：{rg}");
    }

    #[test]
    fn gdb_is_manual_only() {
        assert!(!RemoteTool::Gdb.auto_installable());
        assert!(install_script(RemoteTool::Gdb).is_none());
        assert!(manual_hint(RemoteTool::Gdb).contains("gdb"));
    }

    #[test]
    fn probe_summary_lists_missing() {
        let probe = ToolchainProbe {
            found: vec![(RemoteTool::Rg, "/usr/bin/rg".to_string())],
            missing: vec![RemoteTool::Clangd, RemoteTool::Gdb],
        };
        assert!(!probe.is_complete());
        assert_eq!(probe.summary(), "远端缺少工具：clangd、gdb");
        assert_eq!(probe.path_of(RemoteTool::Rg), Some("/usr/bin/rg"));
        assert!(ToolchainProbe::default().is_complete());
    }

    /// 真实 SSH 验收：一次往返拿到三个工具的探测结果。
    ///
    /// `cargo test -p ide-remote ssh_probe_toolchain -- --ignored --nocapture`
    #[test]
    #[ignore = "需要一台可免密登录的 SSH 主机（LITEIDE_SSH_TEST_HOST/USER）"]
    fn ssh_probe_toolchain() {
        let target = ssh_test_target();
        let probe = probe(&target).expect("探测应成功");
        println!("found: {:?}", probe.found);
        println!("missing: {:?}", probe.missing);
        assert!(!probe.found.is_empty() || !probe.missing.is_empty());
    }

    /// 真实 SSH 验收：自动安装 clangd（免 root），装完探测必须能找到。
    ///
    /// `cargo test -p ide-remote ssh_install_clangd -- --ignored --nocapture`
    #[test]
    #[ignore = "会在远端下载并解包 RPM（约 1 分钟）"]
    fn ssh_install_clangd() {
        let target = ssh_test_target();
        let out = install(&target, RemoteTool::Clangd, &mut |line| {
            println!("[install] {line}")
        })
        .expect("自动安装应成功");
        assert!(out.contains("OK:"), "安装输出里应有 OK: {out}");
        let probe = probe(&target).expect("装完探测应成功");
        let path = probe
            .path_of(RemoteTool::Clangd)
            .expect("装完应能解析出 clangd");
        assert!(path.contains(".liteide/tools"), "应命中自动安装目录: {path}");
    }

    fn ssh_test_target() -> RemoteTarget {
        let user = std::env::var("LITEIDE_SSH_TEST_USER").unwrap_or_else(|_| "fubin".to_string());
        let host = std::env::var("LITEIDE_SSH_TEST_HOST")
            .unwrap_or_else(|_| "192.168.77.6".to_string());
        RemoteTarget::ssh(user, host, 22)
    }
}
