//! Bounded installed-application discovery through standard macOS bundle roots.

use std::collections::VecDeque;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use nexus_cua_protocol::ApplicationProvenance;
use nexus_cua_runtime::{DriverError, DriverErrorKind, DriverLaunchableApplication};
use objc2_foundation::{NSBundle, NSString, NSURL};

use super::provenance;

const MAX_SCANNED_ENTRIES: usize = 16_384;
const MAX_DIRECTORY_DEPTH: usize = 3;

pub(super) fn discover(
    cache: &mut provenance::Cache,
) -> Result<Vec<DriverLaunchableApplication>, DriverError> {
    let mut roots = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    let roots = roots
        .into_iter()
        .filter_map(|root| root.canonicalize().ok())
        .collect::<Vec<_>>();
    let mut queue = roots
        .iter()
        .cloned()
        .map(|root| (root, 0_usize))
        .collect::<VecDeque<_>>();
    let mut scanned = 0_usize;
    let mut applications = Vec::new();
    while let Some((directory, depth)) = queue.pop_front() {
        let entries = fs::read_dir(&directory).map_err(|_| discovery_failure())?;
        for entry in entries {
            scanned = scanned.saturating_add(1);
            if scanned > MAX_SCANNED_ENTRIES {
                return Err(DriverError::new(
                    DriverErrorKind::Busy,
                    "macOS installed application discovery exceeded its entry bound",
                )
                .retryable("narrow_installed_application_roots"));
            }
            let entry = entry.map_err(|_| discovery_failure())?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(|_| discovery_failure())?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            let path = entry.path();
            if is_application_bundle(&path) {
                if let Some(application) = inspect_bundle(&path, &roots, cache) {
                    applications.push(application);
                }
            } else if depth < MAX_DIRECTORY_DEPTH {
                queue.push_back((path, depth + 1));
            }
        }
    }
    Ok(applications)
}

fn inspect_bundle(
    path: &Path,
    roots: &[PathBuf],
    cache: &mut provenance::Cache,
) -> Option<DriverLaunchableApplication> {
    let bundle_path = path.canonicalize().ok()?;
    if !roots.iter().any(|root| bundle_path.starts_with(root)) {
        return None;
    }
    let bundle_text = bundle_path.to_str()?;
    let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(bundle_text), true);
    let bundle = NSBundle::bundleWithURL(&url)?;
    let bundle_id = bundle.bundleIdentifier()?.to_string();
    if bundle_id.is_empty() {
        return None;
    }
    let executable_path = bundle.executableURL()?.path()?.to_string();
    let executable = Path::new(&executable_path).canonicalize().ok()?;
    if !executable.starts_with(&bundle_path) {
        return None;
    }
    let executable_text = executable.to_str()?.to_owned();
    let metadata = executable.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    let key = format!(
        "bundle:{bundle_id}:dev:{:x}:ino:{:x}:mtime:{}:{}:len:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.len()
    );
    let signing = cache.inspect(&key, &executable_text);
    let name = bundle_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(&bundle_id)
        .to_owned();
    Some(DriverLaunchableApplication {
        key,
        launch_target: bundle_text.to_owned(),
        identity: format!("bundle:{bundle_id}:executable:{executable_text}"),
        name,
        application_id: bundle_id.clone(),
        provenance: ApplicationProvenance::Macos {
            bundle_id: Some(bundle_id),
            executable_path: Some(executable_text),
            signing_team_id: signing.team_id,
            designated_requirement: signing.designated_requirement,
        },
    })
}

fn is_application_bundle(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("app"))
}

fn discovery_failure() -> DriverError {
    DriverError::new(
        DriverErrorKind::Platform,
        "macOS installed application discovery failed",
    )
    .retryable("retry_installed_application_discovery")
}
