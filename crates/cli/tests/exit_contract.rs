//! Process-level checks for the public failure boundary without desktop authority.

use std::io::Write as _;
use std::process::{Command, Stdio};

#[test]
fn invalid_agent_input_returns_json_failure_without_echoing_the_payload() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_nexuscua"))
        .args([
            "action",
            "perform_action",
            "--registration",
            "unused",
            "--input-stdin",
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"[\"sensitive-test-value\"]")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(report["schema"], "nexus.cua.cli-error.v1");
    assert_eq!(report["code"], "invalid_input");
    assert_eq!(report["exit_code"], 2);
    assert!(report["mutation_status"].is_null());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("sensitive-test-value"));
}

#[test]
fn version_and_help_preserve_success_and_machine_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_nexuscua"))
        .args(["version", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(serde_json::from_slice::<serde_json::Value>(&output.stdout).is_ok());
    let output = Command::new(env!("CARGO_BIN_EXE_nexuscua"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
}
