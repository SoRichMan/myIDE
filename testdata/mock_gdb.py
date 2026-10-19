#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Mock GDB（MI2 解释器）服务器：固定应答，用于 ide-dap 管理器集成测试。

用法：设置环境变量 LITEIDE_GDB_CMD="python <本文件>" 后，DapManager 会直接
spawn 本脚本（跳过 WSL 探测），行为与真实 gdb 在 MI 模式下保持一致：

  - 启动即打印提示符 `(gdb) `
  - `-file-exec-and-symbols` / `-environment-cd` / `-gdb-set`  → `^done`
  - `-break-insert`  → `^done,bkpt={...}` 并广播 `=breakpoint-created`
  - `-exec-run`      → `^running`，随后 `*stopped`（breakpoint-hit, line=10）
  - `-exec-next/-step/-continue` → `^running`，随后 `*stopped`（line=11）
  - `-exec-finish`   → `^error` 后退出(1)（gdb 崩溃守护用例触发点）
  - `-exec-interrupt`→ `^done`，随后 `*stopped`（interrupted）
  - `-stack-list-variables` → i=10 / sum=55 / foo(struct Foo)
  - `-stack-list-frames`    → 1 帧 main@line10
  - `-data-evaluate-expression <expr>` → `^done,value=<expr>`
  - `-var-create`           → var1, numchild=3
  - `-var-list-children`    → 3 个 child（a/b/c）
  - `ptype /o ...`          → console 输出（total size 32）+ `^done`
  - `-gdb-exit`             → `^exit` 后进程退出（code 0）
  未知命令 → `^error,msg=...`

开发辅助：每条收/发记录追加写入 %TEMP%/mock_gdb.log（便于集成测试排查）。
"""

import re
import sys
import time

LOG_PATH = r"C:\Users\Administrator\AppData\Local\Temp\mock_gdb.log"


def send(line):
    sys.stdout.write(line + "\n")
    sys.stdout.flush()


def strip_token(line):
    """剥离行首数字 token，返回 (token, body)。"""
    i = 0
    while i < len(line) and line[i].isdigit():
        i += 1
    return line[:i], line[i:]


def extract_condition(body):
    """从 `-break-insert -c "cond" loc` 提取条件（mock 回显用）。"""
    m = re.search(r'-c\s+"((?:[^"\\]|\\.)*)"', body)
    return m.group(1) if m else None


def main():
    with open(LOG_PATH, "a", encoding="utf-8") as logf:
        logf.write("=== mock start ===\n")
        logf.flush()
        send("(gdb) ")
        for raw in sys.stdin:
            line = raw.strip()
            if not line:
                continue
            logf.write("RECV: %s\n" % line)
            logf.flush()
            token, body = strip_token(line)

            def reply(s):
                logf.write("SEND: %s\n" % (token + s))
                logf.flush()
                send(token + s)

            if body.startswith("-gdb-set") or body.startswith("-environment-cd") \
                    or body.startswith("-file-exec-and-symbols"):
                reply("^done")
            elif body.startswith("-break-insert"):
                cond = extract_condition(body)
                cond_field = 'cond="%s",' % cond if cond is not None else ""
                reply(
                    '^done,bkpt={number="1",type="breakpoint",disp="keep",enabled="y",'
                    'addr="0x0000000000401156",func="main",file="debug_target.cpp",'
                    'fullname="/mnt/e/TestDeepSeek/testdata/debug_target.cpp",line="10",'
                    'thread-groups=["i1"],times="0",%soriginal-location="debug_target.cpp:10"}'
                    % cond_field
                )
                send(
                    '=breakpoint-created,bkpt={number="1",type="breakpoint",disp="keep",'
                    'enabled="y",addr="0x0000000000401156",func="main",'
                    'file="debug_target.cpp",fullname="/mnt/e/TestDeepSeek/testdata/debug_target.cpp",'
                    'line="10",thread-groups=["i1"],times="0",%soriginal-location="debug_target.cpp:10"}'
                    % cond_field
                )
            elif body.startswith("-exec-run"):
                reply("^running")
                time.sleep(0.03)
                send(
                    '*stopped,reason="breakpoint-hit",disp="keep",bkptno="1",'
                    'frame={addr="0x0000000000401156",func="main",args=[],'
                    'file="debug_target.cpp",fullname="/mnt/e/TestDeepSeek/testdata/debug_target.cpp",'
                    'line="10",arch="i386:x86-64"},thread-id="1",stopped-threads="all",core="3"'
                )
            elif body.startswith("-exec-next") or body.startswith("-exec-step") \
                    or body.startswith("-exec-continue"):
                reply("^running")
                time.sleep(0.02)
                send(
                    '*stopped,reason="end-stepping-range",'
                    'frame={addr="0x0000000000401161",func="main",args=[],'
                    'file="debug_target.cpp",fullname="/mnt/e/TestDeepSeek/testdata/debug_target.cpp",'
                    'line="11",arch="i386:x86-64"},thread-id="1",stopped-threads="all"'
                )
            elif body.startswith("-exec-finish"):
                # 崩溃触发路径：模拟 gdb 在命令中途崩溃（守护用例：gdb 崩溃不拖死 IDE）
                reply('^error,msg="mock: finish not supported (crash trigger)"')
                sys.exit(1)
            elif body.startswith("-exec-interrupt"):
                reply("^done")
                time.sleep(0.02)
                send(
                    '*stopped,reason="interrupted",'
                    'frame={addr="0x0000000000401156",func="main",args=[],'
                    'file="debug_target.cpp",fullname="/mnt/e/TestDeepSeek/testdata/debug_target.cpp",'
                    'line="10",arch="i386:x86-64"},thread-id="1",stopped-threads="all"'
                )
            elif body.startswith("-stack-list-variables"):
                reply(
                    '^done,variables=['
                    '{name="i",value="10",type="int"},'
                    '{name="sum",value="55",type="int"},'
                    '{name="h",value="110",type="int"},'
                    '{name="foo",value="{a = 42, b = 3.1400000000000001, c = \\"hello\\"}",type="struct Foo"}'
                    ']'
                )
            elif body.startswith("-stack-list-frames"):
                reply(
                    '^done,stack=['
                    'frame={level="0",addr="0x0000000000401156",func="main",args=[],'
                    'file="debug_target.cpp",fullname="/mnt/e/TestDeepSeek/testdata/debug_target.cpp",'
                    'line="10",arch="i386:x86-64"}'
                    ']'
                )
            elif body.startswith("-data-evaluate-expression"):
                expr = body[len("-data-evaluate-expression "):].strip()
                # 命令构建层对含空格的表达式会加外层双引号；剥掉后回显
                if len(expr) >= 2 and expr.startswith('"') and expr.endswith('"'):
                    expr = expr[1:-1]
                reply('^done,value="' + expr.replace('"', '\\"') + '"')
            elif body.startswith("-var-create"):
                reply(
                    '^done,name="var1",numchild="3",value="{a = 42, b = 3.14, c = \\"hello\\"}",'
                    'type="struct Foo",thread-id="1",has_more="0"'
                )
            elif body.startswith("-var-list-children"):
                reply(
                    '^done,numchild="3",children=['
                    'child={name="var1.a",exp="a",numchild="0",value="42",type="int",thread-id="1"},'
                    'child={name="var1.b",exp="b",numchild="0",value="3.1400000000000001",type="double",thread-id="1"},'
                    'child={name="var1.c",exp="c",numchild="0",value="hello",type="char [16]",thread-id="1"}'
                    '],has_more="0"'
                )
            elif body.startswith("ptype"):
                send('~"/* offset    |  size */  type = struct Foo {\\n"')
                send('~"              4 |    4    int a;\\n"')
                send('~"              8 |    8    double b;\\n"')
                send('~"             16 |   16    char c[16];\\n"')
                send('~"                  total size (bytes):   32\\n"')
                reply("^done")
            elif body.startswith("-gdb-exit"):
                reply("^exit")
                sys.exit(0)
            else:
                safe = body.replace('"', '\\"')
                reply('^error,msg="unknown command: ' + safe + '"')


if __name__ == "__main__":
    main()
