//! 工具链缺失诊断（阶段 6 守护用例）。
//!
//! 把 WSL / SSH / clangd / gdb 探测失败映射成"明确提示 + 可直接照做的修复建议"。
//! 失败类型：
//! - WSL 未安装（`wsl -l -q` 为空）→ 指引 `wsl --install`
//! - WSL 里 clangd / gdb 未安装 → 指引运行 `scripts/setup-wsl.sh`
//! - SSH 主机上 clangd / gdb 未安装 → 指引在远端用包管理器安装
//!
//! 这些函数是纯函数，便于守护测试逐项断言提示与修复命令都在文案里。

use crate::remote::RemoteTarget;

/// WSL 完全不可用（`wsl.exe -l -q` 为空/失败）时的提示与修复建议。
pub fn wsl_missing() -> String {
    "未找到 WSL 发行版。修复：在管理员 PowerShell 运行 `wsl --install -d Ubuntu-24.04`，"
        .to_string()
        + "完成后重启电脑，再启动 LiteIDE。"
}

/// WSL 中某个工具（clangd / gdb / rg）不在 PATH 时的提示与修复建议。
///
/// `tool` 为工具名（用于展示），`detail` 为探测子进程返回的具体原因。
pub fn tool_missing(tool: &str, detail: &str) -> String {
    format!(
        "WSL 中未找到 {tool}（{detail}）。修复：在 WSL 终端运行 `bash scripts/setup-wsl.sh` \
         一键安装全部工具链（幂等，可重复执行）。"
    )
}

/// 按远程目标给出「工具缺失」提示：WSL 沿用 [`tool_missing`]，SSH 不提 WSL 和安装脚本。
///
/// SSH 主机由用户自己维护，`scripts/setup-wsl.sh` 在那边没有意义，文案里给的是
/// CentOS / Debian 系各自的安装命令。
pub fn tool_missing_on(target: &RemoteTarget, tool: &str, detail: &str) -> String {
    if target.is_wsl() {
        return tool_missing(tool, detail);
    }
    let pkg = match tool {
        "clangd" => "clang-tools-extra（Ubuntu：clangd）",
        "rg" => "ripgrep",
        _ => tool,
    };
    format!(
        "{} 上未找到 {tool}（{detail}）。修复：在远端安装 {pkg} \
         （CentOS/RHEL：`sudo yum install <包名>`；Debian/Ubuntu：`sudo apt install <包名>`），\
         装好后重新连接即可。",
        target.label()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_missing_mentions_install_command() {
        let msg = wsl_missing();
        assert!(msg.contains("wsl --install"), "应包含 wsl --install 命令: {msg}");
        assert!(msg.contains("Ubuntu-24.04"), "应指明发行版: {msg}");
        assert!(msg.contains("修复"), "应有修复指引: {msg}");
    }

    #[test]
    fn clangd_missing_mentions_setup_script() {
        let msg = tool_missing("clangd", "clangd 不在 WSL 的 PATH 中");
        assert!(msg.contains("clangd"), "应点出缺失工具: {msg}");
        assert!(msg.contains("setup-wsl.sh"), "应指向一键安装脚本: {msg}");
        assert!(msg.contains("修复"), "应有修复指引: {msg}");
    }

    #[test]
    fn gdb_missing_mentions_setup_script() {
        let msg = tool_missing("gdb", "gdb 不在 WSL 的 PATH 中");
        assert!(msg.contains("gdb"));
        assert!(msg.contains("setup-wsl.sh"));
        assert!(msg.contains("幂等"), "应说明可重复执行: {msg}");
    }

    #[test]
    fn tool_missing_propagates_detail() {
        let msg = tool_missing("gdb", "探测失败 (exit 127)");
        assert!(msg.contains("exit 127"), "应保留底层原因: {msg}");
    }

    /// SSH 目标不能套用 WSL 的文案（WSL 和 setup-wsl.sh 在远端没有意义）。
    #[test]
    fn ssh_tool_missing_does_not_blame_wsl() {
        let t = RemoteTarget::ssh("fubin", "192.168.77.6", 22);
        let msg = tool_missing_on(&t, "clangd", "clangd 不在 SSH 的 PATH 中");
        assert!(msg.contains("SSH: fubin@192.168.77.6"), "应点出主机: {msg}");
        assert!(msg.contains("clangd"), "应点出缺失工具: {msg}");
        assert!(!msg.contains("WSL"), "SSH 文案不应提 WSL: {msg}");
        assert!(!msg.contains("setup-wsl.sh"), "SSH 文案不应指向 WSL 脚本: {msg}");
        assert!(msg.contains("yum"), "应给出 CentOS 安装方式: {msg}");
        assert!(msg.contains("apt"), "应给出 Debian 安装方式: {msg}");
        assert!(msg.contains("clangd 不在 SSH 的 PATH 中"), "应保留底层原因: {msg}");
    }

    /// WSL 目标保持原有文案（守护用例不能让老行为漂移）。
    #[test]
    fn tool_missing_on_wsl_keeps_setup_script() {
        let msg = tool_missing_on(&RemoteTarget::wsl("Ubuntu2404"), "gdb", "missing");
        assert!(msg.contains("WSL 中未找到 gdb"), "{msg}");
        assert!(msg.contains("setup-wsl.sh"), "{msg}");
        assert!(!msg.contains("yum"), "WSL 不应给远端包管理器建议: {msg}");
    }
}
