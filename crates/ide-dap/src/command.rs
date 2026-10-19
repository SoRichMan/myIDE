//! GDB MI 命令构建（阶段 3）。
//!
//! 所有命令以 `-` 开头（MI 命令）或直接 CLI 命令（如 `ptype /o`），行尾换行由
//! 客户端统一追加。MI 参数若含空白/引号/反斜杠必须用双引号包裹并转义——
//! 本模块负责统一的 `escape_mi_arg`，避免手工拼接出错（守护用例要求：
//! 条件断点命令拼接正确、特殊字符转义正确）。

/// 转义一个 MI 命令参数：含空白/引号/反斜杠时用双引号包裹，
/// 内部 `\` → `\\`、`"` → `\"`（gdb MI 引号规则）。
pub fn escape_mi_arg(s: &str) -> String {
    let needs_quotes = s
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\\');
    if !needs_quotes {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `-break-insert [ -c cond ] location`：行断点/条件断点。
pub fn break_insert(location: &str, condition: Option<&str>) -> String {
    let loc = escape_mi_arg(location);
    match condition {
        Some(c) => format!("-break-insert -c {} {}", escape_mi_arg(c), loc),
        None => format!("-break-insert {}", loc),
    }
}

/// `-break-delete n1 n2 ...`（也可用 `-break-delete` 删全部）。
pub fn break_delete(numbers: &[u32]) -> String {
    if numbers.is_empty() {
        return "-break-delete".to_string();
    }
    let nums = numbers
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    format!("-break-delete {nums}")
}

/// `-break-disable n1 n2 ...`。
pub fn break_disable(numbers: &[u32]) -> String {
    let nums = numbers
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    format!("-break-disable {nums}")
}

/// `-break-enable n1 n2 ...`。
pub fn break_enable(numbers: &[u32]) -> String {
    let nums = numbers
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    format!("-break-enable {nums}")
}

/// `-exec-run`：启动程序。
pub fn exec_run() -> String {
    "-exec-run".to_string()
}

/// `-exec-next`：单步跳过（F10）。
pub fn exec_next() -> String {
    "-exec-next".to_string()
}

/// `-exec-step`：单步进入（F11）。
pub fn exec_step() -> String {
    "-exec-step".to_string()
}

/// `-exec-finish`：单步跳出（Shift+F11）。
pub fn exec_finish() -> String {
    "-exec-finish".to_string()
}

/// `-exec-continue`：继续运行（F5）。
pub fn exec_continue() -> String {
    "-exec-continue".to_string()
}

/// `-exec-interrupt`：中断运行中的程序（Shift+F5 的暂停部分）。
pub fn exec_interrupt() -> String {
    "-exec-interrupt".to_string()
}

/// `-stack-list-variables 1`：当前帧局部变量（带值）。
pub fn stack_list_variables() -> String {
    "-stack-list-variables 1".to_string()
}

/// `-stack-list-frames`：调用栈（全部帧）。
pub fn stack_list_frames() -> String {
    "-stack-list-frames".to_string()
}

/// `-var-create - * expr`：为表达式创建变量对象（自动命名，当前帧）。
pub fn var_create(expr: &str) -> String {
    format!("-var-create - * {}", escape_mi_arg(expr))
}

/// `-var-list-children name`：列出变量对象的子字段。
pub fn var_list_children(name: &str) -> String {
    format!("-var-list-children {}", escape_mi_arg(name))
}

/// `-data-evaluate-expression expr`：直接求值（监视表达式用）。
pub fn data_evaluate_expression(expr: &str) -> String {
    format!("-data-evaluate-expression {}", escape_mi_arg(expr))
}

/// `-file-exec-and-symbols path`：加载程序与符号表。
pub fn file_exec_and_symbols(path: &str) -> String {
    format!("-file-exec-and-symbols {}", escape_mi_arg(path))
}

/// `-environment-cd dir`：切换工作目录。
pub fn environment_cd(dir: &str) -> String {
    format!("-environment-cd {}", escape_mi_arg(dir))
}

/// `-gdb-exit`：退出 gdb。
pub fn gdb_exit() -> String {
    "-gdb-exit".to_string()
}

/// `-gdb-set name value`：设置 gdb 参数（如 pagination off）。
pub fn gdb_set(name: &str, value: &str) -> String {
    format!("-gdb-set {} {}", escape_mi_arg(name), escape_mi_arg(value))
}

/// `ptype /o type`：查看结构体大小与内存布局（CLI 命令，MI 模式下同样可用）。
pub fn ptype_offset(type_name: &str) -> String {
    format!("ptype /o {}", escape_mi_arg(type_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn break_insert_plain_line() {
        assert_eq!(
            break_insert("debug_target.cpp:10", None),
            "-break-insert debug_target.cpp:10"
        );
    }

    #[test]
    fn break_insert_conditional() {
        assert_eq!(
            break_insert("debug_target.cpp:7", Some("i == 5")),
            "-break-insert -c \"i == 5\" debug_target.cpp:7"
        );
    }

    #[test]
    fn break_insert_conditional_with_quotes() {
        // 条件里含双引号（字符比较）→ 必须转义；单引号是 C 字符字面量，无需转义
        assert_eq!(
            break_insert("debug_target.cpp:1", Some("c == '\"'")),
            "-break-insert -c \"c == '\\\"'\" debug_target.cpp:1"
        );
    }

    #[test]
    fn break_insert_path_with_space() {
        assert_eq!(
            break_insert("my proj/a.cpp:10", None),
            "-break-insert \"my proj/a.cpp:10\""
        );
    }

    #[test]
    fn escape_backslash_roundtrip() {
        // 反斜杠 → 双写；MI 层解析回来正好还原
        let escaped = escape_mi_arg(r"a\b");
        assert_eq!(escaped, r#""a\\b""#);
    }

    #[test]
    fn escape_plain_no_wrap() {
        assert_eq!(escape_mi_arg("debug_target.cpp:10"), "debug_target.cpp:10");
        assert_eq!(escape_mi_arg("-"), "-");
        assert_eq!(escape_mi_arg("i"), "i");
    }

    #[test]
    fn exec_commands() {
        assert_eq!(exec_run(), "-exec-run");
        assert_eq!(exec_next(), "-exec-next");
        assert_eq!(exec_step(), "-exec-step");
        assert_eq!(exec_finish(), "-exec-finish");
        assert_eq!(exec_continue(), "-exec-continue");
        assert_eq!(exec_interrupt(), "-exec-interrupt");
    }

    #[test]
    fn stack_and_var_commands() {
        assert_eq!(stack_list_variables(), "-stack-list-variables 1");
        assert_eq!(stack_list_frames(), "-stack-list-frames");
        assert_eq!(var_create("i"), "-var-create - * i");
        assert_eq!(var_create("foo.bar"), "-var-create - * foo.bar");
        assert_eq!(var_list_children("var1"), "-var-list-children var1");
        assert_eq!(
            data_evaluate_expression("sum * 2"),
            "-data-evaluate-expression \"sum * 2\""
        );
    }

    #[test]
    fn lifecycle_commands() {
        assert_eq!(
            file_exec_and_symbols("/mnt/e/TestDeepSeek/testdata/debug_target"),
            "-file-exec-and-symbols /mnt/e/TestDeepSeek/testdata/debug_target"
        );
        assert_eq!(
            environment_cd("/mnt/e/TestDeepSeek/testdata"),
            "-environment-cd /mnt/e/TestDeepSeek/testdata"
        );
        assert_eq!(gdb_exit(), "-gdb-exit");
        assert_eq!(gdb_set("pagination", "off"), "-gdb-set pagination off");
        assert_eq!(ptype_offset("Foo"), "ptype /o Foo");
    }

    #[test]
    fn break_batch_commands() {
        assert_eq!(break_delete(&[1, 2]), "-break-delete 1 2");
        assert_eq!(break_delete(&[]), "-break-delete");
        assert_eq!(break_disable(&[3]), "-break-disable 3");
        assert_eq!(break_enable(&[1, 4]), "-break-enable 1 4");
    }
}
