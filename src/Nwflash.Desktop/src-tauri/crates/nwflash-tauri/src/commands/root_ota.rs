//! ROOT 云端 OTA 提取命令：解析服务器 OTA 链接，云提取修补所需的启动分区镜像。
//!
//! - `root_ota_check`：读设备 PD/版本 → `/api/rom` 解析 OTA → 缓存在 Rust 内存（URL 不进浏览器）。
//! - `root_ota_extract_images`：用缓存 URL 云提取 init_boot/boot/vendor_boot → 灌入 RootImageRuntime。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use nwflash_application::{result_to_domain_error, RootOtaExtractOptions, RootOtaService};
use nwflash_domain::{DomainError, FlashImageInfo, OperationKind};
use nwflash_infrastructure::{
    remote_firmware::{probe_remote_kind, RemoteFirmwareError, RemoteFirmwareKind},
    PayloadDumperProvisioner, SecretToken,
};
use serde::Serialize;
use tauri::State;
use tokio::task;

use crate::{
    session_capabilities::{SessionCapabilityLease, SessionCapabilityScope},
    AppState,
};

const ROOT_OTA_CAPABILITY_UNAVAILABLE: &str = "ROOT OTA 会话能力已失效，请重新登录后再试。";

/// 服务器解析出的 ROOT OTA（URL 留 Rust 侧）。
#[derive(Debug, Clone)]
struct ResolvedRootOta {
    epoch: u64,
    url: String,
    name: Option<String>,
    pd: String,
    version: String,
}

#[derive(Default)]
struct RootOtaRuntimeState {
    resolved: Option<ResolvedRootOta>,
    /// 最近一次云提取的 staging 根目录（持有者负责清理）。
    staging_root: Option<std::path::PathBuf>,
}

#[derive(Clone)]
pub struct RootOtaRuntime {
    scope: Arc<SessionCapabilityScope>,
    state: Arc<Mutex<RootOtaRuntimeState>>,
}

impl RootOtaRuntime {
    pub fn new() -> Self {
        Self::with_scope(Arc::new(SessionCapabilityScope::new()))
    }

    pub(crate) fn with_scope(scope: Arc<SessionCapabilityScope>) -> Self {
        Self {
            scope,
            state: Arc::new(Mutex::new(RootOtaRuntimeState::default())),
        }
    }

    #[cfg(test)]
    fn capture_lease(&self) -> Result<SessionCapabilityLease, String> {
        self.scope
            .capture()
            .map_err(|_| ROOT_OTA_CAPABILITY_UNAVAILABLE.to_string())
    }

    fn store(
        &self,
        lease: SessionCapabilityLease,
        mut resolved: ResolvedRootOta,
    ) -> Result<(), String> {
        resolved.epoch = lease.epoch;
        self.scope
            .commit(lease, || {
                self.state
                    .lock()
                    .expect("root ota runtime lock should not be poisoned")
                    .resolved = Some(resolved);
            })
            .map_err(|_| ROOT_OTA_CAPABILITY_UNAVAILABLE.to_string())
    }

    #[cfg(test)]
    fn resolve(&self) -> Result<ResolvedRootOta, String> {
        let lease = self.capture_lease()?;
        self.resolve_with_lease(lease)
    }

    fn resolve_with_lease(&self, lease: SessionCapabilityLease) -> Result<ResolvedRootOta, String> {
        self.scope
            .commit(lease, || {
                let state = self
                    .state
                    .lock()
                    .expect("root ota runtime lock should not be poisoned");
                let resolved = state
                    .resolved
                    .as_ref()
                    .filter(|resolved| resolved.epoch == lease.epoch)
                    .ok_or_else(|| "尚未检测到服务器 OTA，请先检测服务器 OTA。".to_string())?;
                Ok(resolved.clone())
            })
            .map_err(|_| ROOT_OTA_CAPABILITY_UNAVAILABLE.to_string())?
    }

    fn clear_for_probe(&self, lease: SessionCapabilityLease) -> Result<(), String> {
        self.scope
            .commit(lease, || {
                self.state
                    .lock()
                    .expect("root ota runtime lock should not be poisoned")
                    .resolved = None;
            })
            .map_err(|_| ROOT_OTA_CAPABILITY_UNAVAILABLE.to_string())
    }

    fn adopt_staging(
        &self,
        lease: SessionCapabilityLease,
        staging_root: Option<PathBuf>,
    ) -> Result<(), String> {
        let candidate_root = staging_root.clone();
        let publication = self.scope.commit(lease, || {
            let mut state = self
                .state
                .lock()
                .expect("root ota runtime lock should not be poisoned");
            std::mem::replace(&mut state.staging_root, staging_root)
                .into_iter()
                .collect::<Vec<_>>()
        });
        match publication {
            Ok(previous_roots) => {
                for previous in previous_roots {
                    let _ = std::fs::remove_dir_all(previous);
                }
                Ok(())
            }
            Err(_) => {
                if let Some(candidate_root) = candidate_root {
                    let _ = std::fs::remove_dir_all(candidate_root);
                }
                Err(ROOT_OTA_CAPABILITY_UNAVAILABLE.to_string())
            }
        }
    }

    pub(crate) fn clear_owned(&self) -> Vec<PathBuf> {
        let mut state = self
            .state
            .lock()
            .expect("root ota runtime lock should not be poisoned");
        state.resolved = None;
        state.staging_root.take().into_iter().collect()
    }
}

impl Default for RootOtaRuntime {
    fn default() -> Self {
        Self::new()
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

fn publish_extracted_root_images(
    runtime: &crate::commands::root::RootImageRuntime,
    lease: SessionCapabilityLease,
    boot: Option<(FlashImageInfo, String)>,
    vendor_boot: Option<FlashImageInfo>,
) -> Result<
    (
        Option<crate::commands::root::RootImageSelectionDto>,
        Option<crate::commands::root::RootImageSelectionDto>,
    ),
    String,
> {
    runtime.replace_extracted_set(lease, boot, vendor_boot)
}

/// 检测服务器 OTA 并把链接缓存到 Rust 内存。无设备/未登录/查询失败 → 静默不可用。
#[tauri::command]
pub async fn root_ota_check(state: State<'_, AppState>) -> Result<RootOtaCheckDto, String> {
    let lease = match state.session_capabilities.capture() {
        Ok(lease) => lease,
        Err(_) => {
            return Ok(RootOtaCheckDto {
                available: false,
                label: None,
            })
        }
    };
    // 静默失败：无 token / 无设备 / 服务器查询失败都不打断页面。
    let token = match state
        .session_token
        .read()
        .expect("session token lock should not be poisoned")
        .as_ref()
        .filter(|token| !token.is_empty())
        .map(SecretToken::request_scope)
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
    if runtime.clear_for_probe(lease).is_err() {
        return Ok(RootOtaCheckDto {
            available: false,
            label: None,
        });
    }
    let (pd, version) = match read_online_ota_identity(&serial).await {
        Ok(identity) => identity,
        Err(_) => {
            return Ok(RootOtaCheckDto {
                available: false,
                label: None,
            })
        }
    };
    let rom = match client.resolve_rom(token.as_str(), &pd, &version).await {
        Ok(rom) => rom,
        Err(_) => {
            return Ok(RootOtaCheckDto {
                available: false,
                label: None,
            })
        }
    };
    let label = rom
        .name
        .clone()
        .unwrap_or_else(|| format!("{pd} {version}"));
    if runtime
        .store(
            lease,
            ResolvedRootOta {
                epoch: lease.epoch,
                url: rom.url,
                name: rom.name,
                pd,
                version,
            },
        )
        .is_err()
    {
        return Ok(RootOtaCheckDto {
            available: false,
            label: None,
        });
    }
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
    let lease = state
        .session_capabilities
        .capture()
        .map_err(|_| ROOT_OTA_CAPABILITY_UNAVAILABLE.to_string())?;
    let resolved = state.root_ota_runtime.resolve_with_lease(lease)?;
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
                        let provisioner = PayloadDumperProvisioner::bundled(
                            nwflash_windows::bundled_resource_root(),
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

                    let (init_boot_dto, vendor_boot_dto) = publish_extracted_root_images(
                        &image_runtime,
                        lease,
                        images
                            .boot_image
                            .map(|image| (image, images.boot_partition_name)),
                        images.vendor_boot,
                    )
                    .map_err(DomainError::InvalidOperation)?;

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
    root_ota_runtime.adopt_staging(lease, Some(staging))?;
    Ok(RootOtaExtractResultDto {
        source_label: format!("已从 {source_label} 提取"),
        init_boot,
        vendor_boot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::root::{RootImageKind, RootImageRuntime};
    use crate::{session_capabilities::SessionCapabilityScope, AppState};

    fn activated_root_ota_runtime() -> (
        Arc<SessionCapabilityScope>,
        SessionCapabilityLease,
        RootOtaRuntime,
    ) {
        let scope = Arc::new(SessionCapabilityScope::new());
        let lease = scope.activate();
        let runtime = RootOtaRuntime::with_scope(scope.clone());
        (scope, lease, runtime)
    }

    #[test]
    fn delayed_root_ota_publish_is_rejected_after_invalidation() {
        let scope = Arc::new(SessionCapabilityScope::new());
        let lease = scope.activate();
        let runtime = RootOtaRuntime::with_scope(scope.clone());

        scope.invalidate(|| {});
        let publish = runtime.store(
            lease,
            ResolvedRootOta {
                epoch: lease.epoch,
                url: "https://example.invalid/late-ota.zip".to_string(),
                name: None,
                pd: "PD2417".to_string(),
                version: "1.0".to_string(),
            },
        );
        scope.activate();

        assert!(publish.is_err());
        assert!(runtime.resolve().is_err());
    }

    #[test]
    fn root_ota_clear_owned_returns_staging_for_caller_deletion() {
        let scope = Arc::new(SessionCapabilityScope::new());
        let lease = scope.activate();
        let runtime = RootOtaRuntime::with_scope(scope.clone());
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("nwflash-root-ota-owned-{nonce}"));
        std::fs::create_dir_all(&root).expect("staging should be created");
        runtime
            .adopt_staging(lease, Some(root.clone()))
            .expect("current lease should adopt staging");

        let owned_roots = scope.invalidate(|| runtime.clear_owned());

        assert_eq!(owned_roots, vec![root.clone()]);
        assert!(root.exists());
        for owned_root in owned_roots {
            std::fs::remove_dir_all(owned_root).expect("caller should delete returned OTA staging");
        }
        assert!(!root.exists());
    }

    #[test]
    fn rejected_stale_ota_staging_adoption_deletes_only_the_candidate_root() {
        let scope = Arc::new(SessionCapabilityScope::new());
        let lease = scope.activate();
        let runtime = RootOtaRuntime::with_scope(scope.clone());
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let base = std::env::temp_dir().join(format!("nwflash-root-ota-stale-{nonce}"));
        let current = base.join("current-owned");
        let candidate = base.join("late-candidate");
        let other = base.join("other");
        for root in [&current, &candidate, &other] {
            std::fs::create_dir_all(root).expect("staging fixture should be created");
        }
        runtime
            .adopt_staging(lease, Some(current.clone()))
            .expect("current lease should adopt current staging");

        scope.invalidate(|| {});
        let result = runtime.adopt_staging(lease, Some(candidate.clone()));

        assert!(result.is_err());
        assert!(!candidate.exists());
        assert!(current.exists());
        assert!(other.exists());

        scope.activate();
        let owned_roots = scope.invalidate(|| runtime.clear_owned());
        assert_eq!(owned_roots, vec![current.clone()]);
        for owned_root in owned_roots {
            std::fs::remove_dir_all(owned_root).expect("caller should delete current staging");
        }
        std::fs::remove_dir_all(base).expect("test fixture should be removed");
    }

    #[test]
    fn new_root_ota_without_vendor_invalidates_the_old_vendor_before_staging_swap() {
        let scope = Arc::new(SessionCapabilityScope::new());
        let lease = scope.activate();
        let images = RootImageRuntime::with_scope(scope.clone());
        let ota = RootOtaRuntime::with_scope(scope.clone());
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let base = std::env::temp_dir().join(format!("nwflash-root-ota-slot-swap-{nonce}"));
        let old_staging = base.join("old-owned");
        let new_staging = base.join("new-owned");
        std::fs::create_dir_all(&old_staging).expect("old staging should be created");
        std::fs::create_dir_all(&new_staging).expect("new staging should be created");
        let external_boot = base.join("external-selected.img");
        let old_vendor = old_staging.join("vendor_boot.img");
        let new_boot = new_staging.join("init_boot.img");
        std::fs::write(&external_boot, b"external").expect("external image should be written");
        std::fs::write(&old_vendor, b"vendor").expect("old vendor should be written");
        std::fs::write(&new_boot, b"newboot").expect("new boot should be written");

        let old_boot = images
            .replace_with_target(
                lease,
                RootImageKind::InitBoot,
                FlashImageInfo {
                    path: external_boot.to_string_lossy().into_owned(),
                    size_bytes: 8,
                },
                "init_boot".to_string(),
            )
            .expect("external selection should publish");
        let old_vendor_selection = images
            .replace_with_target(
                lease,
                RootImageKind::VendorBoot,
                FlashImageInfo {
                    path: old_vendor.to_string_lossy().into_owned(),
                    size_bytes: 6,
                },
                "vendor_boot".to_string(),
            )
            .expect("old vendor selection should publish");
        ota.adopt_staging(lease, Some(old_staging.clone()))
            .expect("old staging should be adopted");

        let (new_boot_selection, new_vendor_selection) = publish_extracted_root_images(
            &images,
            lease,
            Some((
                FlashImageInfo {
                    path: new_boot.to_string_lossy().into_owned(),
                    size_bytes: 7,
                },
                "init_boot".to_string(),
            )),
            None,
        )
        .expect("new extracted set should publish");
        let new_boot_selection = new_boot_selection.expect("new boot selection should exist");

        let vendor_invalid_before_swap = images
            .get(RootImageKind::VendorBoot, &old_vendor_selection.id)
            .is_err();
        let old_staging_present_before_swap = old_staging.is_dir();
        let old_external_selection_invalid =
            images.get(RootImageKind::InitBoot, &old_boot.id).is_err();
        let new_boot_valid_before_swap = images
            .get(RootImageKind::InitBoot, &new_boot_selection.id)
            .is_ok();

        ota.adopt_staging(lease, Some(new_staging.clone()))
            .expect("new staging should be adopted");

        let old_staging_deleted = !old_staging.exists();
        let vendor_invalid_after_swap = images
            .get(RootImageKind::VendorBoot, &old_vendor_selection.id)
            .is_err();
        let external_file_preserved = external_boot.is_file();
        let new_boot_valid_after_swap = images
            .get(RootImageKind::InitBoot, &new_boot_selection.id)
            .is_ok();
        let new_vendor_absent = new_vendor_selection.is_none();

        let (_, owned_roots) = scope.invalidate(|| (images.clear_owned(), ota.clear_owned()));
        for owned_root in owned_roots {
            std::fs::remove_dir_all(owned_root).expect("owned staging should be removed");
        }
        std::fs::remove_dir_all(base).expect("fixture root should be removed");

        assert!(vendor_invalid_before_swap);
        assert!(old_staging_present_before_swap);
        assert!(old_external_selection_invalid);
        assert!(new_boot_valid_before_swap);
        assert!(old_staging_deleted);
        assert!(vendor_invalid_after_swap);
        assert!(external_file_preserved);
        assert!(new_boot_valid_after_swap);
        assert!(new_vendor_absent);
    }

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
    fn root_ota_runtime_remains_usable_after_the_current_device_changes() {
        let (_scope, lease, runtime) = activated_root_ota_runtime();
        runtime
            .store(
                lease,
                ResolvedRootOta {
                    epoch: lease.epoch,
                    url: "https://example.invalid/ota.zip".to_string(),
                    name: None,
                    pd: "PD2417".to_string(),
                    version: "1.0".to_string(),
                },
            )
            .expect("current lease should publish cached OTA");

        let original_target = "SERIAL-A";
        let current_target = "SERIAL-B";
        assert_ne!(current_target, original_target);
        let resolved = runtime
            .resolve()
            .expect("a current cached OTA must be target-neutral");

        assert_eq!(resolved.url, "https://example.invalid/ota.zip");
        assert!(!format!("{resolved:?}").contains("serial"));
    }

    #[test]
    fn app_state_root_revocation_discards_cached_ota_and_owned_staging() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let state = AppState::new();
        let lease = state.session_capabilities.activate();
        state
            .root_ota_runtime
            .store(
                lease,
                ResolvedRootOta {
                    epoch: lease.epoch,
                    url: "https://example.invalid/ota.zip".to_string(),
                    name: None,
                    pd: "PD2417".to_string(),
                    version: "1.0".to_string(),
                },
            )
            .expect("current lease should publish cached OTA");
        let resolved = state
            .root_ota_runtime
            .resolve()
            .expect("cached OTA resolves");
        assert_eq!(resolved.url, "https://example.invalid/ota.zip");

        // staging 采用/清理。
        let root = std::env::temp_dir().join(format!("nwflash-root-ota-runtime-{nonce}"));
        std::fs::create_dir_all(&root).expect("staging should be created");
        state
            .root_ota_runtime
            .adopt_staging(lease, Some(root.clone()))
            .expect("current lease should adopt staging");
        assert!(root.exists());
        let idle_lease = state
            .operation_coordinator
            .try_acquire_idle()
            .expect("idle state should grant teardown admission");
        state.revoke_root_capabilities(&idle_lease);
        assert!(!root.exists());
        assert!(state.root_ota_runtime.resolve().is_err());
    }

    #[test]
    fn root_ota_runtime_clears_previous_result_before_a_new_probe() {
        let (_scope, lease, runtime) = activated_root_ota_runtime();
        runtime
            .store(
                lease,
                ResolvedRootOta {
                    epoch: lease.epoch,
                    url: "https://example.invalid/ota.zip".to_string(),
                    name: None,
                    pd: "PD2417".to_string(),
                    version: "1.0".to_string(),
                },
            )
            .expect("current lease should publish cached OTA");

        runtime
            .clear_for_probe(lease)
            .expect("current lease should clear before probe");

        assert!(runtime.resolve().is_err());
    }
}
