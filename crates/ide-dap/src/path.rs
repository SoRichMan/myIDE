//! Windows 路径 ↔ WSL 路径转换（阶段 3）。
//!
//! IDE 跑在 Windows，gdb 跑在 WSL（Ubuntu）。文件在 Windows 磁盘上，
//! WSL 通过 `/mnt/<盘符>/...` 访问。gdb 只能读写 WSL 视角的路径，因此：
//!
//! - 发给 gdb 的路径：`E:\proj\src\a.cpp` → `/mnt/e/proj/src/a.cpp`
//! - gdb 返回的路径：`/mnt/e/proj/src/a.cpp` → `E:\proj\src\a.cpp`
//!
//! 限制：仅支持 NTFS 盘符路径（`C:` / `E:` 等），不支持 UNC/网络路径。

/// Windows 路径 → 远端视角路径。`E:\a\b` / `e:/a/b` → `/mnt/e/a/b`。
///
/// SSH 远程工作区里的路径本来就是远端 POSIX 路径（`/home/fubin/...`），
/// 原样透传：不能按盘符规则重写，否则 gdb 找不到程序。
pub fn win_to_wsl(path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.starts_with('/') {
        return Some(trimmed.replace('\\', "/"));
    }
    let p = trimmed.replace('\\', "/");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_to_wsl_basic() {
        assert_eq!(
            win_to_wsl(r"E:\TestDeepSeek\testdata\debug_target.cpp").as_deref(),
            Some("/mnt/e/TestDeepSeek/testdata/debug_target.cpp")
        );
        assert_eq!(win_to_wsl("C:/fwd/slash").as_deref(), Some("/mnt/c/fwd/slash"));
        assert_eq!(win_to_wsl(r"E:\").as_deref(), Some("/mnt/e"));
        assert_eq!(win_to_wsl(r"D:\only\file.cpp").as_deref(), Some("/mnt/d/only/file.cpp"));
    }

    #[test]
    fn win_to_wsl_rejects_non_drive() {
        assert_eq!(win_to_wsl(r"\\.\pipe\foo"), None);
        assert_eq!(win_to_wsl("relative/path"), None);
    }

    #[test]
    fn win_to_wsl_passes_through_remote_posix() {
        // SSH 远程工作区：程序路径本来就是远端 POSIX 路径，必须原样透传。
        assert_eq!(
            win_to_wsl("/home/fubin/work/candanza/build/app").as_deref(),
            Some("/home/fubin/work/candanza/build/app")
        );
        // 已经有 /mnt/ 前缀的路径同样透传（不再二次加 /mnt）。
        assert_eq!(
            win_to_wsl("/mnt/e/proj/a.out").as_deref(),
            Some("/mnt/e/proj/a.out")
        );
    }

    #[test]
    fn wsl_to_win_basic() {
        assert_eq!(
            wsl_to_win("/mnt/e/TestDeepSeek/testdata/debug_target.cpp").as_deref(),
            Some(r"E:\TestDeepSeek\testdata\debug_target.cpp")
        );
        assert_eq!(wsl_to_win("/mnt/d").as_deref(), Some(r"D:\"));
    }

    #[test]
    fn roundtrip_win_wsl() {
        for p in [r"E:\TestDeepSeek\crates\ide-dap\src\lib.rs", r"C:\a b\c", r"E:\"] {
            let wsl = win_to_wsl(p).unwrap();
            let back = wsl_to_win(&wsl).unwrap();
            assert_eq!(back, p, "roundtrip failed for {p}");
        }
    }
}
