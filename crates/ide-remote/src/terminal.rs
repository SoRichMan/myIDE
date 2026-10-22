//! 远端 PTY 终端：一个 `wsl.exe -d <distro> --cd <posix>`（WSL）或
//! `ssh -t ... user@host 'cd <posix> && exec bash -l'`（SSH）进程 + ConPTY。
//!
//! 集成终端（编辑区下方 Split 出来的面板）用的就是这里：`portable-pty` 在 Windows 上
//! 走 ConPTY，远端拿到的是真正的 Linux tty（有提示符、行编辑、颜色、Ctrl+C、vim/top
//! 这类全屏程序也能跑），而不是拿管道接一个非交互 bash。
//!
//! 读写都在后台线程里：PTY 输出可能很快（`cmake` 一屏一屏刷），UI 线程只做
//! `try_recv` 非阻塞排空，绝不在这里等 IO。

use ide_core::remote::{shell_quote, wsl_exe, RemoteTarget};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// 一次排空的输出。
#[derive(Debug, Default)]
pub struct TermOutput {
    pub bytes: Vec<u8>,
    /// 进程已退出（或管道断开）。退出后仍可能有最后一批 bytes，调用方先处理 bytes。
    pub exited: bool,
}

/// 在远端跑一个交互式 shell 的常驻 PTY 会话。
///
/// 生命周期：
/// - [`RemoteTerminal::spawn`] 建 PTY 并拉起 `wsl.exe` / `ssh`；
/// - 后台线程持续把 PTY 输出送进 channel，UI 线程用 [`RemoteTerminal::drain`] 非阻塞取；
/// - [`RemoteTerminal::write`] 送键盘输入；
/// - [`RemoteTerminal::resize`] 跟随面板尺寸（远端程序会收到 SIGWINCH）；
/// - drop / [`RemoteTerminal::kill`] 结束进程。
pub struct RemoteTerminal {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    rx: Receiver<TermOutput>,
    target: RemoteTarget,
    cwd: String,
    cols: u16,
    rows: u16,
    exited: bool,
}

impl RemoteTerminal {
    /// 在远端 `posix_cwd` 目录下启动登录 shell。
    ///
    /// `TERM=xterm-256color` / `COLORTERM=truecolor` 让远端程序输出 256 色 / 真彩色；
    /// 其余环境变量来自远端用户自己的登录环境（`wsl.exe` / `ssh` + `bash -l` 负责）。
    pub fn spawn(
        target: &RemoteTarget,
        posix_cwd: &str,
        cols: u16,
        rows: u16,
    ) -> Result<Self, String> {
        Self::spawn_with_args(target, posix_cwd, cols, rows, &[])
    }

    /// 同 [`spawn`](Self::spawn)，但可以换掉最终 shell 的参数。
    ///
    /// `extra_args` 为空：启动登录 shell（会加载 `.bashrc` 等，默认行为）；
    /// 非空：**替换**那组默认参数，用来跑干净 shell（测试里的
    /// `--noprofile --norc -i`）或以后给用户配的自定义启动参数。
    pub fn spawn_with_args(
        target: &RemoteTarget,
        posix_cwd: &str,
        cols: u16,
        rows: u16,
        extra_args: &[&str],
    ) -> Result<Self, String> {
        if let RemoteTarget::Wsl { distro } = target {
            if distro.trim().is_empty() {
                return Err("未指定 WSL 发行版".to_string());
            }
        }
        let cwd = crate::normalize_posix(posix_cwd);
        let cols = cols.max(2);
        let rows = rows.max(2);

        let (program, args) = match target {
            RemoteTarget::Wsl { distro } => {
                // `--cd` 由 wsl.exe 自己切目录，不经过 shell；`--exec` 之后的参数
                // 原样交给 bash（`--noprofile` 这类不能直接给 wsl.exe）。
                let mut a: Vec<String> = vec![
                    "-d".into(),
                    distro.clone(),
                    "--cd".into(),
                    cwd.clone(),
                    "--exec".into(),
                    "bash".into(),
                ];
                if extra_args.is_empty() {
                    a.push("-l".into());
                } else {
                    a.extend(extra_args.iter().map(|s| s.to_string()));
                }
                (wsl_exe(), a)
            }
            RemoteTarget::Ssh { .. } => {
                let mut script = format!("cd {} && exec bash", shell_quote(&cwd));
                if extra_args.is_empty() {
                    script.push_str(" -l");
                } else {
                    for a in extra_args {
                        script.push(' ');
                        script.push_str(&shell_quote(a));
                    }
                }
                target.interactive_argv(&script)
            }
        };

        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("创建远程终端失败: {e}"))?;

        let mut cmd = CommandBuilder::new(&program);
        cmd.args(&args);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("启动远程终端失败（{}）: {e}", target.label()))?;
        // slave 必须尽早释放：它一直开着的话 PTY 永远不会 EOF。
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("读取远程终端输出失败: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("写入远程终端失败: {e}"))?;

        let (tx, rx) = mpsc::channel::<TermOutput>();
        std::thread::Builder::new()
            .name("remote-pty-read".to_string())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if tx
                                .send(TermOutput {
                                    bytes: buf[..n].to_vec(),
                                    exited: false,
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                let _ = tx.send(TermOutput {
                    bytes: Vec::new(),
                    exited: true,
                });
            })
            .map_err(|e| format!("启动远程终端读取线程失败: {e}"))?;

        tracing::info!(
            "[terminal] 启动远程 PTY: {}:{cwd} ({cols}x{rows})",
            target.label()
        );
        Ok(Self {
            master: pair.master,
            child,
            writer,
            rx,
            target: target.clone(),
            cwd,
            cols,
            rows,
            exited: false,
        })
    }

    /// 非阻塞排空输出（UI 每帧调用）。
    pub fn drain(&mut self) -> TermOutput {
        let mut out = TermOutput::default();
        loop {
            match self.rx.try_recv() {
                Ok(item) => {
                    out.bytes.extend_from_slice(&item.bytes);
                    out.exited |= item.exited;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    out.exited = true;
                    break;
                }
            }
        }
        if !out.exited && !self.exited {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => out.exited = true,
                Ok(None) => {}
            }
        }
        self.exited |= out.exited;
        out
    }

    /// 把键盘输入写进 PTY。会话已退出时静默忽略（面板上会显示退出提示）。
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.exited || bytes.is_empty() {
            return Ok(());
        }
        self.writer
            .write_all(bytes)
            .and_then(|_| self.writer.flush())
            .map_err(|e| {
                self.exited = true;
                format!("写入远程终端失败: {e}")
            })
    }

    /// 面板尺寸变化时同步给 PTY（远端程序收到 SIGWINCH 后会重排）。
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(2);
        let rows = rows.max(2);
        if self.exited || (cols == self.cols && rows == self.rows) {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    pub fn dimensions(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    pub fn is_exited(&self) -> bool {
        self.exited
    }

    pub fn target(&self) -> &RemoteTarget {
        &self.target
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// 主动结束（关闭标签 / 断开工作区 / 退出程序）。
    pub fn kill(&mut self) {
        if self.exited {
            return;
        }
        self.exited = true;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for RemoteTerminal {
    fn drop(&mut self) {
        if !self.exited {
            let _ = self.child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn distro() -> String {
        std::env::var("LITEIDE_WSL_DISTRO").unwrap_or_else(|_| "Ubuntu2404".to_string())
    }

    /// 手工验收（默认不跑，CI 不依赖 WSL）：
    /// `cargo test -p ide-remote wsl_terminal -- --ignored --nocapture`
    #[test]
    #[ignore = "需要本机 WSL 发行版 Ubuntu2404"]
    fn wsl_terminal_runs_commands_and_reports_exit() {
        let target = RemoteTarget::wsl(distro());
        // 用 `--norc` 的干净 shell：默认登录 shell 会加载用户的 `.bashrc`，
        // 里面可能有联网/隧道脚本，测试不能依赖那套环境。
        let mut term = RemoteTerminal::spawn_with_args(
            &target,
            "/tmp",
            100,
            30,
            &["--noprofile", "--norc", "-i"],
        )
        .expect("spawn");
        assert_eq!(term.dimensions(), (100, 30));

        // 等提示符出现再发命令（PTY 行缓冲，命令必须带 \r）。
        let mut boot = Vec::new();
        let boot_deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < boot_deadline {
            let chunk = term.drain();
            if chunk.bytes.windows(4).any(|w| w == b"\x1b[6n") {
                term.write(b"\x1b[1;1R").expect("reply DSR");
            }
            boot.extend_from_slice(&chunk.bytes);
            if boot.windows(2).any(|w| w == b"# " || w == b"$ ") {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            String::from_utf8_lossy(&boot).contains('#'),
            "没等到 shell 提示符: {:?}",
            String::from_utf8_lossy(&boot)
        );

        // 用算术表达式生成标记：命令回显里是 `$((40+2))`，只有执行结果才是 42，
        // 这样不会像直接 echo 常量那样被回显误判。
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        term.write(b"echo LITEIDE_PTY_$((40+2))\r")
            .expect("write");
        while Instant::now() < deadline {
            let chunk = term.drain();
            if chunk.bytes.windows(4).any(|w| w == b"\x1b[6n") {
                term.write(b"\x1b[1;1R").expect("reply DSR");
            }
            out.extend_from_slice(&chunk.bytes);
            if String::from_utf8_lossy(&out).contains("LITEIDE_PTY_42") {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("LITEIDE_PTY_42"),
            "没收到命令输出，实际输出: {text:?}"
        );

        // resize 不应报错，且终端宽度变化后远端 `tput cols` 能读到新值。
        term.resize(137, 40);
        std::thread::sleep(Duration::from_millis(200));
        term.write(b"printf 'COLS=%s\\n' \"$(tput cols)\"\r")
            .expect("write");
        let mut out2 = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let chunk = term.drain();
            if chunk.bytes.windows(4).any(|w| w == b"\x1b[6n") {
                term.write(b"\x1b[1;1R").expect("reply DSR");
            }
            out2.extend_from_slice(&chunk.bytes);
            if String::from_utf8_lossy(&out2).contains("COLS=137") {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            String::from_utf8_lossy(&out2).contains("COLS=137"),
            "resize 后 `tput cols` 不是 137: {:?}",
            String::from_utf8_lossy(&out2)
        );

        term.write(b"exit\r").expect("write");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut exited = false;
        while Instant::now() < deadline {
            let chunk = term.drain();
            if chunk.bytes.windows(4).any(|w| w == b"\x1b[6n") {
                term.write(b"\x1b[1;1R").expect("reply DSR");
            }
            if chunk.exited {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(exited, "`exit` 之后应报告进程退出");
    }

    /// 真实 SSH 终端验收（免密已配好才跑）：
    /// `cargo test -p ide-remote ssh_terminal -- --ignored --nocapture`
    #[test]
    #[ignore = "需要一台可免密登录的 SSH 主机（LITEIDE_SSH_TEST_HOST/USER）"]
    fn ssh_terminal_runs_commands() {
        let user = std::env::var("LITEIDE_SSH_TEST_USER").unwrap_or_else(|_| "fubin".to_string());
        let host = std::env::var("LITEIDE_SSH_TEST_HOST")
            .unwrap_or_else(|_| "192.168.77.6".to_string());
        let target = RemoteTarget::ssh(user, host, 22);
        let mut term = RemoteTerminal::spawn(&target, "/tmp", 100, 30).expect("spawn ssh");

        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(25);
        term.write(b"echo SSH_PTY_$((40+2))\r").expect("write");
        while Instant::now() < deadline {
            let chunk = term.drain();
            if chunk.bytes.windows(4).any(|w| w == b"\x1b[6n") {
                term.write(b"\x1b[1;1R").expect("reply DSR");
            }
            out.extend_from_slice(&chunk.bytes);
            if String::from_utf8_lossy(&out).contains("SSH_PTY_42") {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            String::from_utf8_lossy(&out).contains("SSH_PTY_42"),
            "SSH 终端没收到命令输出: {:?}",
            String::from_utf8_lossy(&out)
        );
        term.kill();
    }
}
