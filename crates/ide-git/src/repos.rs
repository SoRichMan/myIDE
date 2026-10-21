//! 仓库发现：工作区里可能包含多个 git 仓库（VS Code 多仓库工作区）。

use std::path::{Path, PathBuf};

/// 递归查找时跳过的目录（构建产物 / 依赖）。
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "build",
    "out",
    "dist",
    ".venv",
    "venv",
    "__pycache__",
    ".cache",
    ".idea",
    ".vscode-test",
];

/// 查找工作区内的 git 仓库根目录。
///
/// - 工作区根自身是仓库 → 第一个返回。
/// - 向下最多找 `max_depth` 层（默认 2），子目录里的仓库不再深挖。
pub fn discover_repos(root: &str, max_depth: usize) -> Vec<String> {
    let base = Path::new(root);
    if !base.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if is_repo(base) {
        out.push(base.to_string_lossy().into_owned());
    }
    let mut queue: Vec<(PathBuf, usize)> = vec![(base.to_path_buf(), 0)];
    while let Some((dir, depth)) = queue.pop() {
        if depth >= max_depth {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut children: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        // 稳定顺序（按名字），保证 UI 里仓库顺序不跳变。
        children.sort();
        for child in children {
            let name = child
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            if is_repo(&child) {
                out.push(child.to_string_lossy().into_owned());
                continue; // 仓库内部不再深挖（子模块除外）。
            }
            queue.push((child, depth + 1));
        }
    }
    out
}

/// 目录（或其父目录）是否是 git 仓库。
pub fn is_repo(dir: &Path) -> bool {
    if dir.join(".git").exists() {
        return true;
    }
    // 裸仓库：目录本身就是 .git 的内容。
    dir.join("HEAD").exists() && dir.join("objects").exists()
}

/// 仓库的显示名（目录名）。
pub fn repo_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// 在 WSL（远程工作区）里查找 git 仓库。
///
/// 远程工作区是 POSIX 路径，Windows 侧无法直接遍历，
/// 因此在 WSL 里跑一条 `find`（一次调用，不逐目录往返）。
pub fn discover_repos_wsl(root: &str, distro: &str) -> Vec<String> {
    let script = format!(
        "cd {} && find . -maxdepth 2 -name .git -prune -print 2>/dev/null",
        crate::process::shell_quote(root)
    );
    let Ok(out) = crate::process::run_wsl_blocking(distro, &script) else {
        return Vec::new();
    };
    let mut repos = Vec::new();
    // 根自身是不是仓库
    let head_script = format!(
        "cd {} && git rev-parse --show-toplevel 2>/dev/null",
        crate::process::shell_quote(root)
    );
    if let Ok(toplevel) = crate::process::run_wsl_blocking(distro, &head_script) {
        let t = toplevel.trim();
        if !t.is_empty() {
            repos.push(t.to_string());
        }
    }
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // "./.git" → 根仓库（已由 rev-parse 收集）；"./scripts/.git" → "<root>/scripts"
        let rel = line.strip_prefix("./").unwrap_or(line);
        let rel = rel.strip_suffix(".git").unwrap_or(rel);
        let rel = rel.trim_end_matches('/');
        if rel.is_empty() {
            continue;
        }
        let full = format!("{}/{}", root.trim_end_matches('/'), rel);
        if !repos.contains(&full) {
            repos.push(full);
        }
    }
    repos
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("liteide-repos-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_nested_repos() {
        let base = tmpdir("nested");
        let sub = base.join("sub");
        std::fs::create_dir_all(sub.join(".git")).unwrap();
        std::fs::create_dir_all(base.join("plain")).unwrap();
        let repos = discover_repos(&base.to_string_lossy(), 2);
        assert_eq!(repos.len(), 1);
        assert!(repos[0].ends_with("sub"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn root_repo_is_first() {
        let base = tmpdir("root");
        std::fs::create_dir_all(base.join(".git")).unwrap();
        let repos = discover_repos(&base.to_string_lossy(), 2);
        assert_eq!(repos.len(), 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn repo_name_is_dir_name() {
        assert_eq!(repo_name("E:/work/myrepo"), "myrepo");
    }
}
