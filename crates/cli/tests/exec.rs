use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sw-exec-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn secondwind(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_secondwind"));
    cmd.arg("--home").arg(home).env("HOME", home);
    cmd
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn argv_mode_passes_each_argument_verbatim() {
    let home = home("argv");
    let out = secondwind(&home)
        .args(["exec", "--", "printf", "%s|%s", "a b", "c|d"])
        .output()
        .unwrap();
    assert_eq!(stdout(&out), "a b|c|d");
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn script_mode_runs_a_compound_command_through_one_shell() {
    let home = home("script");
    let out = secondwind(&home)
        .args(["exec", "-c", "echo 'a|b' && printf x"])
        .output()
        .unwrap();
    assert_eq!(stdout(&out), "a|b\nx");
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn the_childs_exit_code_and_stderr_pass_through() {
    let home = home("exit");
    let out = secondwind(&home)
        .args(["exec", "-c", "echo oops >&2; exit 42"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(42));
    assert!(stderr(&out).starts_with("oops\n"), "{}", stderr(&out));

    let out = secondwind(&home)
        .args(["exec", "--", "sh", "-c", "exit 3"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn a_missing_program_exits_127() {
    let home = home("missing");
    let out = secondwind(&home)
        .args(["exec", "--", "sw-no-such-program"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(127));
}

#[test]
fn the_retired_hook_leaves_every_command_to_the_permission_check() {
    let home = home("hook");
    let event =
        serde_json::json!({ "tool_name": "Bash", "tool_input": { "command": "rm -rf build" } });
    let mut hook = secondwind(&home)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    hook.stdin
        .take()
        .unwrap()
        .write_all(event.to_string().as_bytes())
        .unwrap();
    let out = hook.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(
        out.stdout.is_empty(),
        "no rewrite and no permission decision"
    );
}
