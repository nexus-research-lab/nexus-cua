//! Registered authority provider conformance tests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use nexus_cua_mcp::{
    AuthorityChallenge, AuthorityPolicy, AuthorityProvider as _, AuthorityProviderError,
};
use nexus_cua_platform::{HostExecutableIdentity, HostSignatureIdentity};
use nexus_cua_protocol::{ActionKind, PermissionMode};
use nexus_cua_provider::{
    AuthenticatedIntegrationStore, HostIdentitySource, McpRegisteredTransport,
    RegisteredAdapterProfile, RegisteredAuthorityProvider, RegistrationError, RegistrationRequest,
    RegistrationStore, SecretStore, SecretStoreError,
};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Default)]
struct MemorySecrets(Mutex<HashMap<String, Vec<u8>>>);

impl SecretStore for MemorySecrets {
    fn put(&self, registration_id: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
        let mut values = self.0.lock().expect("secret lock");
        if values.contains_key(registration_id) {
            return Err(SecretStoreError::AlreadyExists);
        }
        values.insert(registration_id.to_owned(), secret.to_vec());
        Ok(())
    }

    fn get(&self, registration_id: &str) -> Result<Zeroizing<Vec<u8>>, SecretStoreError> {
        self.0
            .lock()
            .expect("secret lock")
            .get(registration_id)
            .cloned()
            .map(Zeroizing::new)
            .ok_or(SecretStoreError::NotFound)
    }

    fn delete(&self, registration_id: &str) -> Result<(), SecretStoreError> {
        self.0.lock().expect("secret lock").remove(registration_id);
        Ok(())
    }
}

struct FixedHost(HostExecutableIdentity);

impl HostIdentitySource for FixedHost {
    fn current(&self) -> Result<HostExecutableIdentity, RegistrationError> {
        Ok(self.0.clone())
    }
}

fn host(byte: char) -> HostExecutableIdentity {
    HostExecutableIdentity {
        path_sha256: byte.to_string().repeat(64),
        executable_sha256: byte.to_ascii_uppercase().to_string().repeat(64),
        signature: HostSignatureIdentity::Macos {
            signing_team_id: Some("TEAM123456".to_owned()),
            designated_requirement: Some("identifier com.example.host".to_owned()),
        },
    }
}

fn request(registration_id: &str, host_identity: HostExecutableIdentity) -> RegistrationRequest {
    RegistrationRequest {
        registration_id: registration_id.to_owned(),
        provider_id: "local-provider-v1".to_owned(),
        product_version: "0.1.0".to_owned(),
        adapter_profile: RegisteredAdapterProfile::Mcp {
            transport: McpRegisteredTransport::Stdio,
            tool_profile: "full".to_owned(),
        },
        host_identity,
        principal: "local-user".to_owned(),
        work_scope: "fixture-acceptance".to_owned(),
        authority: AuthorityPolicy {
            schema_version: 1,
            mode: PermissionMode::Bounded,
            application_ids: vec!["com.example.Fixture".to_owned()],
            allowed_actions: vec![ActionKind::ClickPoint],
            allow_foreground_input: true,
            restore_foreground_after_input: false,
            allow_display_capture: false,
            allow_clipboard_read: false,
            allow_clipboard_write: false,
            allow_application_launch: true,
            allow_application_launch_activation: true,
            ttl_seconds: 60,
        },
    }
}

fn test_root() -> PathBuf {
    std::env::temp_dir().join(format!("nexuscua-provider-test-{}", Uuid::new_v4()))
}

#[test]
fn registration_store_rejects_a_symlink_state_root() {
    #[cfg(unix)]
    {
        let root = test_root();
        let real = root.join("real");
        let link = root.join("link");
        std::fs::create_dir_all(&real).expect("real root");
        std::os::unix::fs::symlink(&real, &link).expect("state symlink");
        assert!(
            RegistrationStore::with_secret_store(&link, Arc::new(MemorySecrets::default()))
                .is_err()
        );
        std::fs::remove_dir_all(root).expect("remove test root");
    }
}

#[test]
fn integration_plan_store_authenticates_payload_and_rejects_tampering() {
    let root = test_root();
    let plan_ref = format!("plan_{}", Uuid::new_v4().simple());
    let store =
        AuthenticatedIntegrationStore::with_secret_store(&root, Arc::new(MemorySecrets::default()))
            .expect("plan store");
    store
        .install(&plan_ref, br#"{"profile":"mcp"}"#)
        .expect("install plan");
    assert_eq!(
        store.load(&plan_ref).expect("load plan").as_slice(),
        br#"{"profile":"mcp"}"#
    );
    assert!(matches!(
        store.install(&plan_ref, b"duplicate"),
        Err(RegistrationError::AlreadyExists)
    ));

    let path = root.join(format!("{plan_ref}.json"));
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("plan bytes")).expect("plan envelope");
    envelope["payload_hex"] = serde_json::Value::String(hex::encode(b"tampered"));
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&envelope).expect("tampered envelope"),
    )
    .expect("tamper plan");
    assert!(matches!(
        store.load(&plan_ref),
        Err(RegistrationError::AuthenticationFailed)
    ));
    store.remove(&plan_ref).expect("remove plan");
    std::fs::remove_dir_all(root).expect("remove test root");
}

#[test]
fn registration_and_integration_state_have_reclaimable_hard_capacity() {
    let root = test_root();
    let registration_secrets = Arc::new(MemorySecrets::default());
    let registrations =
        RegistrationStore::with_secret_store(&root.join("registrations"), registration_secrets)
            .expect("registration store");
    for index in 0_u128..128 {
        let registration_id = format!("registration_{index:032x}");
        registrations
            .install(request(&registration_id, host('c')), 1_000)
            .expect("bounded registration");
    }
    let overflow_registration = "registration_ffffffffffffffffffffffffffffffff";
    assert!(matches!(
        registrations.install(request(overflow_registration, host('c')), 1_001),
        Err(RegistrationError::CapacityExceeded)
    ));
    registrations
        .remove("registration_00000000000000000000000000000000")
        .expect("remove registration");
    registrations
        .install(request(overflow_registration, host('c')), 1_002)
        .expect("reclaimed registration slot");

    let integration_secrets = Arc::new(MemorySecrets::default());
    let states = AuthenticatedIntegrationStore::with_secret_store(
        &root.join("integration"),
        integration_secrets,
    )
    .expect("integration state store");
    for index in 0_u128..512 {
        states
            .install(&format!("plan_{index:032x}"), b"bounded")
            .expect("bounded integration state");
    }
    let overflow_state = "plan_ffffffffffffffffffffffffffffffff";
    assert!(matches!(
        states.install(overflow_state, b"overflow"),
        Err(RegistrationError::CapacityExceeded)
    ));
    states
        .remove("plan_00000000000000000000000000000000")
        .expect("remove integration state");
    states
        .install(overflow_state, b"reclaimed")
        .expect("reclaimed integration slot");
    std::fs::remove_dir_all(root).expect("remove test root");
}

#[test]
fn transport_authorization_is_host_authenticated_secret_derived_and_profile_bound() {
    let root = test_root();
    let registration_id = format!("registration_{}", Uuid::new_v4().simple());
    let store = RegistrationStore::with_secret_store(&root, Arc::new(MemorySecrets::default()))
        .expect("registration store");
    let mut registration = request(&registration_id, host('d'));
    registration.adapter_profile = RegisteredAdapterProfile::Mcp {
        transport: McpRegisteredTransport::StreamableHttp,
        tool_profile: "vision".to_owned(),
    };
    store
        .install(registration, 1_000)
        .expect("install HTTP registration");
    let store = Arc::new(store);
    let provider = RegisteredAuthorityProvider::with_host_identity(
        Arc::clone(&store),
        Arc::new(FixedHost(host('d'))),
    );
    let first = provider
        .http_authorization(&registration_id)
        .expect("derived authorization");
    let second = provider
        .http_authorization(&registration_id)
        .expect("stable derived authorization");
    assert_eq!(first.expose(), second.expose());
    let registration_bytes =
        std::fs::read(root.join(format!("{registration_id}.json"))).expect("registration bytes");
    assert!(
        !registration_bytes
            .windows(first.expose().len())
            .any(|window| window == first.expose().as_bytes())
    );
    let mismatched = RegisteredAuthorityProvider::with_host_identity(
        Arc::clone(&store),
        Arc::new(FixedHost(host('e'))),
    );
    assert!(matches!(
        mismatched.http_authorization(&registration_id),
        Err(RegistrationError::AuthenticationFailed)
    ));

    let cli_registration_id = format!("registration_{}", Uuid::new_v4().simple());
    let mut cli_registration = request(&cli_registration_id, host('d'));
    cli_registration.adapter_profile = RegisteredAdapterProfile::Cli;
    store
        .install(cli_registration, 1_001)
        .expect("install CLI registration");
    assert!(provider.cli_authorization(&cli_registration_id).is_ok());
    assert!(provider.http_authorization(&cli_registration_id).is_err());
    assert!(provider.cli_authorization(&registration_id).is_err());
    std::fs::remove_dir_all(root).expect("remove test root");
}

#[tokio::test]
async fn registration_authenticates_exact_host_and_rejects_replay() {
    let root = test_root();
    let registration_id = format!("registration_{}", Uuid::new_v4().simple());
    let secrets = Arc::new(MemorySecrets::default());
    let store =
        Arc::new(RegistrationStore::with_secret_store(&root, secrets).expect("registration store"));
    let expected_host = host('a');
    let summary = store
        .install(request(&registration_id, expected_host.clone()), 1_000)
        .expect("install registration");
    assert_eq!(summary.registration_id, registration_id);
    assert_eq!(
        store
            .inspect(&registration_id)
            .expect("authenticated inspect")
            .product_version,
        "0.1.0"
    );

    let provider = RegisteredAuthorityProvider::with_host_identity(
        Arc::clone(&store),
        Arc::new(FixedHost(expected_host)),
    );
    let challenge = AuthorityChallenge::new(&registration_id, "runtime-epoch-1", None)
        .expect("valid challenge");
    let grant = provider
        .acquire(challenge.clone())
        .await
        .expect("exact host grant");
    let context = grant
        .resolve(
            &challenge,
            nexus_cua_mcp::current_unix_ms().expect("current time"),
        )
        .expect("bound grant");
    assert!(context.accepts_request_id(&format!(
        "{}:mutation-1",
        context
            .provider_metadata()
            .expect("provider metadata")
            .request_namespace
    )));
    provider
        .validate(context.provider_metadata().expect("provider metadata"))
        .await
        .expect("live grant validates");
    assert_eq!(
        provider
            .acquire(challenge)
            .await
            .expect_err("replay denied"),
        AuthorityProviderError::Denied
    );

    store.remove(&registration_id).expect("remove registration");
    assert_eq!(
        provider
            .validate(context.provider_metadata().expect("provider metadata"))
            .await
            .expect_err("revoked registration is rejected"),
        AuthorityProviderError::Unavailable
    );
    assert!(matches!(
        store.inspect(&registration_id),
        Err(RegistrationError::NotFound)
    ));
    std::fs::remove_dir_all(root).expect("remove test root");
}

#[tokio::test]
async fn registration_tamper_and_host_mismatch_fail_closed() {
    let root = test_root();
    let registration_id = format!("registration_{}", Uuid::new_v4().simple());
    let store = Arc::new(
        RegistrationStore::with_secret_store(&root, Arc::new(MemorySecrets::default()))
            .expect("registration store"),
    );
    store
        .install(request(&registration_id, host('b')), 1_000)
        .expect("install registration");
    let mismatched = RegisteredAuthorityProvider::with_host_identity(
        Arc::clone(&store),
        Arc::new(FixedHost(host('c'))),
    );
    let challenge = AuthorityChallenge::new(&registration_id, "runtime-epoch-1", None)
        .expect("valid challenge");
    assert_eq!(
        mismatched
            .acquire(challenge)
            .await
            .expect_err("host mismatch denied"),
        AuthorityProviderError::AuthenticationFailed
    );

    let path = root.join(format!("{registration_id}.json"));
    let mut bytes = std::fs::read(&path).expect("registration bytes");
    let index = bytes
        .iter()
        .position(|byte| *byte == b'0')
        .expect("version byte");
    bytes[index] = b'9';
    std::fs::write(&path, bytes).expect("tamper registration");
    assert!(store.inspect(&registration_id).is_err());
    std::fs::remove_dir_all(root).expect("remove test root");
}
