//! Runtime-owned transient screenshot storage.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use nexus_cua_protocol::{
    ArtifactRef, PixelSize, ScreenRect, ScreenshotArtifact, ScreenshotMapping, SessionId,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{RgbaImage, error::public_error};
use nexus_cua_protocol::{CuaError, ErrorCode};

pub(crate) struct ArtifactStore {
    root: PathBuf,
    max_image_pixels: u64,
    max_artifacts_per_session: usize,
    artifacts: Mutex<HashMap<SessionId, VecDeque<PathBuf>>>,
}

impl ArtifactStore {
    pub(crate) fn new(
        root: impl AsRef<Path>,
        max_image_pixels: u64,
        max_artifacts_per_session: usize,
    ) -> Result<Self, CuaError> {
        let requested = root.as_ref();
        fs::create_dir_all(requested).map_err(io_error)?;
        let metadata = fs::symlink_metadata(requested).map_err(io_error)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(public_error(
                ErrorCode::InvalidRequest,
                "artifact root must be a real directory",
                false,
                None,
            ));
        }
        #[cfg(unix)]
        fs::set_permissions(requested, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        let base = requested.canonicalize().map_err(io_error)?;
        let generation = base.join(format!("runtime_{}", Uuid::new_v4().simple()));
        fs::create_dir(&generation).map_err(io_error)?;
        #[cfg(unix)]
        fs::set_permissions(&generation, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        let root = generation.canonicalize().map_err(io_error)?;
        Ok(Self {
            root,
            max_image_pixels,
            max_artifacts_per_session,
            artifacts: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn write_image(
        &self,
        session_id: &SessionId,
        image: &RgbaImage,
        screen_bounds: ScreenRect,
    ) -> Result<ScreenshotArtifact, CuaError> {
        let pixel_count = u64::from(image.width)
            .checked_mul(u64::from(image.height))
            .ok_or_else(|| invalid_image("image dimensions overflow"))?;
        if image.width == 0 || image.height == 0 || pixel_count > self.max_image_pixels {
            return Err(invalid_image(
                "image dimensions are outside the supported range",
            ));
        }
        let expected_bytes = pixel_count
            .checked_mul(4)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| invalid_image("image buffer length overflows this platform"))?;
        if image.pixels.len() != expected_bytes {
            return Err(invalid_image(
                "image buffer length does not match RGBA dimensions",
            ));
        }

        let artifact_ref = ArtifactRef::new(format!("artifact_{}", Uuid::new_v4().simple()));
        let session_directory = self.root.join(session_id.as_str());
        fs::create_dir_all(&session_directory).map_err(io_error)?;
        #[cfg(unix)]
        fs::set_permissions(&session_directory, fs::Permissions::from_mode(0o700))
            .map_err(io_error)?;
        let path = session_directory.join(format!("{}.png", artifact_ref.as_str()));

        let mut png_bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_bytes, image.width, image.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder
                .write_header()
                .map_err(|_| invalid_image("failed to initialize PNG encoder"))?;
            writer
                .write_image_data(&image.pixels)
                .map_err(|_| invalid_image("failed to encode PNG image"))?;
        }

        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&path).map_err(io_error)?;
        file.write_all(&png_bytes).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;

        let sha256 = hex::encode(Sha256::digest(&png_bytes));
        self.register_artifact(session_id, path.clone())?;
        Ok(ScreenshotArtifact {
            artifact_ref,
            path: path.to_string_lossy().into_owned(),
            mime_type: "image/png".to_owned(),
            mapping: ScreenshotMapping {
                screen_bounds,
                pixel_size: PixelSize {
                    width: image.width,
                    height: image.height,
                },
            },
            byte_length: png_bytes.len() as u64,
            sha256,
        })
    }

    pub(crate) fn remove_session(&self, session_id: &SessionId) {
        if let Ok(mut artifacts) = self.artifacts.lock() {
            artifacts.remove(session_id);
        }
        let path = self.root.join(session_id.as_str());
        if path.parent() == Some(self.root.as_path()) {
            let _ = fs::remove_dir_all(path);
        }
    }

    fn register_artifact(&self, session_id: &SessionId, path: PathBuf) -> Result<(), CuaError> {
        let evicted = {
            let mut artifacts = self.artifacts.lock().map_err(|_| {
                public_error(
                    ErrorCode::Internal,
                    "artifact index is unavailable",
                    true,
                    Some("restart_runtime"),
                )
            })?;
            let session = artifacts.entry(session_id.clone()).or_default();
            session.push_back(path);
            (session.len() > self.max_artifacts_per_session)
                .then(|| session.pop_front())
                .flatten()
        };
        if let Some(path) = evicted {
            fs::remove_file(path).map_err(io_error)?;
        }
        Ok(())
    }
}

impl Drop for ArtifactStore {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn io_error(_error: std::io::Error) -> CuaError {
    public_error(
        ErrorCode::Internal,
        "transient artifact storage is unavailable",
        true,
        Some("retry_after_storage_recovery"),
    )
}

fn invalid_image(message: &str) -> CuaError {
    public_error(ErrorCode::DriverFailure, message, false, None)
}
