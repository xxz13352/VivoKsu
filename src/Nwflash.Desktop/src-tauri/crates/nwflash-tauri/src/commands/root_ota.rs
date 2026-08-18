//! ROOT 云端 OTA 提取命令：解析服务器 OTA 链接，云提取修补所需的启动分区镜像。
//!
//! - `root_ota_check`：读设备 PD/版本 → `/api/rom` 解析 OTA → 缓存在 Rust 内存（URL 不进浏览器）。
//! - `root_ota_extract_images`：用缓存 URL 云提取 init_boot/boot/vendor_boot → 灌入 RootImageRuntime。

use std::sync::{Arc, Mutex};

use nwflash_application::{result_to_domain_error, RootOtaExtractOptions, RootOtaService};
use nwflash_domain::{DomainError, OperationKind};
use nwflash_infrastructure::{
    remote_firmware::{probe_remote_kind, RemoteFirmwareError, RemoteFirmwareKind},
    PayloadDumperProvisioner, RemoteAssetDownloader,
};
use serde::Serialize;
use tauri::State;
use tokio::task;

use crate::AppState;

/// 服务器解析出的 ROOT OTA（URL 留 Rust 侧）。
#[derive(Debug, Clone)]
struct ResolvedRootOta {
    url: String,
    name: Option<String>,
    pd: String,
    version: String,
    serial: String,
}

#[derive(Default)]
struct RootOtaRuntimeState {
    resolved: Option<ResolvedRootOta>,
    /// 最近一次云提取的 staging 根目录（持有者负责清理）。
    staging_root: Option<std::path::PathBuf>,
}

#[derive(Clone, Default)]
pub struct RootOtaRuntime {
    state: Arc<Mutex<RootOtaRuntimeState>>,
}

impl RootOtaRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    fn store(&self, resolved: ResolvedRootOta) {
        self.state
            .lock()
            .expect("root ota runtime lock should not be poisoned")
            .resolved = Some(resolved);
    }

    fn resolve(&self, serial: &str) -> Result<ResolvedRootOta, String> {
        let state = self
            .state
            .lock()
            .expect("root ota runtime lock should not be poisoned");
        let resolved = state
            .resolved
            .as_ref()
            .ok_or_else(|| "尚未检测到服务器 OTA，请先检测服务器 OTA。".to_string())?;
        if resolved.serial != serial {
            return Err("设备已变更，请重新检测服务器 OTA。".to_string());
        }
        Ok(resolved.clone())
    }

    fn adopt_staging(&self, staging_root: Option<std::path::PathBuf>) {
        let mut state = self
            .state
            .lock()
            .expect("root ota runtime lock should not be poisoned");
        if let Some(previous) = state.staging_root.take() {
            let _ = std::fs::remove_dir_all(previous);
        }
        state.staging_root = staging_root;
    }

    /// 会话结束/登出时清理缓存与 staging。
    pub(crate) fn cleanup(&self) {
        let mut state = self
            .state
            .lock()
            .expect("root ota runtime lock should not be poisoned");
        state.resolved = None;
        if let Some(staging) = state.staging_root.take() {
            let _ = std::fs::remove_dir_all(staging);
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootOtaCheckDto {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootOtaExtractResultDto {
    pub source_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init_boot: Option<crate::commands::root::RootImageSelectionDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_boot: Option<crate::commands::root::RootImageSelectionDto>,
}

async fn read_online_ota_identity(serial: &str) -> Result<(String, String), String> {
    crate::commands::device_identity::read_online_ota_identity(serial).await
}

fn needs_payload_dumper(kind: RemoteFirmwareKind) -> bool {
    matches!(
        kind,
        RemoteFirmwareKind::PayloadZip | RemoteFirmwareKind::PayloadRaw
    )
}

fn map_probe_error(error: RemoteFirmwareError) -> DomainError {
    match error {
        RemoteFirmwareError::Cancelled => {
            DomainError::UserCancelled("ROOT 云提取已取消。".to_string())
        }
        RemoteFirmwareError::RangeUnsupported => {
            DomainError::InvalidOperation("服务器 OTA 不支持 Range 请求。".to_string())
        }
        RemoteFirmwareError::UnsupportedFormat => {
            DomainError::InvalidFormat("不支持的 OTA 格式，无法云提取 ROOT 分区。".to_string())
        }
        RemoteFirmwareError::InvalidUrl(_)
        | RemoteFirmwareError::Transport(_)
        | RemoteFirmwareError::Archive(_)
        | RemoteFirmwareError::MissingPartition(_)
        | RemoteFirmwareError::Integrity(_) => {
            DomainError::InvalidOperation("无法读取服务器 OTA，请重新检测后再试。".to_string())
        }
    }
}

/// 检测服务器 OTA 并把链接缓存到 Rust 内存。无设备/未登录/查询失败 → 静默不可用。
#[tauri::command]
pub async fn root_ota_check(state: State<'_, AppState>) -> Result<RootOtaCheckDto, String> {
    // 静默失败：无 token / 无设备 / 服务器查询失败都不打断页面。
    let token = match state
        .session_token
        .read()
        .expect("session token lock should not be poisoned")
        .clone()
        .filter(|token| !token.is_empty())
    {
        Some(token) => token,
        None => {
            return Ok(RootOtaCheckDto {
                available: false,
                label: None,
            })
        }
    };
    let serial = match state.device_runtime.active_adb_serial() {
        Ok(serial) => serial,
        Err(_) => {
            return Ok(RootOtaCheckDto {
                available: false,
                label: None,
            })
        }
    };
    let client = state.client.clone();
    let runtime = state.root_ota_runtime.clone();

    let result = Arc::new(Mutex::new(None));
    let result_for_operation = result.clone();
    state
        .operation_coordinator
        .run_async(
            OperationKind::Hashing,
            "检测服务器 OTA",
            move |context, cancellation| async move {
                let (pd, version) = read_online_ota_identity(&serial)
                    .await
                    .map_err(DomainError::DeviceUnavailable)?;
                context.report_stage("正在解析服务器 OTA");
                if cancellation.is_cancelled() {
                    return Ok(());
                }
                let rom = client
                    .resolve_rom(&token, &pd, &version)
                    .await
                    .map_err(|error| DomainError::RemoteApi(error.to_string()))?;
                let resolved = ResolvedRootOta {
                    url: rom.url,
                    name: rom.name,
                    pd,
                    version,
                    serial,
                };
                runtime.store(resolved);
                *result_for_operation.lock().map_err(|_| {
                    DomainError::Internal("ROOT OTA 检测结果锁不可用。".to_string())
                })? = Some(());
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;

    let found = result
        .lock()
        .map_err(|_| "ROOT OTA 检测结果锁不可用。".to_string())?
        .take()
        .is_some();
    if !found {
        return Ok(RootOtaCheckDto {
            available: false,
            label: None,
        });
    }
    let label = match state
        .root_ota_runtime
        .state
        .lock()
        .expect("root ota runtime lock should not be poisoned")
        .resolved
        .as_ref()
    {
        Some(resolved) => resolved
            .name
            .clone()
            .unwrap_or_else(|| format!("{} {}", resolved.pd, resolved.version)),
        None => "服务器 OTA".to_string(),
    };
    Ok(RootOtaCheckDto {
        available: true,
        label: Some(label),
    })
}

/// 用缓存的服务器 OTA 云提取 init_boot/boot/vendor_boot 并灌入 RootImageRuntime。
#[tauri::command]
pub async fn root_ota_extract_images(
    state: State<'_, AppState>,
) -> Result<RootOtaExtractResultDto, String> {
    let _ = crate::commands::safe_flash::session_token(&state)?;
    let serial = state.device_runtime.active_adb_serial()?;
    let resolved = state.root_ota_runtime.resolve(&serial)?;
    let image_runtime = state.root_image_runtime.clone();
    let root_ota_runtime = state.root_ota_runtime.clone();

    let result = Arc::new(Mutex::new(None));
    let result_for_operation = result.clone();
    // staging 所有权：提取成功交给 runtime，失败清理。
    let staging = RootOtaService::create_staging_root().map_err(|error| error.to_string())?;
    let staging_for_operation = staging.clone();

    let operation_result = state
        .operation_coordinator
        .run_async(
            OperationKind::Hashing,
            "提取服务器 OTA 分区",
            move |context, cancellation| {
                let staging = staging_for_operation.clone();
                let resolved = resolved.clone();
                let image_runtime = image_runtime.clone();
                let result_for_operation = result_for_operation.clone();
                async move {
                    context.report_stage("正在探测 OTA 格式");
                    let probe_url = resolved.url.clone();
                    let probe_cancellation = cancellation.clone();
                    let remote_kind = task::spawn_blocking(move || {
                        let mut is_canceled = || probe_cancellation.is_cancelled();
                        probe_remote_kind(&probe_url, None, &mut is_canceled)
                            .map_err(map_probe_error)
                    })
                    .await
                    .map_err(|error| {
                        DomainError::Internal(format!("ROOT OTA 格式探测调度失败：{error}"))
                    })??;

                    let payload_dumper = if needs_payload_dumper(remote_kind) {
                        context.report_stage("正在准备 payload 提取工具");
                        let provisioner = PayloadDumperProvisioner::new(
                            RemoteAssetDownloader::default(),
                            None,
                            None,
                        );
                        Some(
                            provisioner
                                .ensure_installed(&cancellation, None)
                                .await
                                .map_err(|error| {
                                    DomainError::ExternalTool(format!(
                                        "payload 提取工具未就绪：{error}"
                                    ))
                                })?,
                        )
                    } else {
                        None
                    };
                    if cancellation.is_cancelled() {
                        return Err(DomainError::UserCancelled(
                            "ROOT 云提取已取消。".to_string(),
                        ));
                    }
                    context.report_stage("正在云提取 ROOT 分区");
                    let service = RootOtaService::new();
                    let resolved_url = resolved.url.clone();
                    let payload_dumper_path = payload_dumper.clone();
                    // 进度/stage 从阻塞线程透传给 OperationCoordinator（单调进度，防回退）。
                    let context_for_blocking = context.clone();
                    let images = task::spawn_blocking(move || {
                        service.extract(
                            RootOtaExtractOptions {
                                url: &resolved_url,
                                payload_dumper: payload_dumper_path.as_deref(),
                                staging_root: &staging,
                            },
                            || cancellation.is_cancelled(),
                            |stage| context_for_blocking.report_stage(stage),
                            |progress| context_for_blocking.report_progress_monotonic(progress),
                        )
                    })
                    .await
                    .map_err(|error| {
                        DomainError::Internal(format!("ROOT 云提取调度失败：{error}"))
                    })??;

                    let init_boot_dto = match images.boot_image {
                        Some(image) => Some(image_runtime.replace(
                            crate::commands::root::RootImageKind::InitBoot,
                            image,
                            images.boot_partition_name,
                        )),
                        None => None,
                    };
                    let vendor_boot_dto = match images.vendor_boot {
                        Some(image) => Some(image_runtime.replace(
                            crate::commands::root::RootImageKind::VendorBoot,
                            image,
                            "vendor_boot".to_string(),
                        )),
                        None => None,
                    };

                    let source_label = resolved
                        .name
                        .unwrap_or_else(|| format!("{} {}", resolved.pd, resolved.version));
                    *result_for_operation.lock().map_err(|_| {
                        DomainError::Internal("ROOT 云提取结果锁不可用。".to_string())
                    })? = Some((source_label, init_boot_dto, vendor_boot_dto));
                    Ok(())
                }
            },
        )
        .await;

    if let Err(error) = operation_result {
        // 失败/取消：清理 staging，避免残留大镜像。
        let _ = std::fs::remove_dir_all(&staging);
        return Err(result_to_domain_error(error).to_string());
    }

    let (source_label, init_boot, vendor_boot) = result
        .lock()
        .map_err(|_| "ROOT 云提取结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "ROOT 云提取未产生结果。".to_string())?;
    // 成功：staging 交给 runtime 持有，供后续修补使用。
    root_ota_runtime.adopt_staging(Some(staging));
    Ok(RootOtaExtractResultDto {
        source_label: format!("已从 {source_label} 提取"),
        init_boot,
        vendor_boot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_dumper_is_required_only_for_payload_ota_kinds() {
        assert!(needs_payload_dumper(
            nwflash_infrastructure::remote_firmware::RemoteFirmwareKind::PayloadZip
        ));
        assert!(needs_payload_dumper(
            nwflash_infrastructure::remote_firmware::RemoteFirmwareKind::PayloadRaw
        ));
        assert!(!needs_payload_dumper(
            nwflash_infrastructure::remote_firmware::RemoteFirmwareKind::DirectImageZip
        ));
        assert!(!needs_payload_dumper(
            nwflash_infrastructure::remote_firmware::RemoteFirmwareKind::Unsupported
        ));
    }

    #[test]
    fn root_ota_dto_does_not_expose_url_serial_or_path() {
        let dto = RootOtaCheckDto {
            available: true,
            label: Some("PD2417 16.2.12.0".to_string()),
        };
        let value = serde_json::to_string(&dto).expect("should serialize");
        assert!(!value.contains("http"));
        assert!(!value.contains("serial"));
        assert!(!value.contains("/"));
    }

    #[test]
    fn root_ota_runtime_rejects_serial_mismatch_and_cleans_staging() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let runtime = RootOtaRuntime::new();
        runtime.store(ResolvedRootOta {
            url: "https://example.invalid/ota.zip".to_string(),
            name: None,
            pd: "PD2417".to_string(),
            version: "1.0".to_string(),
            serial: "SERIAL-A".to_string(),
        });
        // 其它设备 → 拒绝。
        assert!(runtime.resolve("SERIAL-B").is_err());
        // 相同设备 → 可解析（URL 只在 Rust 侧）。
        let resolved = runtime.resolve("SERIAL-A").expect("same serial resolves");
        assert_eq!(resolved.url, "https://example.invalid/ota.zip");

        // staging 采用/清理。
        let root = std::env::temp_dir().join(format!("nwflash-root-ota-runtime-{nonce}"));
        std::fs::create_dir_all(&root).expect("staging should be created");
        runtime.adopt_staging(Some(root.clone()));
        assert!(root.exists());
        runtime.cleanup();
        assert!(!root.exists());
        assert!(runtime.resolve("SERIAL-A").is_err());
    }
}
