//! Windows 路径 ↔ WSL 路径 ↔ `file://` URI 转换。
//!
//! 架构：IDE 跑在 Windows，clangd 跑在 WSL（Ubuntu）。文件在 Windows 磁盘上，
//! WSL 通过 `/mnt/<盘符>/...` 访问。clangd 只能读写 WSL 视角的路径，因此：
//!
//! - 发给 clangd 的 URI：`E:\proj\src\a.cpp` → `file:///mnt/e/proj/src/a.cpp`
//! - clangd 返回的 URI：`file:///mnt/e/proj/src/a.cpp` → `E:\proj\src\a.cpp`
//!
//! 限制：仅支持 NTFS 盘符路径（`C:` / `E:` 等），不支持 UNC/网络路径（阶段2 不涉及）。

/// Windows 路径 → WSL 路径。`E:\a\b` / `e:/a/b` → `/mnt/e/a/b`。
pub fn win_to_wsl(path: &str) -> Option<String> {
    let p = path.replace('\\', "/");
    let p = p.trim_start_matches('/');
    let (drive, rest) = p.split_once(':')?;
    if drive.len() != 1 || !drive.chars().next()?.is_ascii_alphabetic() {
        return None;
    }
    let rest = rest.trim_start_matches('/');
    let drive = drive.to_ascii_lowercase();
    if rest.is_empty() {
        Some(format!("/mnt/{drive}"))
    } else {
        Some(format!("/mnt/{drive}/{rest}"))
    }
}

/// WSL 路径 → Windows 路径。`/mnt/e/a/b` → `E:\a\b`，`/mnt/d` → `D:\`。
pub fn wsl_to_win(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/mnt/")?;
    let (drive, rest) = match rest.split_once('/') {
        Some((d, r)) => (d, Some(r)),
        None => (rest, None),
    };
    if drive.len() != 1 || !drive.chars().next()?.is_ascii_alphabetic() {
        return None;
    }
    let drive = drive.to_ascii_uppercase();
    match rest {
        Some(r) if !r.is_empty() => Some(format!("{drive}:\\{}", r.replace('/', "\\"))),
        _ => Some(format!("{drive}:\\")),
    }
}

/// Windows 路径 → `file://` URI（WSL 视角）。
pub fn win_to_uri(path: &str) -> Option<String> {
    let wsl = win_to_wsl(path)?;
    Some(format!("file://{}", percent_encode(&wsl)))
}

/// `file://` URI → WSL 路径。
pub fn uri_to_wsl(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_decode(rest);
    if decoded.starts_with("/mnt/") {
        Some(decoded)
    } else {
        None
    }
}

/// POSIX 路径 → `file://` URI（WSL 内部视角：`/home/x` → `file:///home/x`）。
///
/// 远程工作区里 clangd 也在 WSL 内部，根和文档都用这种形态，
/// 不再把 Linux 文件伪装成 `file://wsl.localhost/...`。
pub fn posix_to_uri(path: &str) -> Option<String> {
    let p = path.trim().replace('\\', "/");
    if !p.starts_with('/') {
        return None;
    }
    Some(format!("file://{}", percent_encode(&p)))
}

/// `file://` URI → POSIX 路径（`file:///home/x` → `/home/x`）。
///
/// `/mnt/<drive>` 这类 WSL 挂载点也原样返回（那就是发行版里的真实路径）。
pub fn uri_to_posix(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_decode(rest);
    if decoded.starts_with('/') {
        Some(decoded)
    } else {
        None
    }
}

/// `file://` URI → Windows 路径（含远程工作区的 UNC 路径）。
///
/// 远程工作区里 clangd 返回的是 `file://wsl.localhost/<distro>/...`，
/// 对应 Windows 侧的 `\\wsl.localhost\<distro>\...`（9P 挂载可直接读写）。
pub fn uri_to_win(uri: &str) -> Option<String> {
    if let Some(rest) = uri.strip_prefix("file://wsl.localhost/") {
        let rest = percent_decode(rest);
        let (distro, tail) = rest.split_once('/').unwrap_or((rest.as_str(), ""));
        let unc = wsl_dir_to_unc(distro, &format!("/{tail}"));
        return Some(unc.trim_end_matches('\\').to_string());
    }
    if let Some(rest) = uri.strip_prefix("file://wsl$/") {
        let rest = percent_decode(rest);
        let (distro, tail) = rest.split_once('/').unwrap_or((rest.as_str(), ""));
        let unc = wsl_dir_to_unc(distro, &format!("/{tail}"));
        return Some(unc.trim_end_matches('\\').to_string());
    }
    let wsl = uri_to_wsl(uri)?;
    wsl_to_win(&wsl)
}

// ---------------------------------------------------------------------------
// 远程（WSL 发行版内的）路径
// ---------------------------------------------------------------------------

/// WSL 发行版里的目录 → Windows 可访问的 UNC 路径。
///
/// IDE 进程跑在 Windows 上，读目录/读文件只要路径能被 Win32 打开就行；
/// WSL 发行版内的 `/home/work/proj` 在 Windows 侧就是
/// `\\wsl.localhost\Ubuntu2404\home\work\proj`（9P 挂载，读写等同）。
/// 这样「远程打开」不需要为每个文件都套一层 `wsl.exe cat`。
pub fn wsl_dir_to_unc(distro: &str, posix: &str) -> String {
    let posix = posix.trim();
    let posix = posix.replace('\\', "/");
    let rest = posix.trim_start_matches('/');
    if rest.is_empty() {
        format!(r"\\wsl.localhost\{distro}\")
    } else {
        format!(r"\\wsl.localhost\{distro}\{}", rest.replace('/', r"\"))
    }
}

/// UNC 路径是否是 WSL 发行版挂载（`\\wsl.localhost\<distro>\...` 或旧式
/// `\\wsl$\<distro>\...`）。返回发行版名。
pub fn unc_wsl_distro(path: &str) -> Option<String> {
    let p = path.replace('/', r"\");
    let rest = p
        .strip_prefix(r"\\wsl.localhost\")
        .or_else(|| p.strip_prefix(r"\\wsl$\"))?;
    let (distro, _) = rest.split_once('\\').unwrap_or((rest, ""));
    if distro.is_empty() {
        None
    } else {
        Some(distro.to_string())
    }
}

/// UNC 路径 → 发行版内的 POSIX 路径；非 WSL UNC 返回 None。
pub fn unc_to_wsl_posix(path: &str) -> Option<String> {
    let p = path.replace('/', r"\");
    let rest = p
        .strip_prefix(r"\\wsl.localhost\")
        .or_else(|| p.strip_prefix(r"\\wsl$\"))?;
    let (_distro, tail) = rest.split_once('\\')?;
    Some(format!("/{}", tail.replace('\\', "/")))
}

/// 「远程根」→ 根下的 URI：UNC 路径走 9P 形态（`file://wsl.localhost/<distro>/...`，
/// clangd 正好认这种写法），盘符路径走 `/mnt/<drive>` 形态。
pub fn path_to_uri_any(path: &str) -> Option<String> {
    let p = path.replace('/', r"\");
    if let Some(rest) = p
        .strip_prefix(r"\\wsl.localhost\")
        .or_else(|| p.strip_prefix(r"\\wsl$\"))
    {
        let (distro, tail) = rest.split_once('\\').unwrap_or((rest, ""));
        let tail = tail.replace('\\', "/");
        let body = if tail.is_empty() {
            format!("wsl.localhost/{distro}/")
        } else {
            format!("wsl.localhost/{distro}/{tail}")
        };
        return Some(format!("file://{}", percent_encode(&body)));
    }
    win_to_uri(path)
}

/// 百分号编码：只编码保留字符之外的部分，`/` 保留（路径语义）。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 百分号解码（`%XX` → 字节，按 UTF-8 重新组装）。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_to_wsl_basic() {
        assert_eq!(
            win_to_wsl(r"E:\TestDeepSeek\testdata\sample_project").as_deref(),
            Some("/mnt/e/TestDeepSeek/testdata/sample_project")
        );
        assert_eq!(win_to_wsl(r"e:\lower\case").as_deref(), Some("/mnt/e/lower/case"));
        assert_eq!(win_to_wsl("C:/fwd/slash").as_deref(), Some("/mnt/c/fwd/slash"));
        assert_eq!(win_to_wsl(r"E:\").as_deref(), Some("/mnt/e"));
        assert_eq!(win_to_wsl(r"D:\only\file.cpp").as_deref(), Some("/mnt/d/only/file.cpp"));
    }

    #[test]
    fn win_to_wsl_rejects_non_drive() {
        assert_eq!(win_to_wsl(r"\\.\pipe\foo"), None);
        assert_eq!(win_to_wsl("relative/path"), None);
        assert_eq!(win_to_wsl(r"\\server\share\x"), None);
    }

    #[test]
    fn wsl_to_win_basic() {
        assert_eq!(
            wsl_to_win("/mnt/e/TestDeepSeek/testdata").as_deref(),
            Some(r"E:\TestDeepSeek\testdata")
        );
        assert_eq!(wsl_to_win("/mnt/c/Users/me").as_deref(), Some(r"C:\Users\me"));
        assert_eq!(wsl_to_win("/mnt/d").as_deref(), Some(r"D:\"));
    }

    #[test]
    fn roundtrip_win_wsl() {
        for p in [r"E:\TestDeepSeek\crates\ide-lsp\src\lib.rs", r"C:\a b\c", r"E:\"] {
            let wsl = win_to_wsl(p).unwrap();
            let back = wsl_to_win(&wsl).unwrap();
            assert_eq!(back, p, "roundtrip failed for {p}");
        }
    }

    #[test]
    fn uri_roundtrip_with_spaces() {
        let uri = win_to_uri(r"E:\TestDeepSeek\testdata\sample_project\src\main.cpp").unwrap();
        assert_eq!(uri, "file:///mnt/e/TestDeepSeek/testdata/sample_project/src/main.cpp");
        assert_eq!(uri_to_win(&uri).as_deref(), Some(r"E:\TestDeepSeek\testdata\sample_project\src\main.cpp"));

        let spaced = win_to_uri(r"E:\my proj\a b.cpp").unwrap();
        assert_eq!(spaced, "file:///mnt/e/my%20proj/a%20b.cpp");
        assert_eq!(uri_to_win(&spaced).as_deref(), Some(r"E:\my proj\a b.cpp"));
    }

    #[test]
    fn uri_to_win_rejects_non_mnt() {
        assert_eq!(uri_to_win("file:///home/user/a.cpp"), None);
        assert_eq!(uri_to_win("https://x/y"), None);
        assert_eq!(uri_to_win("not a uri"), None);
    }

    #[test]
    fn wsl_dir_to_unc_and_back() {
        assert_eq!(
            wsl_dir_to_unc("Ubuntu2404", "/home/work/NewLithoTune/candanza"),
            r"\\wsl.localhost\Ubuntu2404\home\work\NewLithoTune\candanza"
        );
        assert_eq!(
            wsl_dir_to_unc("Ubuntu2404", "/"),
            r"\\wsl.localhost\Ubuntu2404\"
        );
        // UNC → POSIX 回环（正反斜杠都接受）。
        let unc = wsl_dir_to_unc("Ubuntu2404", "/home/work/x y");
        assert_eq!(unc_to_wsl_posix(&unc).as_deref(), Some("/home/work/x y"));
        assert_eq!(unc_wsl_distro(&unc).as_deref(), Some("Ubuntu2404"));
        assert_eq!(
            unc_to_wsl_posix(r"\\wsl$\Debian\srv\a").as_deref(),
            Some("/srv/a")
        );
        // 普通盘符路径不是 WSL UNC。
        assert_eq!(unc_wsl_distro(r"E:\proj"), None);
        assert_eq!(unc_to_wsl_posix(r"\\server\share\x"), None);
    }

    #[test]
    fn uri_to_win_handles_wsl_remote() {
        assert_eq!(
            uri_to_win("file://wsl.localhost/Ubuntu2404/home/work/proj/main.cpp").as_deref(),
            Some(r"\\wsl.localhost\Ubuntu2404\home\work\proj\main.cpp")
        );
        // 带空格（URI 里是 %20）。
        assert_eq!(
            uri_to_win("file://wsl.localhost/Ubuntu2404/home/my%20proj/a.b").as_deref(),
            Some(r"\\wsl.localhost\Ubuntu2404\home\my proj\a.b")
        );
    }

    #[test]
    fn posix_uri_roundtrip() {
        assert_eq!(
            posix_to_uri("/home/work/NewLithoTune/candanza/src/a.cpp").as_deref(),
            Some("file:///home/work/NewLithoTune/candanza/src/a.cpp")
        );
        assert_eq!(
            posix_to_uri("/home/my proj/a.cpp").as_deref(),
            Some("file:///home/my%20proj/a.cpp")
        );
        assert_eq!(
            uri_to_posix("file:///home/my%20proj/a.cpp").as_deref(),
            Some("/home/my proj/a.cpp")
        );
        assert_eq!(posix_to_uri("relative/x"), None);
        assert_eq!(uri_to_posix("https://x/y"), None);
    }

    #[test]
    fn path_to_uri_any_handles_unc_and_drive() {
        let unc = wsl_dir_to_unc("Ubuntu2404", "/home/work/proj");
        assert_eq!(
            path_to_uri_any(&unc).as_deref(),
            Some("file://wsl.localhost/Ubuntu2404/home/work/proj")
        );
        // 带空格要百分号编码。
        let spaced = wsl_dir_to_unc("Ubuntu2404", "/home/work/my proj");
        assert_eq!(
            path_to_uri_any(&spaced).as_deref(),
            Some("file://wsl.localhost/Ubuntu2404/home/work/my%20proj")
        );
        assert_eq!(
            path_to_uri_any(r"E:\proj\a.cpp").as_deref(),
            Some("file:///mnt/e/proj/a.cpp")
        );
    }
}
