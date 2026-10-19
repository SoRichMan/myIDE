#!/usr/bin/env bash
# LiteIDE WSL toolchain bootstrap (idempotent).
# Installs clang/clangd/cmake/ninja/gdb/ripgrep/git/bear + build essentials,
# then prints a self-check report. Safe to re-run.
set -u

TOOLS=(build-essential cmake ninja-build pkg-config clang clangd lldb gdb ripgrep git bear gcc g++)

echo "== LiteIDE WSL toolchain setup =="
if [ "$(id -u)" -eq 0 ]; then
    SUDO=""
else
    SUDO="sudo"
fi

export DEBIAN_FRONTEND=noninteractive
$SUDO apt-get update -qq || { echo "apt-get update failed"; exit 1; }

MISSING=()
for t in "${TOOLS[@]}"; do
    if ! dpkg -s "$t" >/dev/null 2>&1; then
        MISSING+=("$t")
    fi
done

if [ "${#MISSING[@]}" -gt 0 ]; then
    echo "Installing: ${MISSING[*]}"
    $SUDO apt-get install -y -qq "${MISSING[@]}" || { echo "install failed"; exit 1; }
else
    echo "All packages already installed."
fi

echo
echo "== Environment self-check =="
for t in clang clangd cmake ninja gcc g++ gdb git rg bear pkg-config; do
    if command -v "$t" >/dev/null 2>&1; then
        printf "%-12s %s\n" "$t" "$(command -v "$t")  $($t --version 2>/dev/null | head -1)"
    else
        printf "%-12s MISSING\n" "$t"
    fi
done
echo
echo "== Setup done =="
