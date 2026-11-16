//! stdio MCP shim: `dos-commander mcp --role <role>`.
//!
//! The agent client launches this process and speaks JSON-RPC 2.0 over
//! stdin/stdout, one message per line.  Every `tools/call` is forwarded to
//! the TUI socket with the role's token; the TUI is the authority on what
//! is allowed (the tool list shown here is only a convenience).

use std::io::{self, BufRead, Write};
use std::path::Path;

use serde_json::{json, Value};

#[cfg_attr(not(unix), allow(unused_imports))]
use super::{tools_for, Role, WireRequest, WireResponse};

/// Key under which a tool result carries a base64 PNG (see `handle_message`).
pub const IMAGE_KEY: &str = "_image_png_base64";

pub const PROTOCOL_FALLBACK: &str = "2024-11-05";
const KNOWN_PROTOCOLS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

/// One request/response exchange with the TUI socket.
pub fn call_socket(
    path: &Path,
    token: &str,
    id: u64,
    tool: &str,
    args: &Value,
) -> Result<Value, String> {
    #[cfg(not(unix))]
    {
        let _ = (path, token, id, tool, args);
        Err("the agent connector needs Unix sockets".to_string())
    }
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        let mut stream = UnixStream::connect(path).map_err(|e| {
            format!(
                "cannot reach the dos terminal at {} ({e}); start it with --agent",
                path.display()
            )
        })?;
        stream
            .set_read_timeout(Some(
                super::server::REPLY_TIMEOUT + std::time::Duration::from_secs(2),
            ))
            .map_err(|e| e.to_string())?;
        let req = WireRequest {
            id,
            token: token.to_string(),
            tool: tool.to_string(),
            args: args.clone(),
        };
        let mut text = serde_json::to_string(&req).map_err(|e| e.to_string())?;
        text.push('\n');
        stream
            .write_all(text.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut line = String::new();
        io::BufReader::new(&stream)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        let resp: WireResponse = serde_json::from_str(line.trim())
            .map_err(|e| format!("bad reply from the terminal: {e}"))?;
        if resp.ok {
            Ok(resp.result)
        } else {
            Err(resp.error.unwrap_or_else(|| "failed".into()))
        }
    }
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Handle one JSON-RPC message.  `None` = nothing to send back (a
/// notification).  `call` performs a tool call against the TUI.
pub fn handle_message(
    role: Role,
    msg: &Value,
    call: &mut dyn FnMut(&str, &Value) -> Result<Value, String>,
) -> Option<Value> {
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let Some(id) = msg.get("id").cloned() else {
        return None; // notification (e.g. notifications/initialized)
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let reply = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
    Some(match method {
        "initialize" => {
            let asked = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("");
            let version = if KNOWN_PROTOCOLS.contains(&asked) {
                asked
            } else {
                PROTOCOL_FALLBACK
            };
            reply(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {
                    "name": format!("dos-{}", role.name()),
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }))
        }
        "ping" => reply(json!({})),
        "tools/list" => {
            let tools: Vec<Value> = tools_for(role)
                .into_iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": serde_json::from_str::<Value>(t.input_schema)
                            .unwrap_or(Value::Null),
                    })
                })
                .collect();
            reply(json!({ "tools": tools }))
        }
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(rpc_error(&id, -32602, "tools/call needs a tool name"));
            };
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            match call(name, &args) {
                Ok(mut v) => {
                    // A tool may return a picture: the PNG travels inside the
                    // JSON as base64 and becomes an MCP image block here.
                    let image = v
                        .as_object_mut()
                        .and_then(|o| o.remove(IMAGE_KEY))
                        .and_then(|x| x.as_str().map(str::to_string));
                    let mut content = Vec::new();
                    if let Some(data) = image {
                        content
                            .push(json!({"type": "image", "data": data, "mimeType": "image/png"}));
                    }
                    content.push(json!({"type": "text",
                                        "text": serde_json::to_string_pretty(&v).unwrap_or_default()}));
                    reply(json!({ "content": content, "isError": false }))
                }
                Err(e) => reply(json!({
                    "content": [{"type": "text", "text": e}],
                    "isError": true
                })),
            }
        }
        other => rpc_error(&id, -32601, &format!("method not found: {other}")),
    })
}

/// Longest message accepted on stdin (bytes).
pub const MAX_MESSAGE: u64 = 1 << 20;

/// Serve MCP on stdin/stdout until stdin closes.
pub fn run_stdio(role: Role, token: &str, socket: &Path) -> io::Result<()> {
    use std::io::Read;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut out = io::stdout().lock();
    let mut next_id = 1u64;
    loop {
        let mut line = String::new();
        let n = input.by_ref().take(MAX_MESSAGE).read_line(&mut line)?;
        if n == 0 {
            return Ok(());
        }
        if !line.ends_with('\n') && n as u64 >= MAX_MESSAGE {
            writeln!(
                out,
                "{}",
                rpc_error(&Value::Null, -32600, "message too large")
            )?;
            return Ok(()); // the stream is out of sync: stop
        }
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle_message(role, &msg, &mut |tool, args| {
                next_id += 1;
                call_socket(socket, token, next_id, tool, args)
            }),
            Err(_) => Some(rpc_error(&Value::Null, -32700, "parse error")),
        };
        if let Some(r) = reply {
            writeln!(out, "{r}")?;
            out.flush()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(role: Role, msg: Value) -> Option<Value> {
        handle_message(role, &msg, &mut |t, _| match t {
            "whoami" => Ok(json!({"role": "x"})),
            _ => Err(format!("forbidden: {t}")),
        })
    }

    #[test]
    fn initialize_negotiates_a_known_protocol_and_names_the_role() {
        let r = run(
            Role::Scalper,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
        )
        .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "dos-scalper");
        let r = run(
            Role::Risk,
            json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999"}}),
        )
        .unwrap();
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_FALLBACK);
    }

    #[test]
    fn notifications_get_no_reply() {
        assert!(run(
            Role::Analyst,
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .is_none());
    }

    #[test]
    fn tools_list_only_shows_the_roles_tools() {
        for role in Role::ALL {
            let r = run(role, json!({"jsonrpc":"2.0","id":3,"method":"tools/list"})).unwrap();
            let names: Vec<&str> = r["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap())
                .collect();
            let want: Vec<&str> = tools_for(role).iter().map(|t| t.name).collect();
            assert_eq!(names, want);
            for t in r["result"]["tools"].as_array().unwrap() {
                assert_eq!(t["inputSchema"]["type"], "object");
            }
        }
    }

    #[test]
    fn tools_call_wraps_results_and_errors() {
        let ok = run(
            Role::Analyst,
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"whoami","arguments":{}}}),
        )
        .unwrap();
        assert_eq!(ok["result"]["isError"], false);
        assert!(ok["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("role"));
        let bad = run(
            Role::Analyst,
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"place_order"}}),
        )
        .unwrap();
        assert_eq!(bad["result"]["isError"], true);
        assert!(bad["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("forbidden"));
        let nameless = run(
            Role::Analyst,
            json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{}}),
        )
        .unwrap();
        assert_eq!(nameless["error"]["code"], -32602);
    }

    #[test]
    fn unknown_methods_error_and_ping_works() {
        let r = run(
            Role::Analyst,
            json!({"jsonrpc":"2.0","id":7,"method":"resources/list"}),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32601);
        let r = run(
            Role::Analyst,
            json!({"jsonrpc":"2.0","id":8,"method":"ping"}),
        )
        .unwrap();
        assert_eq!(r["result"], json!({}));
    }

    #[cfg(unix)]
    #[test]
    fn unreachable_terminal_gives_a_helpful_error() {
        let e = call_socket(
            Path::new("/nonexistent/agent.sock"),
            "t",
            1,
            "whoami",
            &Value::Null,
        )
        .unwrap_err();
        assert!(e.contains("--agent"), "{e}");
    }

    #[test]
    fn an_image_in_a_tool_result_becomes_an_mcp_image_block() {
        let r = handle_message(
            Role::Analyst,
            &json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"get_chart_image","arguments":{}}}),
            &mut |_, _| Ok(json!({IMAGE_KEY: "QUJD", "symbol": "X"})),
        )
        .unwrap();
        let content = r["result"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["data"], "QUJD");
        assert_eq!(content[0]["mimeType"], "image/png");
        assert_eq!(content[1]["type"], "text");
        let text = content[1]["text"].as_str().unwrap();
        assert!(
            text.contains("\"symbol\"") && !text.contains("QUJD"),
            "the base64 is not repeated as text"
        );
        assert_eq!(r["result"]["isError"], false);
    }
}
