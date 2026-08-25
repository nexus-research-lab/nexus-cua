//! Secure service-path and token preparation.

use std::path::{Path, PathBuf};

use nexus_cua_protocol::AuthorizationToken;
use nexus_cua_transport::LocalEndpoint;
use uuid::Uuid;

use crate::error::CliError;

pub(crate) struct ServicePaths {
    pub(crate) endpoint: LocalEndpoint,
    pub(crate) token_file: PathBuf,
    pub(crate) artifact_root: PathBuf,
}

impl ServicePaths {
    pub(crate) fn explicit(
        endpoint: String,
        token_file: PathBuf,
        artifact_root: PathBuf,
    ) -> Result<Self, CliError> {
        Ok(Self {
            endpoint: LocalEndpoint::new(endpoint)?,
            token_file,
            artifact_root,
        })
    }

    pub(crate) fn development(root: &Path) -> Result<Self, CliError> {
        secure_directory(root)?;
        let endpoint = if cfg!(windows) {
            format!(r"\\.\pipe\nexus-cua-dev-{}", stable_path_suffix(root))
        } else {
            root.join("service.sock").to_string_lossy().into_owned()
        };
        let token_file = root.join("token");
        if !token_file.exists() {
            let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
            write_private_file(&token_file, token.as_bytes())?;
        }
        Ok(Self {
            endpoint: LocalEndpoint::new(endpoint)?,
            token_file,
            artifact_root: root.join("artifacts"),
        })
    }

    pub(crate) fn prepare(&self) -> Result<AuthorizationToken, CliError> {
        let endpoint_parent = if cfg!(unix) {
            Path::new(self.endpoint.as_str()).parent()
        } else {
            None
        };
        if let Some(parent) = endpoint_parent {
            secure_directory(parent)?;
        }
        secure_directory(&self.artifact_root)?;
        let value = std::fs::read_to_string(&self.token_file)?;
        let value = value.trim();
        if value.len() < 32 {
            return Err(CliError::InvalidConfiguration(
                "token file must contain at least 32 non-whitespace bytes".to_owned(),
            ));
        }
        Ok(AuthorizationToken::new(value))
    }
}

fn stable_path_suffix(path: &Path) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn secure_directory(path: &Path) -> Result<(), CliError> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), CliError> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}
