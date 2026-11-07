//! `orch mcp` through the real binary, over a stdio pipe.
//!
//! Black-box on purpose: the client in `mcp_common` is hand-rolled JSON-RPC
//! rather than `rmcp`, so these tests check what an arbitrary MCP client would
//! see — stdout carries protocol frames and nothing else, the lifecycle ends
//! cleanly, and an uninitialized project is refused before any frame is
//! written. They are written first and fail until `orch mcp` exists.
//!
//! Spec rows: R1 (handshake, stdout/stderr discipline, exit 3), R4 (tool
//! annotations), R8 (clean exit on EOF).

mod mcp_common;

use mcp_common::{McpServer, empty_project, initialized_project, orch_command};
use serde_json::{Value, json};

/// Tool names the catalogue must contain (spec R2). The design fills this in;
/// until then the conformance test below is `#[ignore]`d.
const EXPECTED: &[&str] = &[];

fn tool_names(tools: &Value) -> Vec<String> {
    tools["tools"]
        .as_array()
        .expect("`tools` is an array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name").to_owned())
        .collect()
}

#[test]
fn initialize_handshake_names_the_server_orch_and_advertises_tools() {
    let project = initialized_project();
    let mut server = McpServer::spawn(project.path());

    let result = server.initialize();

    assert_eq!(result["serverInfo"]["name"], json!("orch"));
    assert!(
        result["capabilities"]["tools"].is_object(),
        "capabilities.tools missing: {result}"
    );
}

#[test]
fn stdout_carries_only_json_rpc_frames() {
    let project = initialized_project();
    let mut server = McpServer::spawn(project.path());
    server.initialize();
    server.expect_result("tools/list", json!({}));

    let exit = server.shutdown();

    assert!(
        !exit.stdout_lines.is_empty(),
        "the server wrote no frames at all; stderr: {}",
        exit.stderr
    );
    for line in &exit.stdout_lines {
        let frame: Value = serde_json::from_str(line)
            .unwrap_or_else(|err| panic!("stdout line is not JSON ({err}): {line:?}"));
        assert_eq!(
            frame["jsonrpc"],
            json!("2.0"),
            "stdout line is JSON but not a JSON-RPC frame: {line:?}"
        );
    }
}

#[test]
fn tools_list_returns_annotated_tools_with_input_schemas() {
    let project = initialized_project();
    let mut server = McpServer::spawn(project.path());
    server.initialize();

    let result = server.expect_result("tools/list", json!({}));

    let tools = result["tools"].as_array().expect("`tools` is an array");
    assert!(!tools.is_empty(), "tools/list returned no tools");
    for tool in tools {
        let name = &tool["name"];
        assert!(
            tool["inputSchema"].is_object(),
            "tool {name} has no inputSchema: {tool}"
        );
        assert!(
            tool["annotations"].is_object(),
            "tool {name} has no annotations: {tool}"
        );
    }
}

#[test]
fn uninitialized_project_exits_three_with_nothing_on_stdout() {
    let dir = empty_project();
    let server = McpServer::spawn(dir.path());

    // No stdin traffic: a project with no `.orchestry` is refused up front,
    // before the server reads a single request.
    let exit = server.wait_for_exit();

    assert_eq!(exit.status.code(), Some(3), "stderr: {}", exit.stderr);
    assert!(
        exit.stdout_lines.is_empty(),
        "stdout must stay empty, got {:?}",
        exit.stdout_lines
    );
}

#[test]
fn mcp_subcommand_is_registered_and_documents_itself() {
    let project = initialized_project();

    let output = orch_command(project.path())
        .args(["mcp", "--help"])
        .output()
        .expect("run `orch mcp --help`");

    assert!(
        output.status.success(),
        "`orch mcp --help` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn stdin_eof_after_initialized_exits_zero() {
    let project = initialized_project();
    let mut server = McpServer::spawn(project.path());
    server.initialize();

    let exit = server.shutdown();

    assert_eq!(exit.status.code(), Some(0), "stderr: {}", exit.stderr);
}

#[test]
#[ignore = "B1: fill EXPECTED from the design's tool catalogue, then un-ignore"]
fn tools_list_contains_every_expected_tool() {
    assert!(
        !EXPECTED.is_empty(),
        "EXPECTED is still the placeholder; the design must fill the catalogue"
    );
    let project = initialized_project();
    let mut server = McpServer::spawn(project.path());
    server.initialize();

    let names = tool_names(&server.expect_result("tools/list", json!({})));

    let missing: Vec<&&str> = EXPECTED
        .iter()
        .filter(|expected| !names.iter().any(|name| name == **expected))
        .collect();
    assert!(missing.is_empty(), "tools/list is missing {missing:?}");
}
