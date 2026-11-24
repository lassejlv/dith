//! End-to-end tests for `dith mcp`: spawn the server and speak MCP over stdio.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tempfile::TempDir;

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Server {
    fn spawn(cwd: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_dith"))
            .arg("mcp")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            next_id: 0,
        }
    }

    fn send_line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    fn read_message(&mut self) -> Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        assert!(line.ends_with('\n'), "every message is one line: {line:?}");
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout must be JSON-RPC ({e}): {line}"))
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let mut message = json!({ "jsonrpc": "2.0", "id": id, "method": method });
        message["params"] = params;
        self.send_line(&message.to_string());
        let response = self.read_message();
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], id);
        response
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let mut params = json!({ "name": name });
        params["arguments"] = arguments;
        let response = self.request("tools/call", params);
        response
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("tools/call {name} failed: {response}"))
    }

    fn initialize(&mut self) -> Value {
        let response = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "dith-tests", "version": "1.0.0" },
            }),
        );
        self.send_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        response["result"].clone()
    }

    /// Close stdin and wait for the server to exit on EOF.
    fn shutdown(mut self) -> std::process::ExitStatus {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "server did not exit on EOF");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn report(result: &Value) -> Value {
    assert_eq!(result["isError"], false, "unexpected tool error: {result}");
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

fn tool_error(result: &Value) -> Value {
    assert_eq!(result["isError"], true, "expected a tool error: {result}");
    let envelope: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(envelope["status"], "error");
    assert!(
        envelope["suggestion"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    envelope
}

fn decode_base64(text: &str) -> Vec<u8> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => u32::from(c - b'A'),
        b'a'..=b'z' => u32::from(c - b'a') + 26,
        b'0'..=b'9' => u32::from(c - b'0') + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("invalid base64 byte {c}"),
    };
    let mut out = Vec::new();
    for chunk in text.as_bytes().chunks(4) {
        let digits: Vec<u32> = chunk
            .iter()
            .filter(|c| **c != b'=')
            .map(|c| value(*c))
            .collect();
        let n = digits
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, d)| acc | (d << (18 - 6 * i)));
        out.extend(n.to_be_bytes()[1..digits.len()].iter().copied());
    }
    out
}

/// Assert the result carries a valid PNG preview no larger than 512px and return its size.
fn preview_dimensions(result: &Value) -> (u32, u32) {
    let image = result["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["type"] == "image")
        .unwrap_or_else(|| panic!("no image content in {result}"));
    assert_eq!(image["mimeType"], "image/png");
    let bytes = decode_base64(image["data"].as_str().unwrap());
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).unwrap();
    assert!(decoded.width().max(decoded.height()) <= 512);
    (decoded.width(), decoded.height())
}

/// Minimal JSON Schema check for the subset dith's tool schemas use.
fn assert_matches_schema(schema: &Value, value: &Value, path: &str) {
    if let Some(expected) = schema.get("type") {
        let types: Vec<&str> = match expected {
            Value::String(t) => vec![t.as_str()],
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
            _ => panic!("bad type at {path}"),
        };
        let ok = types.iter().any(|t| match *t {
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "array" => value.is_array(),
            "object" => value.is_object(),
            "null" => value.is_null(),
            other => panic!("unexpected schema type {other}"),
        });
        assert!(ok, "{path}: {value} is not {types:?}");
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array) {
        assert!(
            options.contains(value),
            "{path}: {value} not in {options:?}"
        );
    }
    if let (Some(min), Some(n)) = (
        schema.get("minimum").and_then(Value::as_f64),
        value.as_f64(),
    ) {
        assert!(n >= min, "{path}: {n} < {min}");
    }
    if let (Some(items), Some(values)) = (schema.get("items"), value.as_array()) {
        for (i, item) in values.iter().enumerate() {
            assert_matches_schema(items, item, &format!("{path}[{i}]"));
        }
    }
    if let Some(object) = value.as_object() {
        let properties = schema.get("properties").and_then(Value::as_object);
        for key in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            assert!(
                object.contains_key(key.as_str().unwrap()),
                "{path}: missing required {key}"
            );
        }
        for (key, item) in object {
            match properties.and_then(|p| p.get(key)) {
                Some(sub) => assert_matches_schema(sub, item, &format!("{path}.{key}")),
                None => assert_ne!(
                    schema.get("additionalProperties"),
                    Some(&Value::Bool(false)),
                    "{path}: unexpected property {key}"
                ),
            }
        }
    }
}

fn tool_schemas(server: &mut Server) -> Map<String, Value> {
    let listed = server.request("tools/list", json!({}));
    listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| {
            (
                tool["name"].as_str().unwrap().to_string(),
                tool["inputSchema"].clone(),
            )
        })
        .collect()
}

#[test]
fn mcp_server_handshakes_lists_and_calls_every_tool() {
    let dir = TempDir::new().unwrap();
    let mut server = Server::spawn(dir.path());

    let init = server.initialize();
    assert_eq!(init["protocolVersion"], "2025-11-25");
    assert_eq!(init["serverInfo"]["name"], "dith");
    assert_eq!(init["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(init["capabilities"]["tools"].is_object());
    assert!(
        init["instructions"]
            .as_str()
            .unwrap()
            .contains("dith_apply")
    );
    assert_eq!(server.request("ping", json!({}))["result"], json!({}));

    let schemas = tool_schemas(&mut server);
    let names: Vec<&str> = schemas.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "dith_animate",
            "dith_apply",
            "dith_capabilities",
            "dith_compare",
            "dith_generate",
            "dith_inspect",
            "dith_list"
        ]
    );
    for schema in schemas.values() {
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
    }
    let apply = &schemas["dith_apply"]["properties"];
    assert_eq!(apply["max_dim"]["type"], "integer");
    assert_eq!(apply["strength"]["type"], "number");
    assert_eq!(apply["dry_run"]["type"], "boolean");
    assert!(
        apply["algorithm"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("floyd-steinberg"))
    );
    assert!(apply.get("view").is_none() && apply.get("save_preset").is_none());
    assert_eq!(
        schemas["dith_compare"]["properties"]["presets"]["type"],
        "array"
    );

    let calls = [
        (
            "dith_generate",
            json!({
                "pattern": "sphere", "output": "orb.png", "preset": "risograph",
                "canvas_width": 800, "canvas_height": 600, "emit_recipe": true,
            }),
        ),
        (
            "dith_apply",
            json!({ "input": "orb.png", "output": "out/applied.png", "algorithm": "atkinson", "colors": "#111111,#f5efe6", "max_dim": 320 }),
        ),
        (
            "dith_animate",
            json!({
                "inputs": ["waves"], "output": "loop.gif", "algorithm": "bayer4", "palette": "gameboy",
                "frames": 4, "fps": 10, "canvas_width": 96, "canvas_height": 64, "temporal": "locked",
            }),
        ),
        (
            "dith_compare",
            json!({ "pattern": "sphere", "output": "sheet.png", "presets": ["mac-classic", "risograph"], "tile_max_dim": 96 }),
        ),
        ("dith_inspect", json!({ "input": "orb.png", "colors": 4 })),
        (
            "dith_list",
            json!({ "kind": "palettes", "tag": "risograph" }),
        ),
        ("dith_capabilities", json!({ "section": "commands" })),
    ];
    let mut results = Map::new();
    for (name, arguments) in calls {
        assert_matches_schema(&schemas[name], &arguments, name);
        results.insert(name.to_string(), server.call(name, arguments));
    }

    let generated = report(&results["dith_generate"]);
    assert_eq!(generated["status"], "ok");
    assert_eq!(generated["output_dimensions"], json!([800, 600]));
    assert_eq!(preview_dimensions(&results["dith_generate"]), (400, 300));
    assert!(dir.path().join("orb.png").exists());

    let applied = report(&results["dith_apply"]);
    assert_eq!(applied["algorithm"], "atkinson");
    assert_eq!(applied["output_dimensions"], json!([320, 240]));
    assert_eq!(preview_dimensions(&results["dith_apply"]), (320, 240));
    assert!(dir.path().join("out/applied.png").exists());

    let animated = report(&results["dith_animate"]);
    assert_eq!(animated["frame_count"], 4);
    assert_eq!(preview_dimensions(&results["dith_animate"]), (96, 64));
    assert_eq!(
        &std::fs::read(dir.path().join("loop.gif")).unwrap()[..3],
        b"GIF"
    );

    let compared = report(&results["dith_compare"]);
    assert_eq!(compared["variants"].as_array().unwrap().len(), 2);
    preview_dimensions(&results["dith_compare"]);
    assert!(dir.path().join("sheet.png").exists());

    let inspected = report(&results["dith_inspect"]);
    assert_eq!(inspected["width"], 800);
    assert_eq!(
        results["dith_inspect"]["content"].as_array().unwrap().len(),
        1
    );

    let palettes = report(&results["dith_list"]);
    assert!(
        palettes
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["id"].as_str().unwrap().contains("risograph"))
    );

    let commands = report(&results["dith_capabilities"]);
    assert!(
        commands
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "mcp")
    );

    // Replay the emitted recipe inline, without the preview image.
    let replay = json!({ "input": "orb.png", "output": "replay.png", "recipe": generated["recipe"], "include_image": false });
    assert_matches_schema(&schemas["dith_apply"], &replay, "replay");
    let replayed = server.call("dith_apply", replay);
    assert_eq!(report(&replayed)["palette_name"], generated["palette_name"]);
    assert_eq!(replayed["content"].as_array().unwrap().len(), 1);

    assert!(server.shutdown().success());
}

#[test]
fn mcp_tool_errors_are_structured_results() {
    let dir = TempDir::new().unwrap();
    let mut server = Server::spawn(dir.path());
    server.initialize();
    server.call("dith_generate", json!({ "pattern": "noise", "output": "noise.png", "canvas_width": 32, "canvas_height": 32 }));

    let unknown_palette = tool_error(&server.call(
        "dith_apply",
        json!({ "input": "noise.png", "output": "x.png", "palette": "nope" }),
    ));
    assert_eq!(unknown_palette["code"], "UNKNOWN_PALETTE");
    assert!(
        unknown_palette["valid_values"]
            .as_array()
            .unwrap()
            .contains(&json!("gameboy"))
    );

    let unknown_arg = tool_error(&server.call(
        "dith_apply",
        json!({ "input": "noise.png", "output": "x.png", "fps": 3 }),
    ));
    assert_eq!(unknown_arg["code"], "INVALID_ARGUMENT");
    assert!(
        unknown_arg["valid_values"]
            .as_array()
            .unwrap()
            .contains(&json!("max_dim"))
    );

    let bad_value = tool_error(&server.call(
        "dith_apply",
        json!({ "input": "noise.png", "output": "x.png", "algorithm": "sideways" }),
    ));
    assert_eq!(bad_value["code"], "INVALID_ARGUMENT");
    assert!(bad_value["message"].as_str().unwrap().contains("sideways"));

    let stdin = tool_error(&server.call("dith_apply", json!({ "input": "-", "output": "x.png" })));
    assert_eq!(stdin["code"], "INVALID_ARGUMENT");

    let missing = tool_error(&server.call("dith_inspect", json!({ "input": "missing.png" })));
    assert_eq!(missing["code"], "RUNTIME_ERROR");

    std::fs::write(dir.path().join("taken.png"), b"keep").unwrap();
    let clobber = tool_error(&server.call(
        "dith_generate",
        json!({ "pattern": "noise", "output": "taken.png", "no_clobber": true }),
    ));
    assert_eq!(clobber["code"], "OUTPUT_EXISTS");
    assert_eq!(
        std::fs::read(dir.path().join("taken.png")).unwrap(),
        b"keep"
    );

    let bad_recipe = tool_error(&server.call("dith_apply", json!({ "input": "noise.png", "output": "x.png", "recipe": { "dither": { "algorithm": 7 } } })));
    assert_eq!(bad_recipe["code"], "INVALID_RECIPE_JSON");
    assert!(!dir.path().join("x.png").exists());

    let unknown_tool = server.request(
        "tools/call",
        json!({ "name": "dith_nope", "arguments": {} }),
    );
    assert_eq!(unknown_tool["error"]["code"], -32602);
    assert_eq!(
        server.request("resources/list", json!({}))["error"]["code"],
        -32601
    );

    server.send_line("{not json");
    let parse_error = server.read_message();
    assert_eq!(parse_error["error"]["code"], -32700);
    assert_eq!(parse_error["id"], Value::Null);

    assert!(server.shutdown().success());
}

#[test]
fn mcp_server_speaks_the_stateless_2026_revision() {
    let dir = TempDir::new().unwrap();
    let mut server = Server::spawn(dir.path());
    let meta = |version: &str| {
        json!({
            "io.modelcontextprotocol/protocolVersion": version,
            "io.modelcontextprotocol/clientInfo": { "name": "dith-tests", "version": "1.0.0" },
            "io.modelcontextprotocol/clientCapabilities": {},
        })
    };

    let discovered =
        server.request("server/discover", json!({ "_meta": meta("2026-07-28") }))["result"].clone();
    assert_eq!(discovered["resultType"], "complete");
    assert_eq!(discovered["supportedVersions"][0], "2026-07-28");
    assert!(discovered["capabilities"]["tools"].is_object());
    assert_eq!(
        discovered["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "dith"
    );

    let listed =
        server.request("tools/list", json!({ "_meta": meta("2026-07-28") }))["result"].clone();
    assert_eq!(listed["resultType"], "complete");
    assert_eq!(listed["cacheScope"], "public");
    assert!(listed["ttlMs"].as_u64().unwrap() > 0);
    assert_eq!(listed["tools"].as_array().unwrap().len(), 7);

    let called = server.request(
        "tools/call",
        json!({ "_meta": meta("2026-07-28"), "name": "dith_list", "arguments": { "kind": "generators" } }),
    )["result"]
        .clone();
    assert_eq!(called["resultType"], "complete");
    assert!(report(&called).as_array().unwrap().len() >= 8);

    let unsupported = server.request("tools/list", json!({ "_meta": meta("1999-01-01") }));
    assert_eq!(unsupported["error"]["code"], -32022);
    assert_eq!(unsupported["error"]["data"]["requested"], "1999-01-01");
    assert!(
        unsupported["error"]["data"]["supported"]
            .as_array()
            .unwrap()
            .contains(&json!("2026-07-28"))
    );

    assert!(server.shutdown().success());
}
