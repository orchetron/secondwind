use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sw-gate-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// A body the optimizer offloads as a code outline when nothing stops it.
fn code() -> String {
    (0..200)
        .map(|i| {
            format!(
                "fn handler_{i}(input: &str) -> Result<usize> {{ let key = uniquetoken{i} ; parse(input).map(|v| v * {i}) }}\n"
            )
        })
        .collect()
}

fn optimize(home: &Path, arguments: Value) -> String {
    let call = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": "optimize", "arguments": arguments } });
    let mut server = Command::new(env!("CARGO_BIN_EXE_secondwind"))
        .arg("--home")
        .arg(home)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    server
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{call}\n").as_bytes())
        .unwrap();
    let out = server.wait_with_output().unwrap();
    let response: Value = serde_json::from_slice(&out.stdout).unwrap();
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn a_large_cat_stays_in_view_while_the_same_block_unnamed_is_offloaded() {
    let home = home("mcp");
    let unnamed = optimize(&home, json!({ "block": code() }));
    assert!(
        unnamed.contains("<<swload:"),
        "offloaded: {}",
        &unnamed[..80]
    );

    let cat = optimize(
        &home,
        json!({ "block": code(), "tool": "Bash", "command": "cat src/handlers.rs" }),
    );
    assert!(!cat.contains("<<swload:"), "a whole read is not hidden");
    assert!(cat.contains("uniquetoken137"), "every line stays in view");
}

#[test]
fn exec_keeps_a_large_cat_in_view() {
    let home = home("exec");
    let file = home.join("handlers.rs");
    std::fs::write(&file, code()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_secondwind"))
        .arg("--home")
        .arg(&home)
        .args(["exec", "--", "cat"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("<<swload:"));
    assert!(stdout.contains("uniquetoken137"));
}

#[test]
fn a_learned_policy_replaces_the_built_in_one() {
    let home = home("learned");
    let policy = json!({ "version": 1, "threshold": 0.4, "prior": 0.05, "classes": {
        "Bash:cat": { "rate": 0.05, "n": 1000, "sizes": {} } } });
    std::fs::create_dir_all(home.join(".secondwind")).unwrap();
    std::fs::write(home.join(".secondwind/policy.json"), policy.to_string()).unwrap();

    let cat = optimize(
        &home,
        json!({ "block": code(), "tool": "Bash", "command": "cat src/handlers.rs" }),
    );
    assert!(
        cat.contains("<<swload:"),
        "this user's cats are rarely reopened"
    );
}

#[test]
fn eval_learn_writes_a_policy_and_reports_held_out_sessions() {
    let home = home("learn");
    let projects = home.join(".claude/projects/demo");
    std::fs::create_dir_all(&projects).unwrap();
    for n in 0..4 {
        let session = [
            json!({ "type": "assistant", "sessionId": format!("s-{n}"), "timestamp": "2026-10-23T10:00:00Z",
                    "isSidechain": false, "message": { "role": "assistant", "content": [
                        { "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "cat a.rs" } } ] } }),
            json!({ "type": "user", "sessionId": format!("s-{n}"), "timestamp": "2026-10-23T10:00:01Z",
                    "isSidechain": false, "message": { "role": "user", "content": [
                        { "type": "tool_result", "tool_use_id": "t1", "content": code() } ] } }),
            json!({ "type": "assistant", "sessionId": format!("s-{n}"), "timestamp": "2026-10-23T10:00:02Z",
                    "isSidechain": false, "message": { "role": "assistant", "content": [
                        { "type": "text", "text": "uniquetoken137 is the key" } ] } }),
        ];
        let lines: Vec<String> = session.iter().map(Value::to_string).collect();
        std::fs::write(projects.join(format!("s-{n}.jsonl")), lines.join("\n")).unwrap();
    }

    let out = Command::new(env!("CARGO_BIN_EXE_secondwind"))
        .arg("--home")
        .arg(&home)
        .args(["eval", "--learn"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    let held = &report["held_out"];
    assert_eq!(held["without_gate"]["reopened"], 2);
    assert_eq!(held["with_gate"]["unfaithful"], 0);
    assert!(home.join(".secondwind/policy.json").exists());
}
