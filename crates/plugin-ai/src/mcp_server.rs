use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, BufRead, Write};
use time::OffsetDateTime;

#[derive(Deserialize, Debug)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    #[serde(default)]
    params: Value,
    id: Option<Value>,
}

#[derive(Serialize, Debug)]
struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
    id: Option<Value>,
}

#[derive(Serialize, Debug)]
struct JsonRpcError {
    code: i32,
    message: String,
}

pub fn run_mcp_server(server_name: &str) {
    if server_name != "time" {
        eprintln!("Unknown internal server: {}", server_name);
        std::process::exit(1);
    }

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    
    // Tools definition
    let tools = serde_json::json!({
        "tools": [
            {
                "name": "get_current_time",
                "description": "Returns the current UTC time.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        ]
    });

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        if let Ok(req) = serde_json::from_str::<JsonRpcRequest>(&line) {
            let mut response = JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: None,
                id: req.id.clone(),
            };

            match req.method.as_str() {
                "initialize" => {
                    response.result = Some(serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {
                            "tools": {}
                        },
                        "serverInfo": {
                            "name": "matrix-bot-time",
                            "version": "1.0.0"
                        }
                    }));
                }
                "tools/list" => {
                    response.result = Some(tools.clone());
                }
                "tools/call" => {
                    if let Some(params) = req.params.as_object() {
                        if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
                            if name == "get_current_time" {
                                let now = OffsetDateTime::now_utc();
                                let time_str = now.format(&time::format_description::well_known::Rfc3339).unwrap();
                                response.result = Some(serde_json::json!({
                                    "content": [
                                        {
                                            "type": "text",
                                            "text": time_str
                                        }
                                    ]
                                }));
                            } else {
                                 response.error = Some(JsonRpcError {
                                    code: -32601,
                                    message: format!("Tool not found: {}", name),
                                });
                            }
                        } else {
                            response.error = Some(JsonRpcError {
                                code: -32602,
                                message: "Missing 'name' parameter".to_string(),
                            });
                        }
                    } else {
                         response.error = Some(JsonRpcError {
                            code: -32602,
                            message: "Invalid params".to_string(),
                        });
                    }
                }
                "notificiations/initialized" => {
                     // ignore
                     continue; 
                }
                _ => {
                    // Ignore other methods or return error?
                    // MCP has ping etc.
                }
            }
            
            if response.result.is_some() || response.error.is_some() {
                 let out = serde_json::to_string(&response).unwrap();
                 let _ = writeln!(stdout, "{}", out);
                 let _ = stdout.flush();
            }
        }
    }
}
