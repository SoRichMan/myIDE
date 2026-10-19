; C/C++ 语法高亮查询（liteide.cpp-highlight 扩展的语言资源）
;
; 约定：
; - 捕获名 → ide_core::HighlightGroup 的映射见 src/lib.rs 的 capture_to_group()；
; - `@variable` / `@field` 是**兜底**规则，宿主会把全大写标识符改判为宏、
;   把同文件枚举成员改判为枚举值，因此它们必须排在具体规则之后（同区间时具体规则优先）；
; - 配色目标见 ext-theme-vscode（VSCode C/C++ Themes 的 scope 家族）。

; ---- 注释 ----
(comment) @comment

; ---- 字面量 ----
(number_literal) @number
(string_literal) @string
(raw_string_literal) @string
(char_literal) @string
(system_lib_string) @string          ; #include <vector> 里的 <vector>

; ---- 语言常量（constant.language → 蓝）----
; 注意：true/false/null 在 grammar 里是**具名**叶子节点（`(null)` 同时覆盖 NULL 与
; nullptr），decltype/noexcept/auto 同理，必须用 `(节点名)` 而不是字符串字面量。
[(true) (false) (null)] @constant
(this) @keyword

; ---- 控制流关键字（keyword.control → 紫）----
[
  "if" "else" "for" "while" "do" "return" "break" "continue"
  "switch" "case" "default" "goto" "try" "catch" "throw"
  "co_await" "co_return" "co_yield"
] @control

; ---- 基本类型（storage.type → 蓝）----
(primitive_type) @keyword

; 例外：`int32_t`/`size_t` 这类标准库 typedef 在 grammar 里也算 primitive_type，
; 但 VSCode 里它们是**类型色**（青）。用谓词把它们挑出来，具体度更高 → 必胜。
((primitive_type) @typedefName
  (#match? @typedefName "_t$"))
(auto) @keyword
(decltype) @keyword
(noexcept) @keyword

; ---- 其它关键字 / 存储类 / 类型修饰 ----
(type_qualifier) @keyword             ; const / volatile / restrict
(storage_class_specifier) @keyword    ; static / extern / register / mutable / inline
[
  "signed" "unsigned" "thread_local"
  "class" "struct" "union" "enum"
  "namespace" "template" "typename" "using" "typedef" "concept" "requires"
  "constexpr" "consteval" "constinit" "virtual" "explicit" "friend" "final" "override"
  "public" "private" "protected"
  "new" "delete" "operator" "sizeof" "alignof"
  "static_assert" "asm"
] @keyword

; ---- 类型与命名空间（entity.name.type / entity.name.namespace）----
(type_identifier) @type
(namespace_identifier) @namespace

; ---- 函数（entity.name.function → 黄）----
(function_declarator declarator: (identifier) @function)
(function_declarator declarator: (field_identifier) @function)
(function_declarator declarator: (qualified_identifier name: (identifier) @function))
(function_declarator declarator: (destructor_name) @function)
(function_declarator declarator: (operator_name) @function)
(call_expression function: (identifier) @function)
(call_expression function: (field_expression field: (field_identifier) @function))
(call_expression function: (qualified_identifier name: (identifier) @function))
(call_expression function: (template_function name: (identifier) @function))
(template_function name: (identifier) @function)
(template_method name: (field_identifier) @function)

; ---- 形参（variable.parameter）----
(parameter_declaration declarator: (identifier) @parameter)
(parameter_declaration declarator: (pointer_declarator declarator: (identifier) @parameter))
(parameter_declaration declarator: (reference_declarator (identifier) @parameter))

; ---- 枚举成员声明处（variable.other.enummember）；使用处由宿主按
;      「同文件枚举名集合」判定，无需跨文件语义分析 ----
(enumerator name: (identifier) @enumMember)

; ---- 预处理 ----
; 注意：`#include`/`#define`/`#if` 在 grammar 里是**匿名 token**（不是 preproc_directive
; 节点，那个节点只覆盖 #pragma 这类其余指令），所以只能按字面量匹配。
[
  "#define" "#elif" "#elifdef" "#elifndef" "#else" "#endif"
  "#if" "#ifdef" "#ifndef" "#include"
] @preproc
(preproc_directive) @preproc
(preproc_def name: (identifier) @macro)
(preproc_function_def name: (identifier) @macro)

; ---- 标点与运算符 ----
[
  "(" ")" "[" "]" "{" "}" ";" "," "." "->" "::" ":" "?"
  "=" "+" "-" "*" "/" "%" "!" "~" "&" "|" "^" "<" ">"
  "==" "!=" "<=" ">=" "&&" "||" "<<" ">>" "++" "--"
  "+=" "-=" "*=" "/=" "%=" "&=" "|=" "^=" "<<=" ">>="
] @operator

; ---- 兜底：其余标识符 ----
(identifier) @variable
(field_identifier) @field
