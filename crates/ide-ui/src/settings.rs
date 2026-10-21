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
            mouse_scroll_direction: "normal".to_string(),
        }
    }
}

impl AppSettings {
    /// 滚轮方向是否反转（`"reversed"` = 反向）。
    pub fn scroll_reversed(&self) -> bool {
        self.mouse_scroll_direction.trim().eq_ignore_ascii_case("reversed")
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
    pub fn load() -> (AppSettings, Option<SystemTime>) {
        let path = Self::config_path();
        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<AppSettings>(&raw) {
                Ok(s) => {
                    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                    (s, mtime)
                }
                Err(e) => {
                    tracing::warn!("settings.json 解析失败({e})，使用默认设置: {}", path.display());
                    (AppSettings::default(), None)
                }
            },
            Err(_) => (AppSettings::default(), None),
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
        assert!(mtime.is_none());
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
