//! 资源管理器文件树（复核改进）。
//!
//! 由 `rg --files` 给出的扁平相对路径（`a/b/c.rs`）构建目录层级，支持展开/折叠，
//! 并按展开集合压平为可虚拟化的行列表（`Row`）。纯逻辑、无 UI 依赖，便于单测。
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// 压平后的一行（目录或文件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// 显示名（不含父路径）。
    pub name: String,
    /// 相对根路径（`a/b/c.rs`），目录与文件共用；用作展开集合的键与打开路径。
    pub path: String,
    /// 是否目录。
    pub is_dir: bool,
    /// 缩进层级（0 = 顶层）。
    pub depth: usize,
}

#[derive(Debug, Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    files: BTreeSet<String>,
}

/// 文件树。
#[derive(Debug, Default)]
pub struct FileTree {
    root: Node,
    /// 源文件数（构建完成时记录，供 UI 显示）。
    pub file_count: usize,
}

impl FileTree {
    /// 由扁平相对路径列表构建（`/` 分隔；空段忽略）。
    pub fn build(files: &[String]) -> Self {
        let mut root = Node::default();
        let mut count = 0usize;
        for f in files {
            let parts: Vec<&str> = f.split('/').filter(|s| !s.is_empty()).collect();
            if parts.is_empty() {
                continue;
            }
            let mut cur = &mut root;
            for d in &parts[..parts.len() - 1] {
                cur = cur.dirs.entry((*d).to_string()).or_default();
            }
            cur.files.insert(parts[parts.len() - 1].to_string());
            count += 1;
        }
        Self {
            root,
            file_count: count,
        }
    }

    /// 按展开集合压平为可见行（目录在前、各自字典序）。
    pub fn rows(&self, expanded: &HashSet<String>) -> Vec<Row> {
        let mut out = Vec::new();
        walk(&self.root, "", 0, expanded, &mut out);
        out
    }
}

fn walk(node: &Node, prefix: &str, depth: usize, expanded: &HashSet<String>, out: &mut Vec<Row>) {
    for (name, child) in &node.dirs {
        let path = join(prefix, name);
        out.push(Row {
            name: name.clone(),
            path: path.clone(),
            is_dir: true,
            depth,
        });
        if expanded.contains(&path) {
            walk(child, &path, depth + 1, expanded, out);
        }
    }
    for f in &node.files {
        out.push(Row {
            name: f.clone(),
            path: join(prefix, f),
            is_dir: false,
            depth,
        });
    }
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn builds_and_flattens_dirs_before_files() {
        let tree = FileTree::build(&s(&[
            "src/main.rs",
            "src/lib.rs",
            "Cargo.toml",
            "src/io/mod.rs",
        ]));
        let rows = tree.rows(&HashSet::new());
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        // 顶层：目录 src 在文件 Cargo.toml 之前；src 未展开时不出现其子项
        assert_eq!(names, vec!["src", "Cargo.toml"]);
        assert_eq!(rows[0].depth, 0);
    }

    #[test]
    fn expands_selected_dirs() {
        let tree = FileTree::build(&s(&["src/main.rs", "src/io/mod.rs", "Cargo.toml"]));
        let mut expanded = HashSet::new();
        expanded.insert("src".to_string());
        let rows = tree.rows(&expanded);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        // src 展开：其下 io 目录 + main.rs 文件（目录优先，各自排序）
        assert_eq!(names, vec!["src", "io", "main.rs", "Cargo.toml"]);
        assert_eq!(rows[1].depth, 1);
        // io 未展开 → 不含 mod.rs
        assert!(!names.contains(&"mod.rs"));
    }

    #[test]
    fn nested_expansion_and_deep_path() {
        let tree = FileTree::build(&s(&["a/b/c/d.rs", "a/x.rs"]));
        let mut expanded = HashSet::new();
        expanded.insert("a".to_string());
        expanded.insert("a/b".to_string());
        let rows = tree.rows(&expanded);
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["a", "a/b", "a/b/c", "a/x.rs"]);
        assert_eq!(rows[2].depth, 2);
    }

    #[test]
    fn counts_files() {
        let tree = FileTree::build(&s(&["a.rs", "b/c.rs", "b/d.rs"]));
        assert_eq!(tree.file_count, 3);
    }
}
