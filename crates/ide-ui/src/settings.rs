//! 应用设置：JSON 配置文件 + 热重载（阶段 5）。
//!
//! 配置文件位置：`%APPDATA%\LiteIDE\settings.json`（`LITEIDE_CONFIG_DIR` 可覆盖目录，测试用）。
//! 热重载：每帧 stat 一次 mtime，变化才重新读取（本地小文件，微秒级开销）。
//! 遵循红线：非测试代码不使用 unwrap/expect。
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;

/// 设置项（可序列化，未知字段忽略，缺失字段回退默认）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AppSettings {
    /// 编辑器字号（10..=24）。
    pub font_size: f32,
    /// 字体族："Default" | "Consolas" | "Cascadia Code"。
    pub font_family: String,
    /// 主题：`ThemeKind::ALL` 之一的名字（"C/C++ Dark" | "Dark+" | "Light+"）。
    pub theme: String,
    /// 显示侧边栏。
    pub show_sidebar: bool,
    /// 显示 minimap。
    pub show_minimap: bool,
    /// 显示面包屑。
    pub show_breadcrumbs: bool,
    /// 远程连接的 WSL 发行版（`None` = 本地工作区）。重启后恢复上次的连接。
    pub remote_distro: Option<String>,
    /// 远程工作区的 POSIX 路径（如 `/home/work/proj`）；与 `remote_distro` 成对出现。
    pub remote_path: Option<String>,
    /// 远程连接种类：`"wsl"` / `"ssh"`。
    ///
    /// 旧配置文件里没有这个字段（且只有 `remote_distro`），按 WSL 处理。
    pub remote_kind: Option<String>,
    /// SSH 主机名 / IP（`remote_kind == "ssh"` 时使用）。
    pub ssh_host: Option<String>,
    /// SSH 用户名（免密登录用的那个用户）。
    pub ssh_user: Option<String>,
    /// SSH 端口（0 表示未设置，按 22 处理）。
    pub ssh_port: u16,
    /// 鼠标滚轮方向：
    /// - `"normal"`：滚轮向下 = 看到更后面的内容（VS Code 行为）；
    /// - `"reversed"`：与上面相反。
    ///
    /// 改完立刻生效（设置是热重载的）。
    pub mouse_scroll_direction: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            font_family: "Default".to_string(),
            theme: "C/C++ Dark".to_string(),
            show_sidebar: true,
            show_minimap: true,
            show_breadcrumbs: true,
            remote_distro: None,
            remote_path: None,
            remote_kind: None,
            ssh_host: None,
            ssh_user: None,
            ssh_port: ide_core::remote::DEFAULT_SSH_PORT,
            mouse_scroll_direction: "normal".to_string(),
        }
    }
}

impl AppSettings {
    /// 滚轮方向是否反转（`"reversed"` = 反向）。
    pub fn scroll_reversed(&self) -> bool {
        self.mouse_scroll_direction.trim().eq_ignore_ascii_case("reversed")
    }

    /// 当前应该使用的远程目标（`None` = 本地工作区）。
    ///
    /// - `remote_kind == "ssh"`：需要用户名 + 主机（端口 0 按 22 算）；
    /// - 其余情况（含旧配置里只有 `remote_distro` 的）：按 WSL 处理，需要发行版名。
    ///
    /// `remote_path` 必须存在：只选主机不选目录不算连上工作区。
    pub fn remote_target(&self) -> Option<ide_core::remote::RemoteTarget> {
        let path = self.remote_path.as_deref()?.trim();
        if path.is_empty() {
            return None;
        }
        let kind = self
            .remote_kind
            .as_deref()
            .map(|k| k.trim().to_ascii_lowercase());
        if kind.as_deref() == Some("ssh") {
            let user = self.ssh_user.as_deref()?.trim();
            let host = self.ssh_host.as_deref()?.trim();
            if user.is_empty() || host.is_empty() {
                return None;
            }
            return Some(ide_core::remote::RemoteTarget::ssh(
                user,
                host,
                self.ssh_port,
            ));
        }
        let distro = self.remote_distro.as_deref()?.trim();
        if distro.is_empty() {
            return None;
        }
        Some(ide_core::remote::RemoteTarget::wsl(distro))
    }

    /// 状态栏/菜单里的远程标签（未连接为 `None`）。
    pub fn remote_label(&self) -> Option<String> {
        self.remote_target().map(|t| t.label())
    }

    /// 配置文件绝对路径。
    pub fn config_path() -> PathBuf {
        if let Ok(dir) = std::env::var("LITEIDE_CONFIG_DIR") {
            let mut p = PathBuf::from(dir);
            p.push("settings.json");
            return p;
        }
        let base = std::env::var("APPDATA").unwrap_or_default();
        let mut p = PathBuf::from(base);
        p.push("LiteIDE");
        p.push("settings.json");
        p
    }

    /// 读取配置：文件不存在/损坏时返回默认值并附警告信息（不 panic）。
    ///
    /// 解析失败也返回文件的 mtime：热重载靠 mtime 判断「外部改过没有」，
    /// 返回 `None` 会让损坏的配置文件被当成「每帧都变」，从而每帧重新加载。
    pub fn load() -> (AppSettings, Option<SystemTime>) {
        let path = Self::config_path();
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<AppSettings>(&raw) {
                Ok(s) => (s, mtime),
                Err(e) => {
                    tracing::warn!("settings.json 解析失败({e})，使用默认设置: {}", path.display());
                    (AppSettings::default(), mtime)
                }
            },
            Err(_) => (AppSettings::default(), mtime),
        }
    }

    /// 写回配置文件（原子性：先写临时文件再改名，避免半截 JSON）。
    pub fn save(&self) -> Result<(), String> {
        let path = Self::config_path();
        if let Some(dir) = path.parent() {
            if !dir.is_dir() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("创建配置目录失败: {e}"))?;
            }
        }
        let raw = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, raw).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        tracing::info!("settings saved: {}", path.display());
        Ok(())
    }

    /// 热重载：配置 mtime 变化时重新读取并应用。
    /// 返回 `true` 表示设置已更新（调用方需据此刷新字体/主题/编辑器）。
    ///
    /// 契约：只要磁盘 mtime 与已知值不同（含首次已知为 None），即视为外部修改，
    /// 重新加载并返回 true。这样「刚保存完再热重载」也能被感知（mtime 指针未更新）。
    pub fn hot_reload(&mut self, last_mtime: &mut Option<SystemTime>) -> bool {
        let path = Self::config_path();
        let now = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if now != *last_mtime {
            let (s, m) = Self::load();
            *last_mtime = m;
            *self = s;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ide_core::theme::ThemeKind;
    use std::sync::Mutex;
    use std::time::Duration;

    /// 序列化所有会改 `LITEIDE_CONFIG_DIR` 的测试（env 是进程级，并行会互相污染）。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// 每个测试独立的临时配置目录。
    fn temp_config_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("liteide_cfg_test_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_are_sane() {
        assert_eq!(AppSettings::default().font_size, 14.0);
        // 默认主题 = 用户 VS Code 用的 "Dark (Visual Studio - C/C++)"
        assert_eq!(AppSettings::default().theme, "C/C++ Dark");
        assert_eq!(
            ThemeKind::from_name(&AppSettings::default().theme),
            ThemeKind::CppDark
        );
    }

    #[test]
    fn remote_target_ssh_and_legacy_wsl() {
        use ide_core::remote::RemoteTarget;

        let mut s = AppSettings::default();
        assert!(s.remote_target().is_none(), "默认是本地工作区");

        // 旧配置：只有发行版 + 路径 ⇒ WSL。
        s.remote_distro = Some("Ubuntu2404".to_string());
        assert!(s.remote_target().is_none(), "没有路径不算连接");
        s.remote_path = Some("/home/fubin/work/candanza".to_string());
        assert_eq!(
            s.remote_target(),
            Some(RemoteTarget::wsl("Ubuntu2404")),
            "legacy 配置按 WSL 处理"
        );
        assert_eq!(s.remote_label().as_deref(), Some("WSL: Ubuntu2404"));

        // SSH：需要用户名 + 主机；端口 0 按 22 算。
        s.remote_kind = Some("ssh".to_string());
        s.ssh_host = Some("192.168.77.6".to_string());
        assert!(s.remote_target().is_none(), "缺用户名不算连接");
        s.ssh_user = Some("fubin".to_string());
        s.ssh_port = 0;
        assert_eq!(
            s.remote_target(),
            Some(RemoteTarget::ssh("fubin", "192.168.77.6", 22))
        );
        assert_eq!(s.remote_label().as_deref(), Some("SSH: fubin@192.168.77.6"));

        // kind 大小写/空格不敏感；端口原样保留。
        s.remote_kind = Some(" SSH ".to_string());
        s.ssh_port = 2222;
        assert_eq!(
            s.remote_target(),
            Some(RemoteTarget::ssh("fubin", "192.168.77.6", 2222))
        );

        // 显式回到 WSL：即使 ssh_* 还留着也不影响。
        s.remote_kind = Some("wsl".to_string());
        assert_eq!(
            s.remote_target(),
            Some(RemoteTarget::wsl("Ubuntu2404"))
        );
    }

    #[test]
    fn remote_kind_survives_json_roundtrip() {
        let s = AppSettings {
            remote_kind: Some("ssh".to_string()),
            ssh_host: Some("h".to_string()),
            ssh_user: Some("u".to_string()),
            ssh_port: 2200,
            remote_path: Some("/home/u/p".to_string()),
            ..AppSettings::default()
        };
        let raw = serde_json::to_string(&s).unwrap();
        let back: AppSettings = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, s);
        // 旧 JSON（无 remote_kind）仍能解析成 WSL 连接。
        let legacy: AppSettings = serde_json::from_str(
            r#"{"remote_distro":"Debian","remote_path":"/home/x"}"#,
        )
        .unwrap();
        assert_eq!(
            legacy.remote_target(),
            Some(ide_core::remote::RemoteTarget::wsl("Debian"))
        );
    }


    #[test]
    fn scroll_direction_defaults_to_normal_and_can_be_inverted() {
        let mut s = AppSettings::default();
        assert!(!s.scroll_reversed(), "默认应与 VS Code 一致");
        s.mouse_scroll_direction = "reversed".to_string();
        assert!(s.scroll_reversed());
        // 大小写/空格不敏感。
        s.mouse_scroll_direction = " Reversed ".to_string();
        assert!(s.scroll_reversed());
        s.mouse_scroll_direction = "normal".to_string();
        assert!(!s.scroll_reversed());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_config_dir("roundtrip");
        std::env::set_var("LITEIDE_CONFIG_DIR", &dir);
        let s = AppSettings {
            font_size: 17.0,
            font_family: "Cascadia Code".to_string(),
            theme: "Light+".to_string(),
            show_minimap: false,
            remote_distro: Some("Ubuntu2404".to_string()),
            remote_path: Some("/home/work/NewLithoTune/candanza".to_string()),
            ..Default::default()
        };
        s.save().unwrap();
        let (loaded, mtime) = AppSettings::load();
        assert_eq!(loaded, s);
        assert!(mtime.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_falls_back_to_default() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_config_dir("corrupt");
        std::env::set_var("LITEIDE_CONFIG_DIR", &dir);
        let path = AppSettings::config_path();
        std::fs::write(&path, "{ not json !!!").unwrap();
        let (s, mtime) = AppSettings::load();
        assert_eq!(s, AppSettings::default());
        // 记住 mtime 才不会让热重载每帧重试同一个坏文件。
        assert!(mtime.is_some(), "损坏文件也应记住 mtime");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_returns_defaults() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_config_dir("missing");
        std::env::set_var("LITEIDE_CONFIG_DIR", &dir);
        let (s, mtime) = AppSettings::load();
        assert_eq!(s, AppSettings::default());
        assert!(mtime.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_fields_ignored_and_partial_defaults() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_config_dir("partial");
        std::env::set_var("LITEIDE_CONFIG_DIR", &dir);
        let path = AppSettings::config_path();
        std::fs::write(&path, r#"{"font_size": 16, "future_field": 1}"#).unwrap();
        let (s, _) = AppSettings::load();
        assert_eq!(s.font_size, 16.0);
        assert_eq!(s.theme, "C/C++ Dark"); // 缺失字段回退默认
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 损坏的 JSON 不能导致「每帧重新加载」：mtime 必须被记住。
    #[test]
    fn corrupt_settings_reloads_only_when_file_changes() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_config_dir("corrupt-hot");
        std::env::set_var("LITEIDE_CONFIG_DIR", &dir);
        let path = AppSettings::config_path();
        std::fs::write(&path, "{ this is not json").unwrap();
        let (mut s, mtime) = AppSettings::load();
        assert_eq!(s, AppSettings::default());
        assert!(mtime.is_some(), "损坏文件也要记住 mtime");
        let mut guard = mtime;
        assert!(!s.hot_reload(&mut guard), "内容没变时不应重复热重载");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hot_reload_detects_external_change() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = temp_config_dir("hot");
        std::env::set_var("LITEIDE_CONFIG_DIR", &dir);
        let mut s = AppSettings::default();
        let mut mtime = None;
        assert!(!s.hot_reload(&mut mtime)); // 首次：无文件，不视为变更

        s.save().unwrap();
        // mtime 指针未更新 → 检测到变更
        assert!(s.hot_reload(&mut mtime));
        assert!(mtime.is_some());
        // 再次调用：无变更
        assert!(!s.hot_reload(&mut mtime));

        // 外部修改文件
        std::thread::sleep(Duration::from_millis(20));
        let s2 = AppSettings {
            font_size: 20.0,
            ..Default::default()
        };
        s2.save().unwrap();
        assert!(s.hot_reload(&mut mtime));
        assert_eq!(s.font_size, 20.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
