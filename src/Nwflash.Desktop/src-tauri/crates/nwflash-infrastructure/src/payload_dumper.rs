use std::{fs, path::Path};

use nwflash_domain::{PayloadExtractionResult, PayloadPartitionEntry};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadDumperCommand {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Error)]
pub enum PayloadDumperError {
    #[error("payload_dumper 未就绪。")]
    MissingExecutable,
    #[error("payload 源不能为空。")]
    MissingSource,
    #[error("输出目录不能为空。")]
    MissingOutputDirectory,
    #[error("payload 分区名不能为空。")]
    MissingPartition,
    #[error("payload 分区名不安全。")]
    UnsafePartition,
    #[error("payload_dumper 未生成所选分区镜像：{0}")]
    MissingOutput(String),
    #[error("payload 元数据格式无效：{0}")]
    Metadata(String),
}

impl PayloadDumperCommand {
    pub fn metadata(
        executable: impl Into<String>,
        payload_source: impl Into<String>,
        output_directory: impl Into<String>,
    ) -> Result<Self, PayloadDumperError> {
        let program = executable.into();
        let payload_source = payload_source.into();
        let output_directory = output_directory.into();
        if program.trim().is_empty() {
            return Err(PayloadDumperError::MissingExecutable);
        }
        if payload_source.trim().is_empty() {
            return Err(PayloadDumperError::MissingSource);
        }
        if output_directory.trim().is_empty() {
            return Err(PayloadDumperError::MissingOutputDirectory);
        }

        Ok(Self {
            program,
            args: vec![
                payload_source,
                "--metadata".to_string(),
                "-o".to_string(),
                output_directory,
                "--quiet".to_string(),
            ],
        })
    }

    pub fn extract(
        executable: impl Into<String>,
        payload_source: impl Into<String>,
        partition_names: &[&str],
        output_directory: impl Into<String>,
    ) -> Result<Self, PayloadDumperError> {
        let program = executable.into();
        let payload_source = payload_source.into();
        let output_directory = output_directory.into();
        if program.trim().is_empty() {
            return Err(PayloadDumperError::MissingExecutable);
        }
        if payload_source.trim().is_empty() {
            return Err(PayloadDumperError::MissingSource);
        }
        if output_directory.trim().is_empty() {
            return Err(PayloadDumperError::MissingOutputDirectory);
        }
        for name in partition_names {
            validate_partition_name(name)?;
        }

        let mut args = vec![payload_source];
        if !partition_names.is_empty() {
            args.push("-i".to_string());
            args.push(partition_names.join(","));
        }
        args.push("-o".to_string());
        args.push(output_directory);
        Ok(Self { program, args })
    }
}

pub fn collect_payload_extraction_results(
    output_directory: &Path,
    partition_names: &[&str],
) -> Result<Vec<PayloadExtractionResult>, PayloadDumperError> {
    if !output_directory.is_dir() {
        return Err(PayloadDumperError::MissingOutputDirectory);
    }

    let mut results = Vec::new();
    for partition_name in partition_names {
        if partition_name.trim().is_empty() {
            return Err(PayloadDumperError::MissingPartition);
        }
        let output_path = output_directory.join(format!("{partition_name}.img"));
        let size_bytes = match fs::metadata(&output_path) {
            Ok(metadata) if metadata.len() > 0 => i64::try_from(metadata.len()).unwrap_or(i64::MAX),
            _ => continue,
        };
        results.push(PayloadExtractionResult {
            partition_name: (*partition_name).to_string(),
            output_path: output_path.to_string_lossy().into_owned(),
            size_bytes,
        });
    }
    Ok(results)
}

pub fn collect_required_payload_extraction_results(
    output_directory: &Path,
    partition_names: &[&str],
) -> Result<Vec<PayloadExtractionResult>, PayloadDumperError> {
    if !output_directory.is_dir() {
        return Err(PayloadDumperError::MissingOutputDirectory);
    }
    partition_names
        .iter()
        .map(|partition_name| {
            validate_partition_name(partition_name)?;
            let output_path = output_directory.join(format!("{partition_name}.img"));
            let metadata = fs::metadata(&output_path)
                .map_err(|_| PayloadDumperError::MissingOutput((*partition_name).to_string()))?;
            if !metadata.is_file() || metadata.len() == 0 {
                return Err(PayloadDumperError::MissingOutput(
                    (*partition_name).to_string(),
                ));
            }
            Ok(PayloadExtractionResult {
                partition_name: (*partition_name).to_string(),
                output_path: output_path.to_string_lossy().into_owned(),
                size_bytes: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
            })
        })
        .collect()
}

pub fn validate_partition_name(name: &str) -> Result<(), PayloadDumperError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(PayloadDumperError::MissingPartition);
    }
    if name == "."
        || name == ".."
        || name.contains(['/', '\\', ':'])
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(PayloadDumperError::UnsafePartition);
    }
    Ok(())
}

#[derive(Deserialize)]
struct PayloadMetadata {
    #[serde(default)]
    partitions: Vec<PayloadMetadataPartition>,
}

#[derive(Deserialize)]
struct PayloadMetadataPartition {
    partition_name: Option<String>,
    #[serde(default)]
    size_in_bytes: i64,
    compression_type: Option<String>,
}

pub fn parse_payload_metadata(
    json: &str,
) -> Result<Vec<PayloadPartitionEntry>, PayloadDumperError> {
    let metadata: PayloadMetadata = serde_json::from_str(json)
        .map_err(|error| PayloadDumperError::Metadata(error.to_string()))?;

    Ok(metadata
        .partitions
        .into_iter()
        .filter_map(|partition| {
            let name = partition.partition_name?.trim().to_string();
            validate_partition_name(&name)
                .is_ok()
                .then_some(PayloadPartitionEntry {
                    name,
                    size_bytes: partition.size_in_bytes,
                    compression_type: partition
                        .compression_type
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or_else(|| "none".to_string()),
                })
        })
        .collect())
}
