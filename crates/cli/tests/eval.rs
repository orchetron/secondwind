use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};

fn home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sw-eval-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".claude").join("projects").join("demo")).unwrap();
    dir
}

fn record(kind: &str, content: Value) -> String {
    json!({
        "type": kind,
        "sessionId": "s-1",
        "timestamp": "2026-10-23T10:00:00Z",
        "isSidechain": false,
        "message": { "role": kind, "content": content },
    })
    .to_string()
}

fn code() -> String {
    (0..200)
        .map(|i| {
            format!(
                "fn handler_{i}(input: &str) -> Result<usize> {{ let key = uniquetoken{i} ; parse(input).map(|v| v * {i}) }}\n"
            )
        })
        .collect()
}

#[test]
fn eval_reports_reopens_per_command_and_every_block_faithful() {
    let home = home("report");
    let session = [
        record("user", json!("look at the handlers")),
        record(
            "assistant",
            json!([{ "type": "tool_use", "id": "t1", "name": "Bash",
                     "input": { "command": "cd repo && cat src/handlers.rs" } }]),
        ),
        record(
            "user",
            json!([{ "type": "tool_result", "tool_use_id": "t1", "content": [
                { "type": "text", "text": code() },
                { "type": "image", "source": { "type": "base64", "data": "A".repeat(200_000) } }
            ] }]),
        ),
        record(
            "assistant",
            json!([{ "type": "text", "text": "handler 137 reads uniquetoken137" },
                   { "type": "tool_use", "id": "t2", "name": "Bash", "input": { "command": "ls" } }]),
        ),
        record(
            "user",
            json!([{ "type": "tool_result", "tool_use_id": "t2", "content": "Cargo.toml\nsrc\n" }]),
        ),
    ];
    std::fs::write(
        home.join(".claude/projects/demo/s-1.jsonl"),
        session.join("\n"),
    )
    .unwrap();
    let blocks = home.join("blocks.jsonl");

    let out = Command::new(env!("CARGO_BIN_EXE_secondwind"))
        .arg("--home")
        .arg(&home)
        .args(["eval", "--blocks"])
        .arg(&blocks)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();

    assert_eq!(report["files_read"], 1);
    assert_eq!(report["total"]["blocks"], 2);
    assert_eq!(report["total"]["unfaithful"], 0);
    let cat = report["by_class"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["class"] == "Bash:cat")
        .expect("the cat result is classed by its program");
    assert_eq!(cat["offloaded"], 1);
    assert_eq!(cat["reopened"], 1);
    assert!(
        cat["raw_tokens"].as_u64().unwrap() < 20_000,
        "the image counts as a placeholder, not its base64"
    );

    let lines: Vec<Value> = std::fs::read_to_string(&blocks)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|line| line["faithful"] == true));
}
