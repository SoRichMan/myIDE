#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""修复 rustup 工具链缺失的组件 manifest（只重建记账文件，不动二进制）。

背景：10-15 一次失败的 channel 同步把 stable 工具链的
lib/rustlib/manifest-{rustc,rust-std,clippy-preview,rustfmt-preview}-x86_64-pc-windows-msvc
删掉了，导致 rustup 认为工具链损坏（rustc.exe 等二进制其实完好，1.99.0）。
本脚本按现有文件树重建这 4 个 manifest，格式与 manifest-cargo 一致（file: 前缀）。
"""
import os
import sys

TC = os.path.expandvars(
    r"%USERPROFILE%\.rustup\toolchains\stable-x86_64-pc-windows-msvc"
)

# rustup 自己的记账文件，不属于任何组件 manifest
RUSTUP_BOOKKEEPING = {
    "lib/rustlib/components",
    "lib/rustlib/manifest-cargo-x86_64-pc-windows-msvc",
    "lib/rustlib/multirust-channel-manifest.toml",
    "lib/rustlib/rust-installer-version",
}

# cargo 组件已存在 manifest（manifest-cargo-*），排除其已登记文件
CARGO_MANIFEST = os.path.join(TC, "lib/rustlib", "manifest-cargo-x86_64-pc-windows-msvc")
CARGO_FILES = set()
with open(CARGO_MANIFEST, "r", encoding="ascii") as f:
    for line in f:
        line = line.strip()
        if line.startswith("file:"):
            CARGO_FILES.add(line[len("file:"):].replace("\\", "/"))

def all_files(root):
    out = []
    for dirpath, dirnames, filenames in os.walk(root):
        # 跳过工具链根部的 lib/rustlib 记账区（单独处理）
        rel = os.path.relpath(dirpath, root).replace("\\", "/")
        for fn in filenames:
            out.append((rel + "/" + fn) if rel != "." else fn)
    return out

def relpaths(prefix):
    """某前缀下的全部文件（相对工具链根）。"""
    base = os.path.join(TC, *prefix.split("/"))
    out = []
    if os.path.isdir(base):
        for dirpath, _, filenames in os.walk(base):
            rel = os.path.relpath(dirpath, TC).replace("\\", "/")
            for fn in filenames:
                out.append((rel + "/" + fn) if rel != "." else fn)
    return out

def write_manifest(name, files):
    path = os.path.join(TC, "lib/rustlib", name)
    files = sorted(set(files))
    # 全部文件必须真实存在（防 rustup 校验失败）
    missing = [f for f in files if not os.path.exists(os.path.join(TC, f.replace("/", "\\")))]
    if missing:
        print(f"[!] {name}: {len(missing)} 个文件不存在: {missing[:5]}", file=sys.stderr)
    with open(path, "w", encoding="ascii", newline="\n") as f:
        f.write("".join(f"file:{p}\n" for p in files))
    print(f"[+] {name}: {len(files)} files (missing={len(missing)})")

rust_std = relpaths("lib/rustlib/x86_64-pc-windows-msvc/lib")
clippy = relpaths("clippy-preview") + [f for f in all_files(TC)
                                       if f.startswith("bin/cargo-clippy")
                                       or f.startswith("bin/clippy-driver")
                                       or f.startswith("share/doc/clippy")]
rustfmt = relpaths("rustfmt-preview") + [f for f in all_files(TC)
                                         if f.startswith("bin/cargo-fmt")
                                         or f.startswith("bin/rustfmt")
                                         or f.startswith("share/doc/rustfmt")]

# rustc = 其余全部文件（排除 cargo/clippy/rustfmt/rust-std/记账区）
covered = set(CARGO_FILES) | set(rust_std) | set(clippy) | set(rustfmt) | RUSTUP_BOOKKEEPING
rustc = [f for f in all_files(TC) if f not in covered]

write_manifest("manifest-rustc-x86_64-pc-windows-msvc", rustc)
write_manifest("manifest-rust-std-x86_64-pc-windows-msvc", rust_std)
write_manifest("manifest-clippy-preview-x86_64-pc-windows-msvc", clippy)
write_manifest("manifest-rustfmt-preview-x86_64-pc-windows-msvc", rustfmt)

# 完整性核对：工具链全部文件必须被某个 manifest 覆盖（除记账区）
total = set(all_files(TC))
uncovered = total - covered - set(rustc)  # rustc 已补全覆盖
print(f"[i] total files={len(total)} covered={len(total) - len(uncovered)} uncovered={len(uncovered)}")
if uncovered:
    print("[!] 未覆盖:", sorted(uncovered)[:10], file=sys.stderr)
