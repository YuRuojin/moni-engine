//! End-to-end test for `moni mcp` over real stdio: initialize,
//! notifications/initialized, tools/list, and tools/call against a spawned
//! `moni mcp` child process. Lives as an integration test so cargo builds
//! and refreshes the binary before running it (`CARGO_BIN_EXE_*`).

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The first-batch tool surface this server must expose. Duplicated from
/// `src/mcp.rs` on purpose: this pins the public tool list end to end.
const TOOL_NAMES: [&str; 14] = [
    "capabilities",
    "scene_info",
    "scene_load",
    "scene_save",
    "entity_spawn",
    "entity_despawn",
    "entity_set_transform",
    "component_attach",
    "component_remove",
    "step",
    "render_svg",
    "screenshot",
    "run_client",
    "diagnostics",
];

#[test]
fn mcp_stdio_server_handshake_lists_and_calls_tools() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_moni-engine"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn moni mcp");
    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    let (line_tx, line_rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        while let Ok(bytes) = reader.read_line(&mut line) {
            if bytes == 0 {
                break;
            }
            if line_tx.send(line.clone()).is_err() {
                break;
            }
            line.clear();
        }
    });

    let mut send = |message: Value| {
        writeln!(stdin, "{message}").expect("write json-rpc line");
        stdin.flush().expect("flush json-rpc line");
    };
    let read_response = |expected_id: i64| -> Value {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for json-rpc response {expected_id}"
            );
            let line = line_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("server response line");
            let message: Value = serde_json::from_str(&line).expect("response is json");
            if message.get("id").and_then(Value::as_i64) == Some(expected_id) {
                return message;
            }
        }
    };

    send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "moni-mcp-test", "version": "0.0.0" },
        },
    }));
    let initialize = read_response(1);
    assert_eq!(initialize["error"], json!(null), "initialize succeeds");
    assert_eq!(
        initialize["result"]["serverInfo"]["name"],
        json!("moni-engine")
    );
    assert!(
        initialize["result"]["capabilities"]["tools"].is_object(),
        "tools capability is advertised"
    );

    send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    send(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let list = read_response(2);
    let tools = list["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    for expected in TOOL_NAMES {
        assert!(
            names.contains(&expected),
            "tools/list contains '{expected}'"
        );
    }

    send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": { "name": "capabilities", "arguments": {} },
    }));
    let call = read_response(3);
    assert_eq!(call["result"]["isError"], json!(false), "{call}");
    let structured = &call["result"]["structuredContent"];
    assert_eq!(structured["ok"], json!(true));
    assert_eq!(
        structured["result"]["protocol"],
        json!("moni-engine-control/1")
    );
    assert_eq!(structured["result"]["session"]["profile"], json!("serve"));

    send(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "entity_spawn",
            "arguments": { "spec": { "name": "e2e-box" } },
        },
    }));
    let spawn = read_response(4);
    assert_eq!(spawn["result"]["isError"], json!(false), "{spawn}");
    assert_eq!(
        spawn["result"]["structuredContent"]["result"]["entity"],
        json!(1)
    );
    assert_eq!(
        spawn["result"]["structuredContent"]["revision"],
        json!(1),
        "the protocol response envelope stays intact over MCP"
    );

    send(json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "entity_spawn",
            "arguments": { "spec": { "name": "late" }, "expectedRevision": 999 },
        },
    }));
    let conflict = read_response(5);
    assert_eq!(conflict["result"]["isError"], json!(true), "{conflict}");
    assert_eq!(
        conflict["result"]["structuredContent"]["error"]["code"],
        json!("revision_conflict"),
        "protocol errors surface with their structured code"
    );

    send(json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "tools/call",
        "params": { "name": "scene_info", "arguments": {} },
    }));
    let info = read_response(6);
    assert_eq!(
        info["result"]["structuredContent"]["result"]["entityCount"],
        json!(1)
    );

    // Closing stdin ends the server with a clean EOF quit.
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait().expect("poll child") {
            Some(status) => {
                assert!(status.success(), "the server quit cleanly: {status}");
                break;
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the mcp server did not exit after stdin EOF");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}
