//! ide-dap：GDB MI 调试适配（阶段 3）。
//!
//! 分层：
//! - [`mi`]：MI 协议输出解析（流记录/结果记录/异步通知/控制台输出、值解析与 C 字符串转义）
//! - [`command`]：MI/CLI 命令构建与参数转义（断点/单步/变量/栈/ptype）
//! - [`path`]：Windows ↔ WSL 路径转换
//! - [`process`]：WSL 发行版探测 / gdb 启动（`LITEIDE_GDB_CMD` 可覆盖，测试用）
//! - [`client`]：异步 GDB 客户端（命令路由、超时、事件转发、进程退出监控）
//! - [`manager`]：UI 门面（状态机、断点/单步/变量编排，UI 线程不阻塞）

pub mod client;
pub mod command;
pub mod manager;
pub mod mi;
pub mod path;
pub mod process;

pub use client::{GdbClient, GdbError, GdbEvent, GdbEvents, MiResponse, DEFAULT_TIMEOUT};
pub use manager::{BreakpointInfo, BreakpointSpec, Command, DapManager, DapStatus, Event, StackFrame, VarNode};
