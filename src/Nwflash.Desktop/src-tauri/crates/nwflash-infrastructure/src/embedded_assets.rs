use std::fs;
use std::io::Write;
use std::path::Path;

use thiserror::Error;

const WIPE_DATA: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/wipe-data.img"));

#[derive(Debug, Error, Clone)]
pub enum EmbeddedAssetError {
    #[error("{0}")]
    Io(String),
}

pub fn write_wipe_data_image(destination: &Path) -> Result<(), EmbeddedAssetError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| EmbeddedAssetError::Io(error.to_string()))?;
    }

    let mut output =
        fs::File::create(destination).map_err(|error| EmbeddedAssetError::Io(error.to_string()))?;
    output
        .write_all(WIPE_DATA)
        .map_err(|error| EmbeddedAssetError::Io(error.to_string()))?;

    output
        .flush()
        .map_err(|error| EmbeddedAssetError::Io(error.to_string()))?;

    Ok(())
}

pub fn wipe_data_size_bytes() -> usize {
    WIPE_DATA.len()
}
