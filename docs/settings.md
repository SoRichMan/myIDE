# LiteIDE 配置说明

## 配置文件位置

```
%APPDATA%\LiteIDE\settings.json
```

在 PowerShell 里打开：

```powershell
explorer "$env:APPDATA\LiteIDE"
```

按 **Ctrl+,** 在应用内打开设置面板修改；手动改 JSON 后**不用重启**，IDE 每帧检测文件
变化并热重载（字体/主题/字号/面板开关独立生效）。

## 可配置项

| 字段 | 类型 | 默认 | 取值 | 说明 |
|---|---|---|---|---|
| `font_size` | number | `14` | 10 – 24 | 编辑器字号（行高 = 字号 × 1.5） |
| `font_family` | string | `"Default"` | `"Default"` / `"Consolas"` / `"Cascadia Code"` | 字体族 |
| `theme` | string | `"Dark+"` | `"Dark+"` / `"Light+"` | 配色主题 |
| `show_sidebar` | bool | `true` | true/false | 显示左侧边栏 |
| `show_minimap` | bool | `true` | true/false | 显示右侧 minimap |
| `show_breadcrumbs` | bool | `true` | true/false | 显示面包屑路径 |

## 示例 settings.json

```json
{
  "font_size": 14.0,
  "font_family": "Consolas",
  "theme": "Dark+",
  "show_sidebar": true,
  "show_minimap": true,
  "show_breadcrumbs": true
}
```

## 容错规则

- 字段缺失 → 用默认值补齐。
- 出现未知字段 → 忽略，不报错（方便后续版本新增配置）。
- JSON 损坏 / 非法数字（如字号 < 10 或 > 24）→ 回退默认，并在日志里告警，不崩溃。
- 想全部回到默认：命令面板执行 **Preferences: Reset Settings**。
