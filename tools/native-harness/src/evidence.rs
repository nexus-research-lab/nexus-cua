//! Native evidence aggregation and release-gate classification.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const CONTRACT_VERSION: &str = "nexus.cua.native-evidence.v2";
const MAX_REPORT_BYTES: u64 = 16 * 1024 * 1024;
const PERMISSIONS: [&str; 3] = ["screen_capture", "accessibility", "input_control"];
const COMMON_SCENARIOS: [&str; 18] = [
    "discovery_and_identity",
    "observation",
    "secure_redaction",
    "semantic_mutation",
    "foreground_mutation",
    "same_request_reconciliation",
    "stale_observation",
    "window_generation",
    "artifact_cleanup",
    "session_expiry",
    "minimized_window",
    "occluded_window",
    "multiple_display",
    "mixed_dpi",
    "negative_coordinates",
    "hung_accessibility_observation",
    "hung_accessibility_preflight",
    "hung_accessibility_after_dispatch",
];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, ValueEnum)]
#[value(rename_all = "snake_case")]
enum EvidenceClass {
    Diagnostic,
    EngineeringAccepted,
    ReleaseAccepted,
}

impl EvidenceClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Diagnostic => "diagnostic",
            Self::EngineeringAccepted => "engineering_accepted",
            Self::ReleaseAccepted => "release_accepted",
        }
    }

    const fn required_soak_profile(self) -> Option<&'static str> {
        match self {
            Self::Diagnostic => None,
            Self::EngineeringAccepted => Some("engineering"),
            Self::ReleaseAccepted => Some("release"),
        }
    }
}

#[derive(Debug, Args)]
pub(super) struct EvidenceArgs {
    /// Directory containing the raw JSON reports from one native runner.
    #[arg(long)]
    evidence_dir: PathBuf,
    /// Repository-pinned maintained-runner manifest used by the raw reports.
    #[arg(long)]
    runner_manifest: Option<PathBuf>,
    /// Exact source revision used to build the tested runtime.
    #[arg(long)]
    source_revision: String,
    /// SHA-256 of the tested release runtime executable.
    #[arg(long)]
    runtime_sha256: String,
    /// Evidence class that enforcement must validate without downgrade.
    #[arg(long, value_enum, default_value_t = EvidenceClass::Diagnostic)]
    required_class: EvidenceClass,
    /// Fail unless the requested evidence class is satisfied.
    #[arg(long)]
    enforce: bool,
}

struct RawReport {
    name: String,
    digest: FileDigest,
    value: Value,
}

#[derive(Clone, Debug, Serialize)]
struct FileDigest {
    sha256: String,
    size_bytes: u64,
}

pub(super) fn summarize(args: &EvidenceArgs) -> Result<(), Box<dyn Error>> {
    if args.enforce && args.required_class == EvidenceClass::Diagnostic {
        return Err(
            "--enforce requires --required-class engineering_accepted or release_accepted".into(),
        );
    }
    let reports = read_reports(&args.evidence_dir)?;
    let validation = reports
        .iter()
        .find(|report| report.name == "validation.json")
        .ok_or("native evidence is missing validation.json")?;
    let platform = validation.value["platform"]
        .as_str()
        .ok_or("validation.json is missing a platform")?;
    let architecture = validation.value["architecture"]
        .as_str()
        .ok_or("validation.json is missing an architecture")?;

    let mut base_issues = Vec::new();
    validate_report_context(&reports, platform, architecture, &mut base_issues);
    validate_hex(
        &args.source_revision,
        &[40, 64],
        "source revision",
        &mut base_issues,
    );
    validate_hex(
        &args.runtime_sha256,
        &[64],
        "runtime SHA-256",
        &mut base_issues,
    );
    let scenarios = merge_scenarios(&reports, &mut base_issues);
    require_common_scenarios(&scenarios, &mut base_issues);
    require_scenario(
        &scenarios,
        "same_request_mutation_reconciliation",
        "passed",
        &mut base_issues,
    );
    require_scenario(&scenarios, "sidecar_restart", "passed", &mut base_issues);

    let (runner, runner_manifest_sha256) = load_runner_manifest(
        args.runner_manifest.as_deref(),
        platform,
        architecture,
        &mut base_issues,
    )?;
    let mut engineering_issues = base_issues.clone();
    validate_performance(
        &reports,
        runner.as_ref(),
        "engineering",
        &mut engineering_issues,
    );
    let mut release_issues = engineering_issues.clone();
    require_accepted_report(
        &reports,
        "native_resource_soak",
        Some("release"),
        runner.as_ref(),
        &mut release_issues,
    );
    require_release_operator_probes(platform, &reports, &scenarios, &mut release_issues);

    let evidence_class = if release_issues.is_empty() {
        EvidenceClass::ReleaseAccepted
    } else if engineering_issues.is_empty() {
        EvidenceClass::EngineeringAccepted
    } else {
        EvidenceClass::Diagnostic
    };
    let class_requirement_satisfied = evidence_class >= args.required_class;
    let issues = match args.required_class {
        EvidenceClass::Diagnostic => base_issues.clone(),
        EvidenceClass::EngineeringAccepted => engineering_issues.clone(),
        EvidenceClass::ReleaseAccepted => release_issues.clone(),
    };
    let report_digests = reports
        .iter()
        .map(|report| (report.name.clone(), report.digest.clone()))
        .collect::<BTreeMap<_, _>>();
    let report = json!({
        "evidence_contract": CONTRACT_VERSION,
        "requested_evidence_class": args.required_class.as_str(),
        "evidence_class": evidence_class.as_str(),
        "class_requirement_satisfied": class_requirement_satisfied,
        "required_soak_profile": args.required_class.required_soak_profile(),
        "source_revision": args.source_revision,
        "runtime_sha256": args.runtime_sha256,
        "tested_payload_digests": {
            "runtime_sha256": args.runtime_sha256,
        },
        "platform": platform,
        "architecture": architecture,
        "runner": runner,
        "runner_manifest_sha256": runner_manifest_sha256,
        "scenario_groups": scenarios,
        "required_report_digests": report_digests,
        "class_evaluations": {
            "engineering_accepted": {
                "accepted": engineering_issues.is_empty(),
                "issues": engineering_issues,
            },
            "release_accepted": {
                "accepted": release_issues.is_empty(),
                "issues": release_issues,
            },
        },
        "issues": issues,
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    if args.enforce && !class_requirement_satisfied {
        return Err(format!(
            "native evidence did not satisfy requested class {}",
            args.required_class.as_str()
        )
        .into());
    }
    Ok(())
}

fn read_reports(directory: &Path) -> Result<Vec<RawReport>, Box<dyn Error>> {
    let mut paths = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter(|path| path.file_name().is_some_and(|name| name != "summary.json"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .ok_or("evidence report has no file name")?
                .to_string_lossy()
                .into_owned();
            let (value, digest) = read_json_with_digest(&path)?;
            Ok(RawReport {
                name,
                digest,
                value,
            })
        })
        .collect()
}

fn merge_scenarios(reports: &[RawReport], issues: &mut Vec<String>) -> BTreeMap<String, String> {
    let mut scenarios = BTreeMap::new();
    for report in reports {
        let Some(groups) = report.value["scenario_groups"].as_object() else {
            continue;
        };
        for (name, status) in groups {
            let Some(status) = status.as_str() else {
                issues.push(format!(
                    "{} scenario {name:?} has a non-string status",
                    report.name
                ));
                continue;
            };
            if let Some(previous) = scenarios.insert(name.clone(), status.to_owned())
                && previous != status
            {
                issues.push(format!(
                    "scenario {name:?} conflicts between {previous:?} and {status:?}"
                ));
            }
        }
    }
    scenarios
}

fn validate_report_context(
    reports: &[RawReport],
    platform: &str,
    architecture: &str,
    issues: &mut Vec<String>,
) {
    for report in reports {
        if let Some(report_platform) = report.value["platform"].as_str()
            && report_platform != platform
        {
            issues.push(format!(
                "{} platform {report_platform:?} does not match validation {platform:?}",
                report.name
            ));
        }
        if let Some(report_architecture) = report.value["architecture"].as_str()
            && report_architecture != architecture
        {
            issues.push(format!(
                "{} architecture {report_architecture:?} does not match validation {architecture:?}",
                report.name
            ));
        }
    }
}

fn require_common_scenarios(scenarios: &BTreeMap<String, String>, issues: &mut Vec<String>) {
    for scenario in COMMON_SCENARIOS {
        require_scenario(scenarios, scenario, "passed", issues);
    }
}

fn require_scenario(
    scenarios: &BTreeMap<String, String>,
    name: &str,
    expected: &str,
    issues: &mut Vec<String>,
) {
    match scenarios.get(name).map(String::as_str) {
        Some(actual) if actual == expected => {}
        Some(actual) => issues.push(format!(
            "scenario {name:?} is {actual:?}; expected {expected:?}"
        )),
        None => issues.push(format!("scenario {name:?} has no evidence")),
    }
}

fn require_release_operator_probes(
    platform: &str,
    reports: &[RawReport],
    scenarios: &BTreeMap<String, String>,
    issues: &mut Vec<String>,
) {
    match platform {
        "macos" => {
            require_permission_coverage(reports, "denied", "passed", issues);
            require_permission_coverage(reports, "revoke_verify", "passed", issues);
            require_scenario(scenarios, "permission_denied", "passed", issues);
            require_scenario(scenarios, "permission_revoked", "passed", issues);
            require_scenario(
                scenarios,
                "elevated_or_protected_target",
                "not_applicable",
                issues,
            );
        }
        "windows" => {
            require_permission_coverage(reports, "denied", "not_applicable", issues);
            require_scenario(scenarios, "permission_denied", "not_applicable", issues);
            require_scenario(scenarios, "permission_revoked", "not_applicable", issues);
            require_scenario(scenarios, "elevated_or_protected_target", "passed", issues);
        }
        other => issues.push(format!("unsupported native evidence platform {other:?}")),
    }
}

fn require_permission_coverage(
    reports: &[RawReport],
    mode: &str,
    expected_status: &str,
    issues: &mut Vec<String>,
) {
    let covered = reports
        .iter()
        .filter(|report| report.value["mode"] == mode)
        .filter(|report| report.value["status"] == expected_status)
        .filter_map(|report| report.value["permission"].as_str())
        .collect::<BTreeSet<_>>();
    for permission in PERMISSIONS {
        if !covered.contains(permission) {
            issues.push(format!(
                "permission {permission:?} has no {mode:?}/{expected_status:?} evidence"
            ));
        }
    }
}

fn load_runner_manifest(
    path: Option<&Path>,
    platform: &str,
    architecture: &str,
    issues: &mut Vec<String>,
) -> Result<(Option<Value>, Option<String>), Box<dyn Error>> {
    let Some(path) = path else {
        issues.push("maintained-runner manifest is missing".to_owned());
        return Ok((None, None));
    };
    let (value, manifest_digest) = read_json_with_digest(path)?;
    let digest = manifest_digest.sha256;
    for field in [
        "runner_id",
        "status",
        "platform",
        "architecture",
        "cpu",
        "memory_bytes",
        "gpu",
        "display_topology",
        "power_mode",
        "os_build",
        "toolchain",
    ] {
        if value.get(field).is_none_or(Value::is_null) {
            issues.push(format!("runner manifest is missing field {field:?}"));
        }
    }
    if value["status"] != "active" {
        issues.push("runner manifest status is not active".to_owned());
    }
    if value["platform"] != platform {
        issues.push("runner manifest platform does not match validation".to_owned());
    }
    if value["architecture"] != architecture {
        issues.push("runner manifest architecture does not match validation".to_owned());
    }
    Ok((Some(value), Some(digest)))
}

fn validate_performance(
    reports: &[RawReport],
    runner: Option<&Value>,
    active_profile: &str,
    issues: &mut Vec<String>,
) {
    require_accepted_report(reports, "native_performance", None, runner, issues);
    require_accepted_report(
        reports,
        "native_resource_soak",
        Some("idle"),
        runner,
        issues,
    );
    require_accepted_report(
        reports,
        "native_resource_soak",
        Some(active_profile),
        runner,
        issues,
    );
}

fn require_accepted_report(
    reports: &[RawReport],
    kind: &str,
    profile: Option<&str>,
    runner: Option<&Value>,
    issues: &mut Vec<String>,
) {
    let label = profile.map_or_else(|| kind.to_owned(), |profile| format!("{kind}/{profile}"));
    let matching = reports
        .iter()
        .filter(|report| {
            report.value["evidence_kind"] == kind
                && profile.is_none_or(|profile| report.value["profile"] == profile)
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        issues.push(format!(
            "{label} must have exactly one report; found {}",
            matching.len()
        ));
        return;
    }
    let report = matching[0];
    if report.value["evidence_accepted"] != true {
        issues.push(format!("{label} report is not accepted"));
        return;
    }
    if let Some(runner) = runner {
        if report.value["runner"]["runner_id"] != runner["runner_id"] {
            issues.push(format!("{label} report used a different maintained runner"));
        } else if &report.value["runner"] != runner {
            issues.push(format!(
                "{label} report runner metadata differs from the pinned manifest"
            ));
        }
    }
}

fn validate_hex(value: &str, lengths: &[usize], label: &str, issues: &mut Vec<String>) {
    if !lengths.contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        issues.push(format!(
            "{label} is not a valid lowercase or uppercase hexadecimal value"
        ));
    }
}

fn read_json_with_digest(path: &Path) -> Result<(Value, FileDigest), Box<dyn Error>> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(format!("evidence input is not a regular file: {}", path.display()).into());
    }
    if metadata.len() > MAX_REPORT_BYTES {
        return Err(format!("evidence input exceeds {MAX_REPORT_BYTES} bytes").into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::fs::File::open(path)?
        .take(MAX_REPORT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_REPORT_BYTES {
        return Err(format!("evidence input exceeds {MAX_REPORT_BYTES} bytes").into());
    }
    let digest = FileDigest {
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        size_bytes: bytes.len() as u64,
    };
    Ok((parse_json_bytes(&bytes)?, digest))
}

fn parse_json_bytes(bytes: &[u8]) -> Result<Value, Box<dyn Error>> {
    if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return parse_utf16(bytes, u16::from_le_bytes);
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return parse_utf16(bytes, u16::from_be_bytes);
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    Ok(serde_json::from_slice(bytes)?)
}

fn parse_utf16(bytes: &[u8], decode: fn([u8; 2]) -> u16) -> Result<Value, Box<dyn Error>> {
    if !bytes.len().is_multiple_of(2) {
        return Err("UTF-16 evidence JSON has an odd byte length".into());
    }
    let words = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| decode(*pair))
        .collect::<Vec<_>>();
    let text = String::from_utf16(&words)?;
    Ok(serde_json::from_str(&text)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_report(name: &str, value: Value) -> RawReport {
        RawReport {
            name: name.to_owned(),
            digest: FileDigest {
                sha256: "0".repeat(64),
                size_bytes: 0,
            },
            value,
        }
    }

    #[test]
    fn conflicting_scenario_evidence_is_rejected() {
        let reports = vec![
            raw_report(
                "first.json",
                json!({"scenario_groups": {"observation": "passed"}}),
            ),
            raw_report(
                "second.json",
                json!({"scenario_groups": {"observation": "failed"}}),
            ),
        ];
        let mut issues = Vec::new();

        let scenarios = merge_scenarios(&reports, &mut issues);

        assert_eq!(scenarios["observation"], "failed");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn permission_coverage_requires_each_os_boundary() {
        let reports = vec![raw_report(
            "screen.json",
            json!({
                "mode": "denied",
                "status": "passed",
                "permission": "screen_capture"
            }),
        )];
        let mut issues = Vec::new();

        require_permission_coverage(&reports, "denied", "passed", &mut issues);

        assert_eq!(issues.len(), 2);
    }

    #[test]
    fn report_context_rejects_mixed_platform_evidence() {
        let reports = vec![raw_report(
            "benchmark.json",
            json!({"platform": "windows", "architecture": "aarch64"}),
        )];
        let mut issues = Vec::new();

        validate_report_context(&reports, "macos", "aarch64", &mut issues);

        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn accepted_performance_requires_one_report_per_engineering_gate() {
        let runner = json!({"runner_id": "maintained-runner"});
        let mut reports = vec![
            raw_report(
                "benchmark.json",
                json!({
                    "evidence_kind": "native_performance",
                    "evidence_accepted": true,
                    "runner": runner,
                }),
            ),
            raw_report(
                "soak-idle.json",
                json!({
                    "evidence_kind": "native_resource_soak",
                    "profile": "idle",
                    "evidence_accepted": true,
                    "runner": runner,
                }),
            ),
            raw_report(
                "soak-engineering.json",
                json!({
                    "evidence_kind": "native_resource_soak",
                    "profile": "engineering",
                    "evidence_accepted": true,
                    "runner": runner,
                }),
            ),
        ];
        let mut issues = Vec::new();

        validate_performance(&reports, Some(&runner), "engineering", &mut issues);
        assert!(issues.is_empty());

        reports.push(raw_report(
            "stale-engineering.json",
            json!({
                "evidence_kind": "native_resource_soak",
                "profile": "engineering",
                "evidence_accepted": true,
                "runner": runner,
            }),
        ));
        validate_performance(&reports, Some(&runner), "engineering", &mut issues);
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("must have exactly one report"))
        );
    }

    #[test]
    fn parses_windows_powershell_utf16_json() {
        let path = std::env::temp_dir().join(format!(
            "nexus-cua-evidence-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let mut bytes = vec![0xff, 0xfe];
        for word in r#"{"status":"passed"}"#.encode_utf16() {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        std::fs::write(&path, bytes).unwrap();

        let (value, _) = read_json_with_digest(&path).unwrap();

        assert_eq!(value["status"], "passed");
        std::fs::remove_file(path).unwrap();
    }
}
