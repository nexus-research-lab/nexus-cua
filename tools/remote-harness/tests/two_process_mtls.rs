//! Executable conformance check for the two-process remote transport harness.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use uuid::Uuid;

struct TestRoot(PathBuf);

impl TestRoot {
    fn create() -> Self {
        let path = std::env::temp_dir().join(format!(
            "nexus-cua-remote-harness-test-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn emits_sanitized_machine_verifiable_evidence() {
    let root = TestRoot::create();
    let output = root.path().join("evidence.json");
    let status = Command::new(env!("CARGO_BIN_EXE_nexus-cua-remote-harness"))
        .arg("run")
        .arg("--source-revision")
        .arg("a".repeat(40))
        .arg("--output")
        .arg(&output)
        .arg("--max-frame-bytes")
        .arg("65536")
        .status()
        .expect("run remote harness");
    assert!(status.success());

    let bytes = fs::read(&output).expect("read evidence");
    let evidence: Value = serde_json::from_slice(&bytes).expect("parse evidence");
    assert_eq!(evidence["schema_version"], "nexus.cua.remote-transport.v1");
    assert_eq!(evidence["source_revision"], "a".repeat(40));
    assert_eq!(evidence["process_count"], 2);
    assert_eq!(evidence["accepted"], true);
    assert_eq!(
        evidence["controller_binary_sha256"],
        evidence["worker_binary_sha256"]
    );
    assert_eq!(
        evidence["scenarios"]
            .as_array()
            .expect("scenario array")
            .len(),
        8
    );
    assert!(
        evidence["scenarios"]
            .as_array()
            .expect("scenario array")
            .iter()
            .all(|entry| entry["accepted"] == true
                && entry["disposition"]
                    .as_str()
                    .is_some_and(|value| !value.is_empty()))
    );
    assert!(
        evidence["issues"]
            .as_array()
            .expect("issues array")
            .is_empty()
    );

    let text = String::from_utf8(bytes).expect("UTF-8 evidence");
    for forbidden in [
        "authorization",
        "token",
        "/private/worker",
        "fixture-remote-observation",
        root.path().to_string_lossy().as_ref(),
    ] {
        assert!(!text.contains(forbidden), "evidence leaked {forbidden}");
    }
}
