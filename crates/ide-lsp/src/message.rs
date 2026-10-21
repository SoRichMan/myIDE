//! JSON-RPC 2.0 信封 + 最小 LSP 类型与响应解析。
//!
//! 请求侧：用 `serde_json::json!` 手工构造（字段完全可控）。
//! 响应侧：clangd 的返回形态多样（单值/数组/嵌套），用 `serde_json::Value`
//! 提取，宽容处理 `Location | Location[] | LocationLink[] | null`、
//! `MarkupContent | MarkedString | MarkedString[] | string`、
//! `DocumentSymbol[] | SymbolInformation[]` 等变体。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 版本号。
pub const JSONRPC: &str = "2.0";

// ---------------------------------------------------------------------------
// 基础 LSP 位置类型
// ---------------------------------------------------------------------------

/// 文档内位置（0-based，LSP 规范）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// 文档内区间（半开区间：start 含，end 不含）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// 一个文件位置（URI + 区间）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

/// hover 结果（仅提取客户端需要的部分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverInfo {
    /// 拼接后的纯文本/markdown 内容。
    pub markdown: String,
    pub range: Option<Range>,
}

/// 文档符号（大纲用）。`children` 为嵌套子符号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolInfo {
    pub name: String,
    /// LSP `SymbolKind` 数值（1=File, 2=Module, 3=Namespace, 4=Package,
    /// 5=Class, 6=Method, 7=Property, 8=Field, 9=Constructor, 10=Enum,
    /// 11=Interface, 12=Function, 13=Variable, ...）。
    pub kind: u32,
    pub detail: Option<String>,
    /// 符号整体区间。
    pub range: Range,
    /// 符号名所在区间（点击跳转用）。
    pub selection_range: Range,
    pub children: Vec<SymbolInfo>,
}

/// 工作区符号（`workspace/symbol` 的结果项）。
///
/// 语义索引在服务端（clangd）里，`textDocument/definition` 找不到时
/// 可以退到这里按名字找定义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSymbol {
    pub name: String,
    /// LSP `SymbolKind` 数值（含义同 [`SymbolInfo::kind`]）。
    pub kind: u32,
    /// 所属容器（类/命名空间），clangd 会给。
    pub container: Option<String>,
    /// 符号所在位置。
    pub location: Location,
}

// ---------------------------------------------------------------------------
// Value → 类型 解析（宽容模式）
// ---------------------------------------------------------------------------

pub fn parse_position(v: &Value) -> Option<Position> {
    Some(Position {
        line: v.get("line")?.as_u64()? as u32,
        character: v.get("character")?.as_u64()? as u32,
    })
}

pub fn parse_range(v: &Value) -> Option<Range> {
    Some(Range {
        start: parse_position(v.get("start")?)?,
        end: parse_position(v.get("end")?)?,
    })
}

/// 解析单个 `Location` 或 `LocationLink`。
fn parse_one_location(v: &Value) -> Option<Location> {
    // LocationLink: { targetUri, targetRange, ... }
    if let Some(uri) = v.get("targetUri").and_then(|u| u.as_str()) {
        return Some(Location {
            uri: uri.to_string(),
            range: parse_range(v.get("targetRange")?)?,
        });
    }
    // Location: { uri, range }
    let uri = v.get("uri")?.as_str()?.to_string();
    let range = parse_range(v.get("range")?)?;
    Some(Location { uri, range })
}

/// 解析定义/声明/类型定义/实现响应：
/// `null | Location | Location[] | LocationLink[]` → 统一成 `Vec<Location>`。
pub fn parse_locations(result: &Value) -> Vec<Location> {
    match result {
        Value::Null => Vec::new(),
        Value::Array(items) => items.iter().filter_map(parse_one_location).collect(),
        other => parse_one_location(other).into_iter().collect(),
    }
}

/// 解析 hover 响应：`null | Hover`。
///
/// `contents` 可以是：
/// - 字符串
/// - `{ language, value }`（MarkedString）
/// - `[{ language, value }, ...]`
/// - `{ kind: "markdown"|"plaintext", value }`（MarkupContent）
pub fn parse_hover(result: &Value) -> Option<HoverInfo> {
    if result.is_null() {
        return None;
    }
    let contents = result.get("contents")?;
    if contents.is_null() {
        return None;
    }
    let markdown = hover_contents_to_text(contents);
    let range = result.get("range").and_then(parse_range);
    Some(HoverInfo { markdown, range })
}

fn hover_contents_to_text(contents: &Value) -> String {
    match contents {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|it| match it {
                Value::String(s) => s.clone(),
                other => other
                    .get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        other => other
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

/// 解析 documentSymbol 响应：
/// `DocumentSymbol[]`（嵌套，含 range/selectionRange）或
/// `SymbolInformation[]`（扁平，含 location）。统一成嵌套的 `SymbolInfo`。
pub fn parse_symbols(result: &Value) -> Vec<SymbolInfo> {
    let Value::Array(items) = result else {
        return Vec::new();
    };
    items.iter().filter_map(parse_one_symbol).collect()
}

fn parse_one_symbol(v: &Value) -> Option<SymbolInfo> {
    let name = v.get("name")?.as_str()?.to_string();
    let kind = v.get("kind")?.as_u64().unwrap_or(0) as u32;
    let detail = v.get("detail").and_then(|d| d.as_str()).map(String::from);

    if let Some(range) = v.get("range").and_then(parse_range) {
        // DocumentSymbol（嵌套）
        let selection_range = v
            .get("selectionRange")
            .and_then(parse_range)
            .unwrap_or(range);
        let children = v
            .get("children")
            .and_then(|c| c.as_array())
            .map(|arr| arr.iter().filter_map(parse_one_symbol).collect())
            .unwrap_or_default();
        Some(SymbolInfo {
            name,
            kind,
            detail,
            range,
            selection_range,
            children,
        })
    } else if let Some(loc) = v.get("location").and_then(parse_one_location) {
        // SymbolInformation（扁平）
        Some(SymbolInfo {
            name,
            kind,
            detail,
            range: loc.range,
            selection_range: loc.range,
            children: Vec::new(),
        })
    } else {
        None
    }
}

/// 解析 `workspace/symbol` 响应：`SymbolInformation[]`。
///
/// 每项形如 `{ name, kind, containerName?, location: { uri, range } }`。
pub fn parse_workspace_symbols(result: &Value) -> Vec<WorkspaceSymbol> {
    let Value::Array(items) = result else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|v| {
            let name = v.get("name")?.as_str()?.to_string();
            let kind = v.get("kind").and_then(|k| k.as_u64()).unwrap_or(0) as u32;
            let container = v
                .get("containerName")
                .and_then(|c| c.as_str())
                .map(String::from);
            let location = parse_one_location(v.get("location")?)?;
            Some(WorkspaceSymbol {
                name,
                kind,
                container,
                location,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_definition_single_location() {
        let v = json!({"uri": "file:///a.cpp", "range": {"start": {"line": 3, "character": 5}, "end": {"line": 3, "character": 8}}});
        let locs = parse_locations(&v);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].uri, "file:///a.cpp");
        assert_eq!(locs[0].range.start.line, 3);
        assert_eq!(locs[0].range.start.character, 5);
    }

    #[test]
    fn parse_definition_location_array() {
        let v = json!([
            {"uri": "file:///a.cpp", "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 1}}},
            {"uri": "file:///b.cpp", "range": {"start": {"line": 9, "character": 0}, "end": {"line": 9, "character": 1}}}
        ]);
        let locs = parse_locations(&v);
        assert_eq!(locs.len(), 2);
        assert_eq!(locs[1].range.start.line, 9);
    }

    #[test]
    fn parse_definition_location_links() {
        let v = json!([
            {"targetUri": "file:///x.cpp", "targetRange": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 4}}, "targetSelectionRange": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 4}}}
        ]);
        let locs = parse_locations(&v);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].uri, "file:///x.cpp");
        assert_eq!(locs[0].range.start.line, 2);
    }

    #[test]
    fn parse_definition_null_and_empty() {
        assert!(parse_locations(&Value::Null).is_empty());
        assert!(parse_locations(&json!([])).is_empty());
    }

    #[test]
    fn parse_workspace_symbols_flat_list() {
        let v = json!([
            {
                "name": "Foo::bar",
                "kind": 6,
                "containerName": "Foo",
                "location": {
                    "uri": "file:///a.cpp",
                    "range": {"start": {"line": 12, "character": 4}, "end": {"line": 12, "character": 7}}
                }
            },
            {
                "name": "kMax",
                "kind": 14,
                "location": {
                    "uri": "file:///b.h",
                    "range": {"start": {"line": 3, "character": 0}, "end": {"line": 3, "character": 5}}
                }
            }
        ]);
        let syms = parse_workspace_symbols(&v);
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name, "Foo::bar");
        assert_eq!(syms[0].kind, 6);
        assert_eq!(syms[0].container.as_deref(), Some("Foo"));
        assert_eq!(syms[0].location.uri, "file:///a.cpp");
        assert_eq!(syms[0].location.range.start.line, 12);
        // 没有 containerName 也能解析。
        assert_eq!(syms[1].container, None);
        assert_eq!(syms[1].location.range.start.character, 0);
    }

    #[test]
    fn parse_workspace_symbols_ignores_garbage() {
        assert!(parse_workspace_symbols(&Value::Null).is_empty());
        assert!(parse_workspace_symbols(&json!([])).is_empty());
        // 缺 location 的项要跳过，不能 panic。
        let v = json!([{"name": "x", "kind": 12}]);
        assert!(parse_workspace_symbols(&v).is_empty());
    }

    #[test]
    fn parse_hover_markup_content() {
        let v = json!({"contents": {"kind": "markdown", "value": "```cpp\nint add(int a, int b)\n```\n\nAdds two integers."}, "range": null});
        let h = parse_hover(&v).unwrap();
        assert!(h.markdown.contains("int add(int a, int b)"));
        assert!(h.markdown.contains("Adds two integers."));
    }

    #[test]
    fn parse_hover_plain_string_and_marked_string_array() {
        let s = parse_hover(&json!({"contents": "just text"})).unwrap();
        assert_eq!(s.markdown, "just text");
        let arr = parse_hover(&json!({"contents": [{"language": "cpp", "value": "int f();"}, {"language": "cpp", "value": "doc line"}]}))
            .unwrap();
        assert!(arr.markdown.contains("int f();"));
        assert!(arr.markdown.contains("doc line"));
    }

    #[test]
    fn parse_hover_null() {
        assert!(parse_hover(&Value::Null).is_none());
        // clangd 可能返回 {contents: null}
        assert!(parse_hover(&json!({"contents": null})).is_none());
    }

    #[test]
    fn parse_symbols_document_symbol_nested() {
        let v = json!([
            {"name": "main", "kind": 12, "range": {"start": {"line": 4, "character": 0}, "end": {"line": 8, "character": 1}}, "selectionRange": {"start": {"line": 4, "character": 4}, "end": {"line": 4, "character": 8}}, "children": [
                {"name": "local", "kind": 13, "range": {"start": {"line": 5, "character": 8}, "end": {"line": 5, "character": 16}}, "selectionRange": {"start": {"line": 5, "character": 8}, "end": {"line": 5, "character": 13}}}
            ]}
        ]);
        let syms = parse_symbols(&v);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "main");
        assert_eq!(syms[0].kind, 12);
        assert_eq!(syms[0].selection_range.start.character, 4);
        assert_eq!(syms[0].children.len(), 1);
        assert_eq!(syms[0].children[0].name, "local");
    }

    #[test]
    fn parse_symbols_symbol_information_flat() {
        let v = json!([
            {"name": "add", "kind": 12, "location": {"uri": "file:///a.cpp", "range": {"start": {"line": 2, "character": 0}, "end": {"line": 4, "character": 1}}}}
        ]);
        let syms = parse_symbols(&v);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].range.start.line, 2);
    }

    #[test]
    fn position_serializes_to_lsp_shape() {
        let p = Position { line: 3, character: 7 };
        let v = serde_json::to_value(p).unwrap();
        assert_eq!(v, json!({"line": 3, "character": 7}));
    }
}
