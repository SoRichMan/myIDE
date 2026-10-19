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

/// `file://` URI → Windows 路径。
pub fn uri_to_win(uri: &str) -> Option<String> {
    let wsl = uri_to_wsl(uri)?;
    wsl_to_win(&wsl)
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
}
