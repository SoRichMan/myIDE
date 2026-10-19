#!/usr/bin/env python3
"""Mock clangd for LiteIDE LSP integration tests.

Speaks the LSP framing protocol (Content-Length header + JSON body) over stdio,
and answers requests with canned, deterministic responses so tests can assert
routing, serialization, versioning and crash-recovery without a real compiler.

Behavior contract (used by crates/ide-lsp/tests/integration.rs):
- initialize        -> static capabilities, serverInfo "mock-clangd"
- initialized       -> no response (notification)
- didOpen           -> remembers text + version; sends window/logMessage
- didChange         -> updates text + version (full-sync model)
- didSave/didClose  -> tracked
- definition        -> Location in math_utils.cpp line 3
- declaration       -> Location in math_utils.h  line 6
- typeDefinition    -> Location in math_utils.h  line 13
                       (if position.character == 0 -> JSON-RPC error -32601)
- implementation    -> intentionally NO response (client must time out)
- hover             -> MarkupContent echoing received version/line/char
- documentSymbol    -> nested DocumentSymbol[] (main -> sum)
- shutdown          -> result null; exit -> process exits 0
- test/pleaseExit   -> exits with code 1 (simulates clangd crash)
- test/pleaseExitByClose -> same, but triggered by didClose on uri "exit://mock"
"""
import json
import sys


def read_frame():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if line in (b"\r\n", b"\n", b""):
            break
        line = line.rstrip(b"\r\n")
        if b":" in line:
            key, value = line.split(b":", 1)
            headers[key.strip().lower()] = value.strip()
    if b"content-length" not in headers:
        return None
    length = int(headers[b"content-length"])
    body = sys.stdin.buffer.read(length)
    return json.loads(body.decode("utf-8"))


def send(obj):
    body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
    sys.stdout.buffer.write(
        b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n" + body
    )
    sys.stdout.buffer.flush()


def log(msg):
    sys.stderr.write("mock: " + msg + "\n")
    sys.stderr.flush()


state = {"version": 0, "text": ""}


def handle(msg):
    method = msg.get("method")
    params = msg.get("params") or {}
    ident = msg.get("id")

    if method == "initialize":
        send({
            "jsonrpc": "2.0",
            "id": ident,
            "result": {
                "capabilities": {
                    "textDocumentSync": {"openClose": True, "change": 1},
                    "hoverProvider": True,
                    "definitionProvider": True,
                    "declarationProvider": True,
                    "typeDefinitionProvider": True,
                    "implementationProvider": True,
                    "documentSymbolProvider": True,
                },
                "serverInfo": {"name": "mock-clangd", "version": "0.1"},
            },
        })
    elif method == "initialized":
        pass
    elif method == "textDocument/didOpen":
        doc = params["textDocument"]
        state["text"] = doc.get("text", "")
        state["version"] = doc.get("version", 0)
        send({
            "jsonrpc": "2.0",
            "method": "window/logMessage",
            "params": {"type": 3, "message": "mock opened " + doc.get("uri", "")},
        })
    elif method == "textDocument/didChange":
        state["version"] = params["textDocument"]["version"]
        for change in params.get("contentChanges") or []:
            if "text" in change:
                state["text"] = change["text"]
    elif method == "textDocument/didSave":
        state["version"] += 1
    elif method == "textDocument/didClose":
        uri = params.get("textDocument", {}).get("uri", "")
        if uri == "exit://mock":
            log("exiting with code 1 per didClose request")
            sys.exit(1)
        state["text"] = ""
    elif method == "textDocument/definition":
        send({
            "jsonrpc": "2.0",
            "id": ident,
            "result": [{
                "uri": "file:///mnt/e/TestDeepSeek/testdata/sample_project/src/math_utils.cpp",
                "range": {"start": {"line": 3, "character": 4},
                          "end": {"line": 3, "character": 7}},
            }],
        })
    elif method == "textDocument/declaration":
        send({
            "jsonrpc": "2.0",
            "id": ident,
            "result": [{
                "uri": "file:///mnt/e/TestDeepSeek/testdata/sample_project/src/math_utils.h",
                "range": {"start": {"line": 6, "character": 4},
                          "end": {"line": 6, "character": 7}},
            }],
        })
    elif method == "textDocument/typeDefinition":
        pos = params.get("position", {})
        if pos.get("character") == 0:
            send({
                "jsonrpc": "2.0",
                "id": ident,
                "error": {"code": -32601, "message": "method not found (mock)"},
            })
        else:
            send({
                "jsonrpc": "2.0",
                "id": ident,
                "result": [{
                    "uri": "file:///mnt/e/TestDeepSeek/testdata/sample_project/src/math_utils.h",
                    "range": {"start": {"line": 13, "character": 7},
                              "end": {"line": 13, "character": 12}},
                }],
            })
    elif method == "textDocument/implementation":
        # 故意不响应 → 客户端应超时
        log("implementation: no response (intentional timeout)")
    elif method == "textDocument/hover":
        pos = params.get("position", {})
        send({
            "jsonrpc": "2.0",
            "id": ident,
            "result": {
                "contents": {
                    "kind": "markdown",
                    "value": "```cpp\nint add(int, int)\n```\n"
                             "version={} line={} char={}".format(
                                 state["version"], pos.get("line"), pos.get("character")
                             ),
                },
                "range": None,
            },
        })
    elif method == "textDocument/documentSymbol":
        send({
            "jsonrpc": "2.0",
            "id": ident,
            "result": [
                {
                    "name": "main",
                    "kind": 12,
                    "range": {"start": {"line": 3, "character": 0},
                              "end": {"line": 9, "character": 1}},
                    "selectionRange": {"start": {"line": 3, "character": 4},
                                       "end": {"line": 3, "character": 8}},
                    "children": [
                        {
                            "name": "sum",
                            "kind": 13,
                            "range": {"start": {"line": 4, "character": 4},
                                      "end": {"line": 4, "character": 11}},
                            "selectionRange": {"start": {"line": 4, "character": 4},
                                               "end": {"line": 4, "character": 7}},
                        }
                    ],
                }
            ],
        })
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": ident, "result": None})
    elif method == "exit":
        sys.exit(0)
    elif method == "test/pleaseExit":
        log("exiting with code 1 per test request")
        sys.exit(1)
    else:
        log("unhandled method: " + str(method))


def main():
    log("mock clangd started")
    while True:
        msg = read_frame()
        if msg is None:
            break
        handle(msg)


if __name__ == "__main__":
    main()
