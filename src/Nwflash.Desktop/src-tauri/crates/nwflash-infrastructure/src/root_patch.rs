use std::{
    fs::{self, File},
    io::Write,
    path::Path,
};

use nwflash_domain::FlashImageInfo;
use thiserror::Error;

pub const ROOT_PATCH_OUTPUT_FOLDER: &str = "VivoKsu_修补镜像";
pub const MAX_ROOT_PATCH_GROWTH_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum RootPatchArtifactError {
    #[error("ROOT 修补镜像缺少有效文件名。")]
    InvalidFileName,
    #[error("ROOT 修补镜像不存在或为空: {0}")]
    InvalidSource(String),
    #[error("ROOT 修补镜像保存失败: {0}")]
    Io(String),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RootPatchArtifactService;

pub fn validate_patched_root_image(
    source: &FlashImageInfo,
    patched_path: &Path,
) -> Result<FlashImageInfo, RootPatchArtifactError> {
    let source_size = file_size(source.path.as_ref())?;
    let patched_size = file_size(patched_path)?;
    let maximum_size = source_size
        .checked_add(MAX_ROOT_PATCH_GROWTH_BYTES)
        .ok_or_else(|| RootPatchArtifactError::InvalidSource(source.path.clone()))?;
    if patched_size > maximum_size {
        return Err(RootPatchArtifactError::InvalidSource(
            patched_path.to_string_lossy().into_owned(),
        ));
    }

    Ok(FlashImageInfo {
        path: patched_path.to_string_lossy().into_owned(),
        size_bytes: i64::try_from(patched_size).unwrap_or(i64::MAX),
    })
}

fn file_size(path: &Path) -> Result<u64, RootPatchArtifactError> {
    let size = fs::metadata(path)
        .map_err(|_| RootPatchArtifactError::InvalidSource(path.to_string_lossy().into_owned()))?
        .len();
    if size == 0 {
        return Err(RootPatchArtifactError::InvalidSource(
            path.to_string_lossy().into_owned(),
        ));
    }
    Ok(size)
}

impl RootPatchArtifactService {
    pub fn new() -> Self {
        Self
    }

    pub fn export_to_directory(
        &self,
        images: &[FlashImageInfo],
        desktop_directory: &Path,
    ) -> Result<Vec<FlashImageInfo>, RootPatchArtifactError> {
        if images.is_empty() {
            return Ok(Vec::new());
        }

        let output_directory = desktop_directory.join(ROOT_PATCH_OUTPUT_FOLDER);
        fs::create_dir_all(&output_directory)
            .map_err(|error| RootPatchArtifactError::Io(error.to_string()))?;

        images
            .iter()
            .map(|image| self.export_one(image, &output_directory))
            .collect()
    }

    fn export_one(
        &self,
        image: &FlashImageInfo,
        output_directory: &Path,
    ) -> Result<FlashImageInfo, RootPatchArtifactError> {
        let source = Path::new(&image.path);
        let file_name = source
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or(RootPatchArtifactError::InvalidFileName)?;
        file_size(source)?;

        let destination = output_directory.join(file_name);
        if source != destination {
            let pending = destination.with_extension("pending");
            let mut input = File::open(source)
                .map_err(|error| RootPatchArtifactError::Io(error.to_string()))?;
            let mut output = File::create(&pending)
                .map_err(|error| RootPatchArtifactError::Io(error.to_string()))?;
            std::io::copy(&mut input, &mut output)
                .map_err(|error| RootPatchArtifactError::Io(error.to_string()))?;
            output
                .flush()
                .map_err(|error| RootPatchArtifactError::Io(error.to_string()))?;
            output
                .sync_all()
                .map_err(|error| RootPatchArtifactError::Io(error.to_string()))?;
            fs::rename(&pending, &destination)
                .map_err(|error| RootPatchArtifactError::Io(error.to_string()))?;
        }

        let published_size = fs::metadata(&destination)
            .map_err(|error| RootPatchArtifactError::Io(error.to_string()))?
            .len();
        if published_size == 0 {
            return Err(RootPatchArtifactError::InvalidSource(
                destination.to_string_lossy().into_owned(),
            ));
        }

        Ok(FlashImageInfo {
            path: destination.to_string_lossy().into_owned(),
            size_bytes: i64::try_from(published_size).unwrap_or(i64::MAX),
        })
    }
}
