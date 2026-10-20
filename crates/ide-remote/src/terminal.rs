//! WSL 内 PTY 终端：一个 `wsl.exe -d <distro> --cd <posix>` 进程 + ConPTY。
//!
//! 集成终端（编辑区下方 Split 出来的面板）用的就是这里：`portable-pty` 在 Windows 上
//! 走 ConPTY，WSL 里拿到的是真正的 Linux tty（有提示符、行编辑、颜色、Ctrl+C、vim/top
//! 这类全屏程序也能跑），而不是拿管道接一个非交互 bash。
//!
//! 读写都在后台线程里：PTY 输出可能很快（`cmake` 一屏一屏刷），UI 线程只做
//! `try_recv` 非阻塞排空，绝不在这里等 IO。

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

/// 在 WSL 里跑一个交互式 shell 的常驻 PTY 会话。
///
/// 生命周期：
/// - [`WslTerminal::spawn`] 建 PTY 并拉起 `wsl.exe`；
/// - 后台线程持续把 PTY 输出送进 channel，UI 线程用 [`WslTerminal::drain`] 非阻塞取；
/// - [`WslTerminal::write`] 送键盘输入；
/// - [`WslTerminal::resize`] 跟随面板尺寸（WSL 里的程序会收到 SIGWINCH）；
/// - drop / [`WslTerminal::kill`] 结束进程。
pub struct WslTerminal {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    rx: Receiver<TermOutput>,
    distro: String,
    cwd: String,
    cols: u16,
    rows: u16,
    exited: bool,
}

impl WslTerminal {
    /// 在 `distro` 的 `posix_cwd` 目录下启动登录 shell。
    ///
    /// `TERM=xterm-256color` / `COLORTERM=truecolor` 让 WSL 里的程序输出 256 色 / 真彩色；
    /// 其余环境变量来自 WSL 用户自己的登录环境（`wsl.exe` 负责）。
    pub fn spawn(distro: &str, posix_cwd: &str, cols: u16, rows: u16) -> Result<Self, String> {
        Self::spawn_with_args(distro, posix_cwd, cols, rows, &[])
    }

    /// 同 [`spawn`](Self::spawn)，但给 `wsl.exe` 追加参数。
    ///
    /// 默认启动发行版的登录 shell（会加载 `.bashrc` 等）；测试或未来的自定义终端配置
    /// 可以用它换成 `--exec bash --noprofile --norc -i` 这类干净 shell。
    pub fn spawn_with_args(
        distro: &str,
        posix_cwd: &str,
        cols: u16,
        rows: u16,
        extra_args: &[&str],
    ) -> Result<Self, String> {
        let distro = distro.trim();
        if distro.is_empty() {
            return Err("未指定 WSL 发行版".to_string());
        }
        let cwd = crate::normalize_posix(posix_cwd);
        let cols = cols.max(2);
        let rows = rows.max(2);

        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("创建 WSL 终端失败: {e}"))?;

        let mut cmd = CommandBuilder::new("wsl.exe");
        cmd.args(["-d", distro, "--cd", cwd.as_str()]);
        cmd.args(extra_args);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("启动 WSL 终端失败（{distro}）: {e}"))?;
        // slave 必须尽早释放：它一直开着的话 PTY 永远不会 EOF。
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("读取 WSL 终端输出失败: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("写入 WSL 终端失败: {e}"))?;

        let (tx, rx) = mpsc::channel::<TermOutput>();
        std::thread::Builder::new()
            .name("wsl-pty-read".to_string())
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
            .map_err(|e| format!("启动 WSL 终端读取线程失败: {e}"))?;

        tracing::info!("[terminal] 启动 WSL PTY: {distro}:{cwd} ({cols}x{rows})");
        Ok(Self {
            master: pair.master,
            child,
            writer,
            rx,
            distro: distro.to_string(),
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
                format!("写入 WSL 终端失败: {e}")
            })
    }

    /// 面板尺寸变化时同步给 PTY（WSL 里的程序收到 SIGWINCH 后会重排）。
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

    pub fn distro(&self) -> &str {
        &self.distro
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

impl Drop for WslTerminal {
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
        // 用 `--norc` 的干净 shell：默认登录 shell 会加载用户的 `.bashrc`，
        // 里面可能有联网/隧道脚本，测试不能依赖那套环境。
        let mut term = WslTerminal::spawn_with_args(
            &distro(),
            "/tmp",
            100,
            30,
            &["--exec", "bash", "--noprofile", "--norc", "-i"],
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

        // resize 不应报错，且终端宽度变化后 WSL 里的 `tput cols` 能读到新值。
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
}
