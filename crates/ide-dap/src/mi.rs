//! GDB MI（Machine Interface）协议输出解析层（阶段 3）。
//!
//! 解析 gdb `--interpreter=mi2` 的逐行输出。MI 输出由四类记录组成：
//!
//! - **结果记录**（result record）：`[token]^class,results`，如 `^done,bkpt={...}`、`^error,msg="..."`、`^running`
//! - **流记录**（stream record）：`*class,results`，如 `*stopped,reason="breakpoint-hit",frame={...}`
//! - **异步通知**（async/notify record）：`=class,results`，如 `=breakpoint-created,bkpt={...}`
//! - **控制台/目标/日志输出**：`~"..."` / `@"..."` / `&"..."`，以及裸 `(gdb) ` 提示符
//!
//! 值语法（result 的右值）：
//! - 字符串 `"..."`（带 C 风格转义 `\n \t \" \\ \xNN \NNN`）
//! - 元组 `{name=value,...}`
//! - 列表 `[v1,v2]`，元素可为 `name=value`（如 `child={...}`）
//! - 裸常量（数字/标识符）

/// MI 值（result 记录的右值）。
#[derive(Debug, Clone, PartialEq)]
pub enum MiValue {
    /// 裸常量：数字、标识符等。
    Const(String),
    /// 解码后的字符串（`"..."`）。
    String(String),
    /// 元组 `{name=value, ...}`。
    Tuple(Vec<(String, MiValue)>),
    /// 列表 `[a, b, c]`；`name=value` 形式的元素记为单元素 Tuple。
    List(Vec<MiValue>),
}

impl MiValue {
    /// 按名字取元组字段。
    pub fn get(&self, key: &str) -> Option<&MiValue> {
        match self {
            MiValue::Tuple(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// 字符串化取值（Const / String 均返回文本）。
    pub fn as_str(&self) -> Option<&str> {
        match self {
            MiValue::Const(s) | MiValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// 元组视图。
    pub fn as_tuple(&self) -> Option<&[(String, MiValue)]> {
        match self {
            MiValue::Tuple(fields) => Some(fields),
            _ => None,
        }
    }

    /// 列表视图。
    pub fn as_list(&self) -> Option<&[MiValue]> {
        match self {
            MiValue::List(items) => Some(items),
            _ => None,
        }
    }

    /// 人类可读展示（变量面板用）。
    pub fn display(&self) -> String {
        match self {
            MiValue::Const(s) | MiValue::String(s) => s.clone(),
            MiValue::Tuple(fields) => {
                let inner: Vec<String> = fields
                    .iter()
                    .map(|(k, v)| format!("{k} = {}", v.display()))
                    .collect();
                format!("{{{}}}", inner.join(", "))
            }
            MiValue::List(items) => {
                let inner: Vec<String> = items.iter().map(|v| v.display()).collect();
                format!("[{}]", inner.join(", "))
            }
        }
    }
}

/// 单条 MI 记录。
#[derive(Debug, Clone, PartialEq)]
pub enum MiRecord {
    /// 流记录 `*stopped,...`。
    Stream { class: String, results: Vec<(String, MiValue)> },
    /// 结果记录 `[token]^done,...`。
    Result { token: Option<u64>, class: String, results: Vec<(String, MiValue)> },
    /// 异步通知 `=breakpoint-created,...`。
    Async { class: String, results: Vec<(String, MiValue)> },
    /// 控制台输出 `~"..."`。
    Console(String),
    /// 目标输出 `@"..."`。
    Target(String),
    /// 日志输出 `&"..."`。
    Log(String),
    /// `(gdb) ` 提示符。
    Prompt,
    /// 无法归类的原始输出行。
    Output(String),
}

/// 解析一行 MI 输出为记录。无法识别的内容按 `Output` 处理（不失败）。
pub fn parse_record(line: &str) -> MiRecord {
    let s = line.trim();
    if s.is_empty() {
        return MiRecord::Output(String::new());
    }
    if s == "(gdb) " || s == "(gdb)" || s.starts_with("(gdb) ") {
        return MiRecord::Prompt;
    }

    let bytes = s.as_bytes();
    // 数字 token 前缀（仅结果记录）
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    let token: Option<u64> = if i > 0 {
        s[..i].parse().ok()
    } else {
        None
    };
    let rest = &s[i..];
    let Some(first) = rest.chars().next() else {
        return MiRecord::Output(s.to_string());
    };

    let results = |r: &str| -> Vec<(String, MiValue)> {
        if r.is_empty() {
            Vec::new()
        } else {
            parse_results(r).unwrap_or_default()
        }
    };

    match first {
        '^' => MiRecord::Result {
            token,
            class: class_of(rest),
            results: results(results_of(rest)),
        },
        '*' => MiRecord::Stream {
            class: class_of(rest),
            results: results(results_of(rest)),
        },
        '+' => MiRecord::Stream {
            class: class_of(rest),
            results: results(results_of(rest)),
        },
        '=' => MiRecord::Async {
            class: class_of(rest),
            results: results(results_of(rest)),
        },
        '~' => MiRecord::Console(decode_c_string(&rest[1..]).unwrap_or_default()),
        '@' => MiRecord::Target(decode_c_string(&rest[1..]).unwrap_or_default()),
        '&' => MiRecord::Log(decode_c_string(&rest[1..]).unwrap_or_default()),
        _ => {
            if token.is_some() {
                // 数字开头但不是结果记录（gdb 偶发）→ 原样输出
                MiRecord::Output(s.to_string())
            } else {
                MiRecord::Output(s.to_string())
            }
        }
    }
}

/// 取记录类名：`^done` → `done`；`*stopped,...` → `stopped`。
fn class_of(rest: &str) -> String {
    let body = &rest[1..];
    let end = body
        .find(',')
        .unwrap_or(body.len());
    body[..end].to_string()
}

/// 取结果子串：`^done,a=1,b=2` → `a=1,b=2`。
fn results_of(rest: &str) -> &str {
    let body = &rest[1..];
    match body.find(',') {
        Some(i) => &body[i + 1..],
        None => "",
    }
}

/// 解析 `k=v,k=v,...`（可含嵌套 tuple/list/string）。遇到无法解析处停止。
pub fn parse_results(s: &str) -> Result<Vec<(String, MiValue)>, String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let b = s.as_bytes();
    while i < b.len() {
        // 跳过空白与逗号
        while i < b.len() && (b[i] == b',' || b[i].is_ascii_whitespace()) {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        // key 直到 '='
        let key_start = i;
        while i < b.len() && b[i] != b'=' {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let key = s[key_start..i].trim().to_string();
        i += 1; // 跳过 '='
        let (val, consumed) = parse_value(&s[i..])?;
        out.push((key, val));
        i += consumed;
    }
    Ok(out)
}

/// 解析单个值，返回 (值, 消费的字节数)。
fn parse_value(s: &str) -> Result<(MiValue, usize), String> {
    let b = s.as_bytes();
    let mut i = 0usize;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= b.len() {
        return Err("值解析：意外结尾".to_string());
    }
    match b[i] {
        b'"' => {
            let (str_val, len) = parse_c_string(&s[i..])?;
            Ok((MiValue::String(str_val), i + len))
        }
        b'{' => {
            let (fields, len) = parse_tuple(&s[i..])?;
            Ok((MiValue::Tuple(fields), i + len))
        }
        b'[' => {
            let (items, len) = parse_list(&s[i..])?;
            Ok((MiValue::List(items), i + len))
        }
        _ => {
            // 裸常量：到 ',' '}' ']' 或结尾
            let start = i;
            while i < b.len() && !matches!(b[i], b',' | b'}' | b']') {
                i += 1;
            }
            let text = s[start..i].trim().to_string();
            Ok((MiValue::Const(text), i))
        }
    }
}

/// 解析 `{k=v, k2=v2}`，返回字段与消费长度（含右花括号）。
fn parse_tuple(s: &str) -> Result<(Vec<(String, MiValue)>, usize), String> {
    let b = s.as_bytes();
    let mut i = 1usize; // 跳过 '{'
    let mut fields = Vec::new();
    loop {
        while i < b.len() && (b[i] == b',' || b[i].is_ascii_whitespace()) {
            i += 1;
        }
        if i >= b.len() {
            return Err("元组解析：未闭合".to_string());
        }
        if b[i] == b'}' {
            return Ok((fields, i + 1));
        }
        let key_start = i;
        while i < b.len() && b[i] != b'=' {
            i += 1;
        }
        if i >= b.len() {
            return Err("元组解析：缺少 '='".to_string());
        }
        let key = s[key_start..i].trim().to_string();
        i += 1;
        let (val, consumed) = parse_value(&s[i..])?;
        fields.push((key, val));
        i += consumed;
    }
}

/// 解析 `[e1, e2]`，元素可为 `k=v`（记为单元素 Tuple）或裸值。
fn parse_list(s: &str) -> Result<(Vec<MiValue>, usize), String> {
    let b = s.as_bytes();
    let mut i = 1usize; // 跳过 '['
    let mut items = Vec::new();
    loop {
        while i < b.len() && (b[i] == b',' || b[i].is_ascii_whitespace()) {
            i += 1;
        }
        if i >= b.len() {
            return Err("列表解析：未闭合".to_string());
        }
        if b[i] == b']' {
            return Ok((items, i + 1));
        }
        // 仅当元素以标识符开头（字母/下划线）时才尝试识别 `name=value`
        // （`{...}` / `[...]` / `"..."` 开头的元素直接交给 parse_value，
        //  否则会把 `{name` 误判为 key）。
        if b[i].is_ascii_alphabetic() || b[i] == b'_' {
            if let Some(eq) = find_unquoted_eq(&s[i..]) {
                let key = s[i..i + eq].trim().to_string();
                let (val, consumed) = parse_value(&s[i + eq + 1..])?;
                // `child={...}` / `frame={...}` 这类元素：名字是冗余的，
                // 直接压入裸元组/裸列表，消费方按内容解析（gdbmi 惯例）。
                match &val {
                    MiValue::Tuple(_) | MiValue::List(_) => items.push(val),
                    _ => items.push(MiValue::Tuple(vec![(key, val)])),
                }
                i += eq + 1 + consumed;
                continue;
            }
        }
        let (val, consumed) = parse_value(&s[i..])?;
        items.push(val);
        i += consumed;
    }
}

/// 在字符串外层找第一个未加引号的 `=`。
fn find_unquoted_eq(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut in_str = false;
    while i < b.len() {
        match b[i] {
            b'"' => in_str = !in_str,
            b'=' if !in_str => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// 解析 C 字符串（含首引号），返回 (解码后文本, 消费长度，含尾引号)。
fn parse_c_string(s: &str) -> Result<(String, usize), String> {
    let b = s.as_bytes();
    if b.first() != Some(&b'"') {
        return Err("C 字符串：缺少开引号".to_string());
    }
    let mut out = String::new();
    let mut i = 1usize;
    loop {
        if i >= b.len() {
            return Err("C 字符串：未闭合".to_string());
        }
        match b[i] {
            b'"' => return Ok((out, i + 1)),
            b'\\' => {
                if i + 1 >= b.len() {
                    return Err("C 字符串：转义截断".to_string());
                }
                let (ch, len) = decode_escape(&b[i..])?;
                out.push(ch);
                i += len;
            }
            _ => {
                // 逐字节按 UTF-8 推进
                let ch_len = utf8_len(b[i]);
                let slice = &s[i..i + ch_len.min(b.len() - i)];
                out.push_str(slice);
                i += ch_len;
            }
        }
    }
}

/// 计算 UTF-8 字符字节数。
fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >= 0xF0 {
        4
    } else if first >= 0xE0 {
        3
    } else if first >= 0xC0 {
        2
    } else {
        1
    }
}

/// 解码一个转义序列（`\` 已消费，i 指向转义首字节）。返回 (字符, 消费长度)。
fn decode_escape(b: &[u8]) -> Result<(char, usize), String> {
    match b[1] {
        b'n' => Ok(('\n', 2)),
        b't' => Ok(('\t', 2)),
        b'r' => Ok(('\r', 2)),
        b'a' => Ok(('\u{7}', 2)),
        b'b' => Ok(('\u{8}', 2)),
        b'f' => Ok(('\u{c}', 2)),
        b'v' => Ok(('\u{b}', 2)),
        b'"' => Ok(('"', 2)),
        b'\\' => Ok(('\\', 2)),
        b'\'' => Ok(('\'', 2)),
        b'0'..=b'7' => {
            // 八进制 \NNN（最多 3 位）
            let mut val = 0u32;
            let mut n = 0;
            while n < 3 && n + 1 < b.len() && matches!(b[n + 1], b'0'..=b'7') {
                val = val * 8 + (b[n + 1] - b'0') as u32;
                n += 1;
            }
            Ok((char::from_u32(val).unwrap_or('\u{fffd}'), n + 1))
        }
        b'x' => {
            // 十六进制 \xNN
            let mut val = 0u32;
            let mut n = 0;
            while n < 2 && n + 2 < b.len() {
                let c = b[n + 2];
                let d = match c {
                    b'0'..=b'9' => c - b'0',
                    b'a'..=b'f' => c - b'a' + 10,
                    b'A'..=b'F' => c - b'A' + 10,
                    _ => break,
                };
                val = val * 16 + d as u32;
                n += 1;
            }
            Ok((char::from_u32(val).unwrap_or('\u{fffd}'), n + 2))
        }
        other => Err(format!("未知转义 \\{}", other as char)),
    }
}

/// 解码一整条 `"..."`（供 `~"..."` / `@"..."` / `&"..."` 使用）。
fn decode_c_string(s: &str) -> Option<String> {
    parse_c_string(s).ok().map(|(text, _)| text)
}

// ---------------------------------------------------------------------------
// 语义辅助
// ---------------------------------------------------------------------------

/// `*stopped` 记录中的 frame 信息。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoppedFrame {
    pub addr: String,
    pub func: String,
    pub file: String,
    pub fullname: String,
    pub line: Option<u32>,
}

impl StoppedFrame {
    /// 从 `*stopped` 的 results 中提取 frame 信息（缺省字段留空）。
    pub fn from_results(results: &[(String, MiValue)]) -> Option<Self> {
        let frame = results
            .iter()
            .find(|(k, _)| k == "frame")
            .map(|(_, v)| v)
            .filter(|v| matches!(v, MiValue::Tuple(_)))?;
        Some(Self {
            addr: frame.get("addr").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            func: frame.get("func").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            file: frame.get("file").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            fullname: frame.get("fullname").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            line: frame
                .get("line")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stream_stopped_breakpoint_hit() {
        let line = r#"*stopped,reason="breakpoint-hit",disp="keep",bkptno="1",frame={addr="0x0000000000401156",func="main",args=[],file="debug_target.cpp",fullname="/mnt/e/TestDeepSeek/testdata/debug_target.cpp",line="10",arch="i386:x86-64"},thread-id="1",stopped-threads="all",core="3""#;
        match parse_record(line) {
            MiRecord::Stream { class, results } => {
                assert_eq!(class, "stopped");
                assert_eq!(results[0].0, "reason");
                assert_eq!(results[0].1.as_str(), Some("breakpoint-hit"));
                let frame = StoppedFrame::from_results(&results).expect("应解析出 frame");
                assert_eq!(frame.func, "main");
                assert_eq!(frame.file, "debug_target.cpp");
                assert_eq!(frame.fullname, "/mnt/e/TestDeepSeek/testdata/debug_target.cpp");
                assert_eq!(frame.line, Some(10));
            }
            other => panic!("预期 Stream 记录，实际 {other:?}"),
        }
    }

    #[test]
    fn parse_stream_stopped_exited() {
        let line = r#"*stopped,reason="exited",exit-code="03""#;
        match parse_record(line) {
            MiRecord::Stream { class, results } => {
                assert_eq!(class, "stopped");
                assert_eq!(results[0].1.as_str(), Some("exited"));
                assert_eq!(results[1].0, "exit-code");
                assert_eq!(results[1].1.as_str(), Some("03"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_result_done_breakpoint() {
        let line = r#"^done,bkpt={number="1",type="breakpoint",disp="keep",enabled="y",addr="0x0000000000401156",func="main",file="debug_target.cpp",fullname="/mnt/e/TestDeepSeek/testdata/debug_target.cpp",line="10",thread-groups=["i1"],times="0",original-location="debug_target.cpp:10"}"#;
        match parse_record(line) {
            MiRecord::Result { token, class, results } => {
                assert_eq!(token, None);
                assert_eq!(class, "done");
                let bkpt = results[0].1.get("number").and_then(|v| v.as_str());
                assert_eq!(bkpt, Some("1"));
                let line_val = results[0].1.get("line").and_then(|v| v.as_str());
                assert_eq!(line_val, Some("10"));
                let groups = results[0].1.get("thread-groups").and_then(|v| v.as_list());
                assert!(groups.is_some(), "thread-groups 应为列表");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_result_error_with_escaped_quotes() {
        let line = r#"^error,msg="No symbol table is loaded. Use the \"file\" command.""#;
        match parse_record(line) {
            MiRecord::Result { class, results, .. } => {
                assert_eq!(class, "error");
                assert_eq!(
                    results[0].1.as_str(),
                    Some(r#"No symbol table is loaded. Use the "file" command."#)
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_result_running_and_exit() {
        match parse_record("^running") {
            MiRecord::Result { class, results, .. } => {
                assert_eq!(class, "running");
                assert!(results.is_empty());
            }
            other => panic!("{other:?}"),
        }
        match parse_record("^exit") {
            MiRecord::Result { class, .. } => assert_eq!(class, "exit"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_result_with_token() {
        let line = "12^done,value=\"42\"";
        match parse_record(line) {
            MiRecord::Result { token, class, results } => {
                assert_eq!(token, Some(12));
                assert_eq!(class, "done");
                assert_eq!(results[0].1.as_str(), Some("42"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_async_breakpoint_created() {
        let line = r#"=breakpoint-created,bkpt={number="1",type="breakpoint",disp="keep",enabled="y",addr="0x0000000000401156",func="main",file="debug_target.cpp",fullname="/mnt/e/TestDeepSeek/testdata/debug_target.cpp",line="10",thread-groups=["i1"],times="0",original-location="debug_target.cpp:10"}"#;
        match parse_record(line) {
            MiRecord::Async { class, results } => {
                assert_eq!(class, "breakpoint-created");
                let number = results[0].1.get("number").and_then(|v| v.as_str());
                assert_eq!(number, Some("1"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_console_stream_with_escapes() {
        let line = r#"~"Breakpoint 1 at 0x401156: file debug_target.cpp, line 10.\n""#;
        match parse_record(line) {
            MiRecord::Console(text) => {
                assert_eq!(
                    text,
                    "Breakpoint 1 at 0x401156: file debug_target.cpp, line 10.\n"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_console_ptype_output() {
        let line = r#"~"                  total size (bytes):   32\n""#;
        match parse_record(line) {
            MiRecord::Console(text) => {
                assert!(text.contains("total size (bytes):   32"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_target_and_log_streams() {
        match parse_record(r#"@"hello from target\n""#) {
            MiRecord::Target(t) => assert_eq!(t, "hello from target\n"),
            other => panic!("{other:?}"),
        }
        match parse_record(r#"&"gdb: warning: something\n""#) {
            MiRecord::Log(l) => assert_eq!(l, "gdb: warning: something\n"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_prompt() {
        assert_eq!(parse_record("(gdb) "), MiRecord::Prompt);
        assert_eq!(parse_record("(gdb)"), MiRecord::Prompt);
    }

    #[test]
    fn parse_unstructured_output() {
        match parse_record("hello world") {
            MiRecord::Output(s) => assert_eq!(s, "hello world"),
            other => panic!("{other:?}"),
        }
        match parse_record("") {
            MiRecord::Output(s) => assert!(s.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_stack_list_variables() {
        let line = r#"^done,variables=[{name="i",value="10",type="int"},{name="sum",value="55",type="int"},{name="foo",value="{a = 42, b = 3.14}",type="struct Foo"}]"#;
        match parse_record(line) {
            MiRecord::Result { class, results, .. } => {
                assert_eq!(class, "done");
                let vars = results[0].1.as_list().expect("variables 应为列表");
                assert_eq!(vars.len(), 3);
                let first = vars[0].as_tuple().unwrap();
                assert_eq!(first[0].0, "name");
                assert_eq!(first[0].1.as_str(), Some("i"));
                assert_eq!(first[1].1.as_str(), Some("10"));
                assert_eq!(first[2].1.as_str(), Some("int"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_var_list_children() {
        let line = r#"^done,numchild="3",children=[child={name="var1.a",exp="a",numchild="0",value="42",type="int",thread-id="1"},child={name="var1.b",exp="b",numchild="0",value="3.14",type="double",thread-id="1"},child={name="var1.c",exp="c",numchild="0",value="\"hello\\000\"",type="char [16]",thread-id="1"}],has_more="0""#;
        match parse_record(line) {
            MiRecord::Result { results, .. } => {
                let children = results
                    .iter()
                    .find(|(k, _)| k == "children")
                    .and_then(|(_, v)| v.as_list())
                    .expect("children 应为列表");
                assert_eq!(children.len(), 3);
                let c0 = children[0].as_tuple().unwrap();
                assert_eq!(c0[0].1.as_str(), Some("var1.a"));
                // 第三个 child 的 value 应解码内层引号
                let c2 = children[2].as_tuple().unwrap();
                assert_eq!(c2[3].1.as_str(), Some("\"hello\\000\""));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_deep_nested_frame() {
        let line = r#"*stopped,reason="end-stepping-range",frame={addr="0x401161",func="helper",args=[{name="x",value="55"}],file="debug_target.cpp",fullname="/mnt/e/TestDeepSeek/testdata/debug_target.cpp",line="2",arch="i386:x86-64"},thread-id="1",stopped-threads="all""#;
        match parse_record(line) {
            MiRecord::Stream { results, .. } => {
                let frame = StoppedFrame::from_results(&results).unwrap();
                assert_eq!(frame.func, "helper");
                assert_eq!(frame.line, Some(2));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn escaped_string_decoding_octal_and_hex() {
        // 直接测试 parse_c_string 的转义：\000、\x41、\n、\t、\"、\\
        let (s, len) = parse_c_string(r#""a\000b\x41c\nd\t\"e\\f""#).unwrap();
        assert_eq!(s, "a\u{0}bAc\nd\t\"e\\f");
        assert!(len > 10);
    }

    #[test]
    fn tuple_value_display() {
        let v = MiValue::Tuple(vec![
            ("a".to_string(), MiValue::Const("1".to_string())),
            ("b".to_string(), MiValue::String("x".to_string())),
        ]);
        assert_eq!(v.display(), "{a = 1, b = x}");
    }

    #[test]
    fn result_error_unknown_msg() {
        let line = r#"^error,msg="unknown command: -foo""#;
        match parse_record(line) {
            MiRecord::Result { class, results, .. } => {
                assert_eq!(class, "error");
                assert_eq!(results[0].1.as_str(), Some("unknown command: -foo"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn results_of_keeps_raw() {
        assert_eq!(results_of("^done"), "");
        assert_eq!(results_of("^done,a=1"), "a=1");
        assert_eq!(results_of("*stopped,reason=\"x\""), "reason=\"x\"");
        assert_eq!(class_of("^done"), "done");
        assert_eq!(class_of("^error,msg=\"x\""), "error");
        assert_eq!(class_of("*stopped,reason=\"x\""), "stopped");
        assert_eq!(class_of("=breakpoint-created,bkpt={}"), "breakpoint-created");
    }
}
