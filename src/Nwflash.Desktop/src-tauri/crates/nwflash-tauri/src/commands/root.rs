use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use nwflash_application::{
    result_to_domain_error, FileManagerService, OperationContext, OperationCoordinator,
    RootManager, RootPatchPreflightRequest, RootPatchReadiness, RootService, SafeFlashBuildOptions,
    SafeFlashExecutionRequest, SafeFlashExecutionService, SafeFlashPartitionSource,
    SafeFlashPreparedSource,
};
use nwflash_domain::{
    DomainError, FlashImageInfo, OperationKind, PartitionExecutionPlan, QuickFlashPartition,
    SafeFlashSlotMode,
};
use nwflash_infrastructure::{
    resolve_vendor_boot_module_directories, validate_patched_root_image, VivoRootResourceService,
};
use nwflash_windows::{bundled_platform_tool, process::run_command_with_cancel, ProcessCommand};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::oneshot;
use tokio::task;

use crate::{
    session_capabilities::{SessionCapabilityLease, SessionCapabilityScope},
    AppState,
};

static ROOT_IMAGE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static ROOT_PATCH_ARTIFACT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const VIVO_KSU_REMOTE_DIRECTORY: &str = "/data/local/tmp";
const VIVO_KSU_REMOTE_LIBRARY: &str = "/data/local/tmp/vivoksu_libksud.so";
const VENDOR_BOOT_REMOTE_BASE: &str = "/data/local/tmp/nwflash_vendor_boot";
const ROOT_CAPABILITY_UNAVAILABLE: &str = "ROOT 会话能力已失效，请重新登录后再试。";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RootImageKind {
    InitBoot,
    VendorBoot,
}

impl RootImageKind {
    fn label(self) -> &'static str {
        match self {
            Self::InitBoot => "init_boot",
            Self::VendorBoot => "vendor_boot",
        }
    }
}

/// 由目标分区名派生刷写用的 QuickFlashPartition（boot / init_boot / vendor_boot）。
fn quick_flash_partition_from_name(name: &str) -> Option<QuickFlashPartition> {
    match name {
        "boot" => Some(QuickFlashPartition::Boot),
        "init_boot" => Some(QuickFlashPartition::InitBoot),
        "vendor_boot" => Some(QuickFlashPartition::VendorBoot),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootImageSelectionDto {
    pub id: String,
    pub kind: RootImageKind,
    pub file_name: String,
    /// 实际刷写/修补的目标分区名（`init_boot`、`boot` 或 `vendor_boot`）。
    pub partition_name: String,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootManagerInstallDto {
    pub manager_label: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootPatchedArtifactDto {
    pub artifact_id: String,
    pub partition: String,
    pub file_name: String,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootPatchedFlashConfirmationDto {
    pub partition: String,
    pub task_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootAutomaticResultDto {
    pub flashed_partition_count: usize,
    pub command_count: usize,
    pub status: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RootPreflightOptionsDto {
    pub manager: RootManager,
    pub init_boot_id: Option<String>,
    pub vendor_boot_id: Option<String>,
    pub use_automatic_kmi: bool,
    pub selected_kmi: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RootVivoKsuPatchOptionsDto {
    pub manager: Option<RootManager>,
    pub init_boot_id: String,
    pub use_automatic_kmi: bool,
    pub selected_kmi: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RootOfficialVendorBootPatchOptionsDto {
    pub vendor_boot_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RootAutomaticOptionsDto {
    pub manager: RootManager,
    pub init_boot_id: String,
    pub vendor_boot_id: Option<String>,
    pub use_automatic_kmi: bool,
    pub selected_kmi: Option<String>,
}

#[derive(Debug, Clone)]
struct RootAutomaticSelection {
    manager: RootManager,
    init_boot: FlashImageInfo,
    /// boot 槽位实际分区名（`init_boot` 或 `boot`）。
    boot_partition_name: String,
    vendor_boot: Option<FlashImageInfo>,
    use_automatic_kmi: bool,
    selected_kmi: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutomaticRootStage {
    InstallManager,
    PatchInitBoot,
    PatchVendorBoot,
    FlashFastbootd,
}

fn automatic_root_stage_plan(manager: RootManager) -> &'static [AutomaticRootStage] {
    match manager {
        RootManager::VivoKsu => &[
            AutomaticRootStage::InstallManager,
            AutomaticRootStage::PatchInitBoot,
            AutomaticRootStage::FlashFastbootd,
        ],
        RootManager::OfficialKernelSu => &[
            AutomaticRootStage::InstallManager,
            AutomaticRootStage::PatchInitBoot,
            AutomaticRootStage::PatchVendorBoot,
            AutomaticRootStage::FlashFastbootd,
        ],
    }
}

#[derive(Debug, Clone)]
struct RootImageSelection {
    epoch: u64,
    id: String,
    image: FlashImageInfo,
    /// 实际刷写/修补的目标分区名（`init_boot`、`boot` 或 `vendor_boot`）。
    /// 本地手动选择默认 = `kind.label()`；云端提取的 boot 槽位可能是 `init_boot` 或 `boot`。
    target_partition_name: String,
}

#[derive(Default)]
struct RootImageRuntimeState {
    init_boot: Option<RootImageSelection>,
    vendor_boot: Option<RootImageSelection>,
}

#[derive(Clone)]
pub struct RootImageRuntime {
    scope: Arc<SessionCapabilityScope>,
    state: Arc<Mutex<RootImageRuntimeState>>,
}

impl RootImageRuntime {
    pub fn new() -> Self {
        Self::with_scope(Arc::new(SessionCapabilityScope::new()))
    }

    pub(crate) fn with_scope(scope: Arc<SessionCapabilityScope>) -> Self {
        Self {
            scope,
            state: Arc::new(Mutex::new(RootImageRuntimeState::default())),
        }
    }

    fn capture_lease(&self) -> Result<SessionCapabilityLease, String> {
        self.scope
            .capture()
            .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())
    }

    pub(crate) fn replace_with_target(
        &self,
        lease: SessionCapabilityLease,
        kind: RootImageKind,
        image: FlashImageInfo,
        target_partition_name: String,
    ) -> Result<RootImageSelectionDto, String> {
        let id = format!(
            "root-image-{}-{}",
            kind.label(),
            ROOT_IMAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let selection = RootImageSelection {
            epoch: lease.epoch,
            id: id.clone(),
            image,
            target_partition_name,
        };
        self.scope
            .commit(lease, || {
                let mut state = self
                    .state
                    .lock()
                    .expect("root image runtime lock should not be poisoned");
                match kind {
                    RootImageKind::InitBoot => state.init_boot = Some(selection.clone()),
                    RootImageKind::VendorBoot => state.vendor_boot = Some(selection.clone()),
                }
                root_image_selection_dto(kind, selection)
            })
            .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())
    }

    pub(crate) fn replace_extracted_set(
        &self,
        lease: SessionCapabilityLease,
        boot: Option<(FlashImageInfo, String)>,
        vendor_boot: Option<FlashImageInfo>,
    ) -> Result<(Option<RootImageSelectionDto>, Option<RootImageSelectionDto>), String> {
        let init_boot = boot.map(|(image, target_partition_name)| RootImageSelection {
            epoch: lease.epoch,
            id: format!(
                "root-image-{}-{}",
                RootImageKind::InitBoot.label(),
                ROOT_IMAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ),
            image,
            target_partition_name,
        });
        let vendor_boot = vendor_boot.map(|image| RootImageSelection {
            epoch: lease.epoch,
            id: format!(
                "root-image-{}-{}",
                RootImageKind::VendorBoot.label(),
                ROOT_IMAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ),
            image,
            target_partition_name: RootImageKind::VendorBoot.label().to_string(),
        });
        self.scope
            .commit(lease, || {
                let mut state = self
                    .state
                    .lock()
                    .expect("root image runtime lock should not be poisoned");
                state.init_boot = init_boot.clone();
                state.vendor_boot = vendor_boot.clone();
                (
                    init_boot.map(|selection| {
                        root_image_selection_dto(RootImageKind::InitBoot, selection)
                    }),
                    vendor_boot.map(|selection| {
                        root_image_selection_dto(RootImageKind::VendorBoot, selection)
                    }),
                )
            })
            .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())
    }

    /// Test-only fixture helper for virtual paths that do not exist on disk.
    #[cfg(test)]
    pub fn replace(
        &self,
        kind: RootImageKind,
        image: FlashImageInfo,
        target_partition_name: String,
    ) -> RootImageSelectionDto {
        let lease = self
            .capture_lease()
            .expect("ROOT image test runtime must be activated");
        self.replace_with_target(lease, kind, image, target_partition_name)
            .expect("current test lease should publish ROOT image")
    }

    /// Test-only fixture helper: target defaults to kind label.
    #[cfg(test)]
    pub fn replace_default(
        &self,
        kind: RootImageKind,
        image: FlashImageInfo,
    ) -> RootImageSelectionDto {
        self.replace(kind, image, kind.label().to_string())
    }

    pub fn get(&self, kind: RootImageKind, id: &str) -> Result<FlashImageInfo, String> {
        self.get_selection(kind, id)
            .map(|selection| selection.image)
    }

    fn get_selection(&self, kind: RootImageKind, id: &str) -> Result<RootImageSelection, String> {
        let lease = self.capture_lease()?;
        self.get_selection_with_lease(lease, kind, id)
    }

    fn get_selection_with_lease(
        &self,
        lease: SessionCapabilityLease,
        kind: RootImageKind,
        id: &str,
    ) -> Result<RootImageSelection, String> {
        self.scope
            .commit(lease, || {
                let state = self
                    .state
                    .lock()
                    .expect("root image runtime lock should not be poisoned");
                let selection = match kind {
                    RootImageKind::InitBoot => state.init_boot.as_ref(),
                    RootImageKind::VendorBoot => state.vendor_boot.as_ref(),
                };
                selection
                    .filter(|selection| selection.id == id && selection.epoch == lease.epoch)
                    .cloned()
                    .ok_or_else(|| "ROOT 镜像选择已失效，请重新选择。".to_string())
            })
            .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())?
    }

    /// 取当前 boot 槽位（InitBoot kind）的镜像与其实际目标分区名。
    #[cfg(test)]
    fn get_boot_with_target(&self, id: &str) -> Result<(FlashImageInfo, String), String> {
        let selection = self.get_selection(RootImageKind::InitBoot, id)?;
        Ok((selection.image, selection.target_partition_name))
    }

    fn get_boot_with_target_with_lease(
        &self,
        lease: SessionCapabilityLease,
        id: &str,
    ) -> Result<(FlashImageInfo, String), String> {
        let selection = self.get_selection_with_lease(lease, RootImageKind::InitBoot, id)?;
        Ok((selection.image, selection.target_partition_name))
    }

    #[cfg(test)]
    fn take_automatic(
        &self,
        options: RootAutomaticOptionsDto,
    ) -> Result<RootAutomaticSelection, String> {
        let lease = self.capture_lease()?;
        self.take_automatic_with_lease(lease, options)
    }

    fn take_automatic_with_lease(
        &self,
        lease: SessionCapabilityLease,
        options: RootAutomaticOptionsDto,
    ) -> Result<RootAutomaticSelection, String> {
        let (init_boot, vendor_boot) = self.snapshot_automatic(lease, &options)?;
        self.take_snapshotted_automatic(lease, options, &init_boot, vendor_boot.as_ref())
    }

    #[cfg(test)]
    fn take_automatic_with_lease_and_observer(
        &self,
        lease: SessionCapabilityLease,
        options: RootAutomaticOptionsDto,
        observe: impl FnOnce(&RootImageSelection, Option<&RootImageSelection>) -> Result<(), String>,
    ) -> Result<RootAutomaticSelection, String> {
        let (init_boot, vendor_boot) = self.snapshot_automatic(lease, &options)?;

        observe(&init_boot, vendor_boot.as_ref())?;

        self.take_snapshotted_automatic(lease, options, &init_boot, vendor_boot.as_ref())
    }

    fn snapshot_automatic(
        &self,
        lease: SessionCapabilityLease,
        options: &RootAutomaticOptionsDto,
    ) -> Result<(RootImageSelection, Option<RootImageSelection>), String> {
        self.scope
            .commit(lease, || {
                let state = self
                    .state
                    .lock()
                    .expect("root image runtime lock should not be poisoned");
                let init_boot = state
                    .init_boot
                    .as_ref()
                    .filter(|selection| {
                        selection.id == options.init_boot_id && selection.epoch == lease.epoch
                    })
                    .cloned()
                    .ok_or_else(|| "ROOT 镜像选择已失效，请重新选择。".to_string())?;

                let vendor_boot = match options.manager {
                    RootManager::VivoKsu => {
                        if options.vendor_boot_id.is_some() {
                            return Err("Vivo KSU 全自动流程只接受 init_boot 镜像。".to_string());
                        }
                        None
                    }
                    RootManager::OfficialKernelSu => {
                        let vendor_boot_id =
                            options.vendor_boot_id.as_deref().ok_or_else(|| {
                                "官方 KernelSU 全自动流程需要当前 vendor_boot 镜像。".to_string()
                            })?;
                        Some(
                            state
                                .vendor_boot
                                .as_ref()
                                .filter(|selection| {
                                    selection.id == vendor_boot_id && selection.epoch == lease.epoch
                                })
                                .cloned()
                                .ok_or_else(|| "ROOT 镜像选择已失效，请重新选择。".to_string())?,
                        )
                    }
                };

                Ok((init_boot, vendor_boot))
            })
            .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())?
    }

    fn take_snapshotted_automatic(
        &self,
        lease: SessionCapabilityLease,
        options: RootAutomaticOptionsDto,
        expected_init_boot: &RootImageSelection,
        expected_vendor_boot: Option<&RootImageSelection>,
    ) -> Result<RootAutomaticSelection, String> {
        self.scope
            .commit(lease, || {
                let mut state = self
                    .state
                    .lock()
                    .expect("root image runtime lock should not be poisoned");
                let init_boot_is_current = state.init_boot.as_ref().is_some_and(|selection| {
                    selection.id == expected_init_boot.id
                        && selection.epoch == lease.epoch
                        && expected_init_boot.epoch == lease.epoch
                });
                let vendor_boot_is_current = match options.manager {
                    RootManager::VivoKsu => expected_vendor_boot.is_none(),
                    RootManager::OfficialKernelSu => {
                        let Some(expected_vendor_boot) = expected_vendor_boot else {
                            return Err(
                                "官方 KernelSU 全自动流程需要当前 vendor_boot 镜像。".to_string()
                            );
                        };
                        state.vendor_boot.as_ref().is_some_and(|selection| {
                            selection.id == expected_vendor_boot.id
                                && selection.epoch == lease.epoch
                                && expected_vendor_boot.epoch == lease.epoch
                        })
                    }
                };
                if !init_boot_is_current || !vendor_boot_is_current {
                    return Err("ROOT 镜像选择已失效，请重新选择。".to_string());
                }

                let init_boot = state
                    .init_boot
                    .take()
                    .ok_or_else(|| "ROOT 镜像选择已失效，请重新选择。".to_string())?;
                let vendor_boot = if options.manager == RootManager::OfficialKernelSu {
                    state.vendor_boot.take()
                } else {
                    None
                };

                Ok(RootAutomaticSelection {
                    manager: options.manager,
                    init_boot: init_boot.image,
                    boot_partition_name: init_boot.target_partition_name,
                    vendor_boot: vendor_boot.map(|selection| selection.image),
                    use_automatic_kmi: options.use_automatic_kmi,
                    selected_kmi: options.selected_kmi,
                })
            })
            .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())?
    }

    #[allow(dead_code)]
    pub(crate) fn clear_owned(&self) -> Vec<PathBuf> {
        let mut state = self
            .state
            .lock()
            .expect("root image runtime lock should not be poisoned");
        state.init_boot = None;
        state.vendor_boot = None;
        Vec::new()
    }
}

impl Default for RootImageRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
struct RootPatchedArtifact {
    epoch: u64,
    id: String,
    partition: QuickFlashPartition,
    image: FlashImageInfo,
    staging_root: Option<PathBuf>,
}

#[derive(Default)]
struct RootPatchedArtifactState {
    init_boot: Option<RootPatchedArtifact>,
    vendor_boot: Option<RootPatchedArtifact>,
    prepared_flash: Option<PreparedRootFlash>,
}

struct PreparedRootFlash {
    epoch: u64,
    artifact_id: String,
    plan: PartitionExecutionPlan,
}

#[derive(Clone)]
pub struct RootPatchedArtifactRuntime {
    scope: Arc<SessionCapabilityScope>,
    state: Arc<Mutex<RootPatchedArtifactState>>,
}

impl RootPatchedArtifactRuntime {
    pub fn new() -> Self {
        Self::with_scope(Arc::new(SessionCapabilityScope::new()))
    }

    pub(crate) fn with_scope(scope: Arc<SessionCapabilityScope>) -> Self {
        Self {
            scope,
            state: Arc::new(Mutex::new(RootPatchedArtifactState::default())),
        }
    }

    #[cfg(test)]
    fn capture_lease(&self) -> Result<SessionCapabilityLease, String> {
        self.scope
            .capture()
            .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())
    }

    #[cfg(test)]
    fn replace(
        &self,
        kind: RootImageKind,
        image: FlashImageInfo,
        flash_partition: QuickFlashPartition,
    ) -> RootPatchedArtifactDto {
        let lease = self
            .capture_lease()
            .expect("ROOT artifact test runtime must be activated");
        self.replace_with_ownership(lease, kind, image, flash_partition, None)
            .expect("current test lease should publish ROOT artifact")
    }

    fn replace_owned(
        &self,
        lease: SessionCapabilityLease,
        kind: RootImageKind,
        image: FlashImageInfo,
        flash_partition: QuickFlashPartition,
        staging_root: PathBuf,
    ) -> Result<RootPatchedArtifactDto, String> {
        self.replace_with_ownership(lease, kind, image, flash_partition, Some(staging_root))
    }

    fn replace_with_ownership(
        &self,
        lease: SessionCapabilityLease,
        kind: RootImageKind,
        image: FlashImageInfo,
        flash_partition: QuickFlashPartition,
        staging_root: Option<PathBuf>,
    ) -> Result<RootPatchedArtifactDto, String> {
        let candidate_root = staging_root.clone();
        let artifact = RootPatchedArtifact {
            epoch: lease.epoch,
            id: format!(
                "root-patch-{}-{}",
                kind.label(),
                ROOT_PATCH_ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ),
            partition: flash_partition,
            image,
            staging_root,
        };
        let publication = self.scope.commit(lease, || {
            let mut state = self
                .state
                .lock()
                .expect("root patched artifact runtime lock should not be poisoned");
            let replaced = match kind {
                RootImageKind::InitBoot => state.init_boot.replace(artifact.clone()),
                RootImageKind::VendorBoot => state.vendor_boot.replace(artifact.clone()),
            };
            state.prepared_flash = None;
            (
                root_patched_artifact_dto(artifact),
                replaced.and_then(|previous| previous.staging_root),
            )
        });
        match publication {
            Ok((dto, previous_root)) => {
                if let Some(previous_root) = previous_root {
                    let _ = fs::remove_dir_all(previous_root);
                }
                Ok(dto)
            }
            Err(_) => {
                if let Some(candidate_root) = candidate_root {
                    let _ = fs::remove_dir_all(candidate_root);
                }
                Err(ROOT_CAPABILITY_UNAVAILABLE.to_string())
            }
        }
    }

    #[cfg(test)]
    fn get(&self, artifact_id: &str) -> Result<RootPatchedArtifact, String> {
        let lease = self.capture_lease()?;
        self.get_with_lease(lease, artifact_id)
    }

    fn get_with_lease(
        &self,
        lease: SessionCapabilityLease,
        artifact_id: &str,
    ) -> Result<RootPatchedArtifact, String> {
        self.scope
            .commit(lease, || {
                let state = self
                    .state
                    .lock()
                    .expect("root patched artifact runtime lock should not be poisoned");
                let artifact = [state.init_boot.as_ref(), state.vendor_boot.as_ref()]
                    .into_iter()
                    .flatten()
                    .find(|artifact| artifact.id == artifact_id && artifact.epoch == lease.epoch)
                    .cloned()
                    .ok_or_else(|| "ROOT 修补工件已失效，请重新修补。".to_string());
                artifact
            })
            .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())?
    }

    #[cfg(test)]
    fn prepare_flash(&self, artifact_id: String, plan: PartitionExecutionPlan) {
        let lease = self
            .capture_lease()
            .expect("ROOT artifact test runtime must be activated");
        self.prepare_flash_with_lease(lease, artifact_id, plan)
            .expect("current test lease should publish prepared ROOT flash");
    }

    fn prepare_flash_with_lease(
        &self,
        lease: SessionCapabilityLease,
        artifact_id: String,
        plan: PartitionExecutionPlan,
    ) -> Result<(), String> {
        self.scope
            .commit(lease, || {
                let mut state = self
                    .state
                    .lock()
                    .expect("root patched artifact runtime lock should not be poisoned");
                let artifact_is_current = [state.init_boot.as_ref(), state.vendor_boot.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|artifact| artifact.id == artifact_id && artifact.epoch == lease.epoch);
                if !artifact_is_current {
                    return Err("ROOT 修补工件已失效，请重新修补。".to_string());
                }
                state.prepared_flash = Some(PreparedRootFlash {
                    epoch: lease.epoch,
                    artifact_id,
                    plan,
                });
                Ok(())
            })
            .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())?
    }

    #[cfg(test)]
    fn take_prepared_flash(&self, artifact_id: &str) -> Result<PartitionExecutionPlan, String> {
        let lease = self.capture_lease()?;
        self.take_prepared_flash_with_lease(lease, artifact_id)
    }

    fn take_prepared_flash_with_lease(
        &self,
        lease: SessionCapabilityLease,
        artifact_id: &str,
    ) -> Result<PartitionExecutionPlan, String> {
        self.scope
            .commit(lease, || {
                let mut state = self
                    .state
                    .lock()
                    .expect("root patched artifact runtime lock should not be poisoned");
                match state.prepared_flash.take() {
                    Some(prepared)
                        if prepared.artifact_id == artifact_id && prepared.epoch == lease.epoch =>
                    {
                        Ok(prepared.plan)
                    }
                    Some(prepared) => {
                        state.prepared_flash = Some(prepared);
                        Err("ROOT 修补镜像刷写预检已失效，请重新确认。".to_string())
                    }
                    None => Err("请先确认 ROOT 修补镜像刷写。".to_string()),
                }
            })
            .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())?
    }

    #[allow(dead_code)]
    pub(crate) fn clear_owned(&self) -> Vec<PathBuf> {
        let mut state = self
            .state
            .lock()
            .expect("root patched artifact runtime lock should not be poisoned");
        state.prepared_flash = None;
        [state.init_boot.take(), state.vendor_boot.take()]
            .into_iter()
            .flatten()
            .filter_map(|artifact| artifact.staging_root)
            .collect()
    }
}

impl Default for RootPatchedArtifactRuntime {
    fn default() -> Self {
        Self::new()
    }
}

fn root_image_selection_dto(
    kind: RootImageKind,
    selection: RootImageSelection,
) -> RootImageSelectionDto {
    let file_name = Path::new(&selection.image.path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("已选镜像")
        .to_string();
    RootImageSelectionDto {
        id: selection.id,
        kind,
        file_name,
        partition_name: selection.target_partition_name,
        size_bytes: selection.image.size_bytes,
    }
}

fn root_patched_artifact_dto(artifact: RootPatchedArtifact) -> RootPatchedArtifactDto {
    let file_name = Path::new(&artifact.image.path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("ROOT 修补镜像")
        .to_string();
    RootPatchedArtifactDto {
        artifact_id: artifact.id,
        partition: artifact.partition.partition_name().to_string(),
        file_name,
        size_bytes: artifact.image.size_bytes,
    }
}

#[cfg(test)]
fn automatic_root_flash_source(
    runtime: &RootPatchedArtifactRuntime,
    manager: RootManager,
    artifact_ids: &[String],
) -> Result<SafeFlashPreparedSource, String> {
    let lease = runtime.capture_lease()?;
    automatic_root_flash_source_with_lease(runtime, lease, manager, artifact_ids)
}

fn automatic_root_flash_source_with_lease(
    runtime: &RootPatchedArtifactRuntime,
    lease: SessionCapabilityLease,
    manager: RootManager,
    artifact_ids: &[String],
) -> Result<SafeFlashPreparedSource, String> {
    let expected_artifact_count = match manager {
        RootManager::VivoKsu => 1,
        RootManager::OfficialKernelSu => 2,
    };
    if artifact_ids.len() != expected_artifact_count {
        return Err(match manager {
            RootManager::VivoKsu => "Vivo KSU 自动流程需要一个 init_boot 工件。",
            RootManager::OfficialKernelSu => {
                "官方 KernelSU 自动流程需要 init_boot 与 vendor_boot 两个工件。"
            }
        }
        .to_string());
    }

    let artifacts = artifact_ids
        .iter()
        .map(|artifact_id| runtime.get_with_lease(lease, artifact_id))
        .collect::<Result<Vec<_>, _>>()?;
    let mut partitions = Vec::with_capacity(artifacts.len());
    // 依规范顺序排列：boot 槽位（init_boot 或 boot 由工件真实分区名决定）在前，vendor_boot 在后。
    for expected in [
        QuickFlashPartition::InitBoot,
        QuickFlashPartition::Boot,
        QuickFlashPartition::VendorBoot,
    ] {
        if let Some(artifact) = artifacts
            .iter()
            .find(|artifact| artifact.partition == expected)
        {
            partitions.push(SafeFlashPartitionSource {
                partition_name: artifact.partition.partition_name().to_string(),
                image_path: artifact.image.path.clone(),
                has_slot: false,
            });
        }
    }

    if partitions.len() != artifacts.len()
        || partitions
            .first()
            .is_none_or(|source| !matches!(source.partition_name.as_str(), "init_boot" | "boot"))
        || (manager == RootManager::OfficialKernelSu
            && partitions
                .get(1)
                .is_none_or(|source| source.partition_name != "vendor_boot"))
    {
        return Err(
            "ROOT 自动流程必须从当前 boot/init_boot 修补工件开始，且不能重复刷写分区。".to_string(),
        );
    }

    Ok(SafeFlashPreparedSource {
        staging_root: None,
        partitions,
        wipe_data_image_path: None,
        has_block_based_content: false,
    })
}

fn inspect_root_image(path: &Path) -> Result<FlashImageInfo, String> {
    let extension_is_supported = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("img") || extension.eq_ignore_ascii_case("bin")
        });
    if !extension_is_supported {
        return Err("ROOT 仅支持 .img 或 .bin 镜像。".to_string());
    }
    let metadata = std::fs::metadata(path).map_err(|_| "无法读取所选 ROOT 镜像。".to_string())?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err("所选 ROOT 镜像为空或不可用。".to_string());
    }
    Ok(FlashImageInfo {
        path: path.to_string_lossy().into_owned(),
        size_bytes: i64::try_from(metadata.len())
            .map_err(|_| "ROOT 镜像大小超出支持范围。".to_string())?,
    })
}

fn build_adb_kernel_release_command(serial: &str) -> Result<ProcessCommand, String> {
    if serial.trim().is_empty() || !serial.chars().all(is_safe_adb_serial_character) {
        return Err("当前 ADB 设备标识无效。".to_string());
    }
    Ok(ProcessCommand::new(
        bundled_platform_tool("adb.exe"),
        [
            "-s".to_string(),
            serial.to_string(),
            "shell".to_string(),
            "uname".to_string(),
            "-r".to_string(),
        ],
    ))
}

fn manager_resource_key(manager: RootManager) -> &'static str {
    match manager {
        RootManager::VivoKsu => "KSU",
        RootManager::OfficialKernelSu => "OfficialKsu",
    }
}

fn build_manager_package_verification_command(
    serial: &str,
    package_name: &str,
) -> Result<ProcessCommand, String> {
    build_adb_manager_command(serial, ["pm", "path", package_name])
}

fn build_manager_launch_command(
    serial: &str,
    package_name: &str,
    activity_name: &str,
) -> Result<ProcessCommand, String> {
    let component = format!("{package_name}/{activity_name}");
    build_adb_manager_command(serial, ["am", "start", "-n", component.as_str()])
}

fn build_adb_manager_command<'a>(
    serial: &str,
    command: impl IntoIterator<Item = &'a str>,
) -> Result<ProcessCommand, String> {
    if serial.trim().is_empty() || !serial.chars().all(is_safe_adb_serial_character) {
        return Err("当前 ADB 设备标识无效。".to_string());
    }
    let mut args = vec!["-s".to_string(), serial.to_string(), "shell".to_string()];
    args.extend(command.into_iter().map(str::to_string));
    Ok(ProcessCommand::new(bundled_platform_tool("adb.exe"), args))
}

fn vivo_ksu_remote_source(partition: &str) -> String {
    format!("/data/local/tmp/vivoksu_{partition}.img")
}

fn vivo_ksu_remote_patched(partition: &str) -> String {
    format!("/data/local/tmp/vivoksu_patched_{partition}.img")
}

fn validate_boot_partition_name(partition: &str) -> Result<(), String> {
    if matches!(partition, "init_boot" | "boot") {
        Ok(())
    } else {
        Err("不支持的 ROOT boot 分区名。".to_string())
    }
}

#[allow(clippy::too_many_arguments)]
fn build_vivo_ksu_patch_commands(
    serial: &str,
    library_path: &Path,
    source_path: &Path,
    staged_output_path: &Path,
    kmi: &str,
    partition: &str,
) -> Result<Vec<ProcessCommand>, String> {
    if serial.trim().is_empty() || !serial.chars().all(is_safe_adb_serial_character) {
        return Err("当前 ADB 设备标识无效。".to_string());
    }
    VivoRootResourceService::validate_kmi(kmi).map_err(|_| "不支持的 ROOT KMI。".to_string())?;
    validate_boot_partition_name(partition)?;

    let adb =
        |arguments: Vec<String>| ProcessCommand::new(bundled_platform_tool("adb.exe"), arguments);
    let remote_source = vivo_ksu_remote_source(partition);
    let remote_patched = vivo_ksu_remote_patched(partition);
    // 脚本内用相对文件名（脚本已 cd 到 VIVO_KSU_REMOTE_DIRECTORY）。
    let script_source = Path::new(&remote_source)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("vivoksu_init_boot.img")
        .to_string();
    let script_patched = Path::new(&remote_patched)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("vivoksu_patched_init_boot.img")
        .to_string();
    let patch_script = format!(
        "cd {VIVO_KSU_REMOTE_DIRECTORY} && chmod 755 vivoksu_libksud.so && TMPDIR={VIVO_KSU_REMOTE_DIRECTORY} ./vivoksu_libksud.so boot-patch -b {script_source} --out {VIVO_KSU_REMOTE_DIRECTORY} --out-name {script_patched} --partition {partition} --kmi {kmi}"
    );

    Ok(vec![
        adb(vec![
            "-s".to_string(),
            serial.to_string(),
            "push".to_string(),
            library_path.to_string_lossy().into_owned(),
            VIVO_KSU_REMOTE_LIBRARY.to_string(),
        ]),
        adb(vec![
            "-s".to_string(),
            serial.to_string(),
            "push".to_string(),
            source_path.to_string_lossy().into_owned(),
            remote_source.clone(),
        ]),
        adb(vec![
            "-s".to_string(),
            serial.to_string(),
            "shell".to_string(),
            "-T".to_string(),
            "sh".to_string(),
            "-c".to_string(),
            patch_script,
        ]),
        adb(vec![
            "-s".to_string(),
            serial.to_string(),
            "pull".to_string(),
            remote_patched,
            staged_output_path.to_string_lossy().into_owned(),
        ]),
    ])
}

fn build_vivo_ksu_patch_cleanup_command(
    serial: &str,
    partition: &str,
) -> Result<ProcessCommand, String> {
    let remote_source = vivo_ksu_remote_source(partition);
    let remote_patched = vivo_ksu_remote_patched(partition);
    build_adb_manager_command(
        serial,
        [
            "rm",
            "-f",
            VIVO_KSU_REMOTE_LIBRARY,
            remote_source.as_str(),
            remote_patched.as_str(),
        ],
    )
}

fn vendor_boot_remote_root_from_token(workspace_token: &str) -> Result<String, String> {
    if workspace_token.is_empty()
        || workspace_token.len() > 64
        || !workspace_token
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("ROOT vendor_boot 工作目录无效。".to_string());
    }
    Ok(format!("{VENDOR_BOOT_REMOTE_BASE}_{workspace_token}"))
}

fn validate_vendor_boot_remote_root(remote_root: &str) -> Result<String, String> {
    let workspace_token = remote_root
        .strip_prefix(&format!("{VENDOR_BOOT_REMOTE_BASE}_"))
        .ok_or_else(|| "ROOT vendor_boot 工作目录无效。".to_string())?;
    let validated_root = vendor_boot_remote_root_from_token(workspace_token)?;
    if validated_root != remote_root {
        return Err("ROOT vendor_boot 工作目录无效。".to_string());
    }
    Ok(validated_root)
}

fn build_vendor_boot_cleanup_command(
    serial: &str,
    remote_root: &str,
) -> Result<ProcessCommand, String> {
    let validated_root = validate_vendor_boot_remote_root(remote_root)?;
    build_adb_manager_command(serial, ["rm", "-rf", validated_root.as_str()])
}

fn build_vendor_boot_setup_commands(
    serial: &str,
    magiskboot_path: &Path,
    source_path: &Path,
    workspace_token: &str,
) -> Result<Vec<ProcessCommand>, String> {
    if serial.trim().is_empty() || !serial.chars().all(is_safe_adb_serial_character) {
        return Err("当前 ADB 设备标识无效。".to_string());
    }
    let remote_root = vendor_boot_remote_root_from_token(workspace_token)?;
    let adb =
        |arguments: Vec<String>| ProcessCommand::new(bundled_platform_tool("adb.exe"), arguments);
    let unpack_script = format!(
        "cd {remote_root} && chmod 755 magiskboot && ./magiskboot unpack vendor_boot.img 2>&1; echo UNPACK_EXIT=$?; find . -maxdepth 3 -name '*.cpio' 2>/dev/null"
    );
    Ok(vec![
        adb(vec![
            "-s".to_string(),
            serial.to_string(),
            "shell".to_string(),
            "mkdir".to_string(),
            "-p".to_string(),
            remote_root.clone(),
        ]),
        adb(vec![
            "-s".to_string(),
            serial.to_string(),
            "push".to_string(),
            magiskboot_path.to_string_lossy().into_owned(),
            format!("{remote_root}/magiskboot"),
        ]),
        adb(vec![
            "-s".to_string(),
            serial.to_string(),
            "push".to_string(),
            source_path.to_string_lossy().into_owned(),
            format!("{remote_root}/vendor_boot.img"),
        ]),
        adb(vec![
            "-s".to_string(),
            serial.to_string(),
            "shell".to_string(),
            "-T".to_string(),
            "sh".to_string(),
            "-c".to_string(),
            unpack_script,
        ]),
    ])
}

fn build_vendor_boot_module_update_command(
    serial: &str,
    remote_root: &str,
    module_directory: &str,
    file_name: &str,
) -> Result<ProcessCommand, String> {
    if serial.trim().is_empty() || !serial.chars().all(is_safe_adb_serial_character) {
        return Err("当前 ADB 设备标识无效。".to_string());
    }
    let validated_root = validate_vendor_boot_remote_root(remote_root)?;
    let is_allowed_directory = module_directory == "lib/modules"
        || module_directory
            .strip_prefix("lib/modules/")
            .and_then(|directory| directory.strip_suffix("-gki"))
            .is_some_and(|version| {
                !version.is_empty()
                    && version.starts_with(|character: char| character.is_ascii_digit())
                    && version.chars().all(|character| {
                        character.is_ascii_alphanumeric()
                            || matches!(character, '.' | '_' | '+' | '-')
                    })
            });
    if !is_allowed_directory {
        return Err("ROOT vendor_boot 模块目录无效。".to_string());
    }
    let filter = match file_name {
        "modules.load" | "modules.load.recovery" => {
            format!("sed -i '/vr\\.ko/d' {file_name}")
        }
        "modules.softdep" => {
            "sed -i '/softdep[[:space:]]\\+vr[[:space:]]\\+pre/d' modules.softdep".to_string()
        }
        _ => return Err("ROOT vendor_boot 模块文件无效。".to_string()),
    };
    let path = format!("{module_directory}/{file_name}");
    let script = format!(
        "cd {validated_root}/vendor_ramdisk && {validated_root}/magiskboot cpio ramdisk.cpio \"extract {path} {file_name}\" && test -f {file_name} && {filter} && {validated_root}/magiskboot cpio ramdisk.cpio \"add 0644 {path} {file_name}\" && rm -f {file_name}"
    );
    Ok(ProcessCommand::new(
        bundled_platform_tool("adb.exe"),
        [
            "-s".to_string(),
            serial.to_string(),
            "shell".to_string(),
            "-T".to_string(),
            "sh".to_string(),
            "-c".to_string(),
            script,
        ],
    ))
}

fn build_vendor_boot_repack_pull_commands(
    serial: &str,
    remote_root: &str,
    staged_output_path: &Path,
) -> Result<Vec<ProcessCommand>, String> {
    if serial.trim().is_empty() || !serial.chars().all(is_safe_adb_serial_character) {
        return Err("当前 ADB 设备标识无效。".to_string());
    }
    let validated_root = validate_vendor_boot_remote_root(remote_root)?;
    let repack_script = format!(
        "cd {validated_root} && {validated_root}/magiskboot repack vendor_boot.img 2>&1 && test -f new-boot.img && echo REPACKED"
    );
    Ok(vec![
        ProcessCommand::new(
            bundled_platform_tool("adb.exe"),
            [
                "-s".to_string(),
                serial.to_string(),
                "shell".to_string(),
                "-T".to_string(),
                "sh".to_string(),
                "-c".to_string(),
                repack_script,
            ],
        ),
        ProcessCommand::new(
            bundled_platform_tool("adb.exe"),
            [
                "-s".to_string(),
                serial.to_string(),
                "pull".to_string(),
                format!("{validated_root}/new-boot.img"),
                staged_output_path.to_string_lossy().into_owned(),
            ],
        ),
    ])
}

fn build_vendor_boot_module_listing_command(
    serial: &str,
    remote_root: &str,
) -> Result<ProcessCommand, String> {
    let root = validate_vendor_boot_remote_root(remote_root)?;
    let script = format!(
        "cd {root}/vendor_ramdisk && {root}/magiskboot cpio ramdisk.cpio \"ls /lib/modules/\""
    );
    build_adb_manager_command(serial, ["-T", "sh", "-c", script.as_str()])
}

fn create_root_patch_staging() -> Result<PathBuf, DomainError> {
    let base = std::env::temp_dir().join("nwflash").join("root-patch");
    fs::create_dir_all(&base)
        .map_err(|_| DomainError::Internal("无法创建 ROOT 修补临时目录。".to_string()))?;
    for _ in 0..16 {
        let root = base.join(format!(
            "patch-{}",
            ROOT_PATCH_ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::create_dir(&root) {
            Ok(()) => return Ok(root),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                return Err(DomainError::Internal(
                    "无法创建 ROOT 修补临时目录。".to_string(),
                ))
            }
        }
    }
    Err(DomainError::Internal(
        "无法创建唯一 ROOT 修补临时目录。".to_string(),
    ))
}

async fn finalize_vendor_boot_workflow<T, Cleanup>(
    workflow_result: Result<T, DomainError>,
    staging: PathBuf,
    cleanup: Cleanup,
) -> Result<T, DomainError>
where
    Cleanup: std::future::Future<Output = Result<(), DomainError>>,
{
    let _ = cleanup.await;
    if workflow_result.is_err() {
        let _ = fs::remove_dir_all(staging);
    }
    workflow_result
}

fn publish_root_patch_candidate(
    artifacts: &RootPatchedArtifactRuntime,
    lease: SessionCapabilityLease,
    kind: RootImageKind,
    candidate: Result<FlashImageInfo, DomainError>,
    flash_partition: QuickFlashPartition,
    staging: PathBuf,
) -> Result<RootPatchedArtifactDto, DomainError> {
    let image = match candidate {
        Ok(candidate) => candidate,
        Err(error) => {
            let _ = fs::remove_dir_all(staging);
            return Err(error);
        }
    };
    artifacts
        .replace_owned(lease, kind, image, flash_partition, staging)
        .map_err(DomainError::InvalidOperation)
}

fn is_safe_adb_serial_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
}

fn parse_kernel_release(output: &str) -> Result<String, String> {
    let release = output.trim();
    if release.is_empty()
        || release.len() > 128
        || release.lines().count() != 1
        || !release.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '+' | '-')
        })
    {
        return Err("无法读取有效的设备 Kernel 版本。".to_string());
    }
    Ok(release.to_string())
}

async fn read_connected_kernel_release(
    serial: String,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<String, DomainError> {
    let command = build_adb_kernel_release_command(&serial).map_err(DomainError::InvalidInput)?;
    let output = tokio::task::spawn_blocking(move || {
        run_command_with_cancel(command, None, move || cancellation.is_cancelled())
    })
    .await
    .map_err(|_| DomainError::Internal("读取设备 Kernel 任务已中断。".to_string()))??;
    if output.exit_code != 0 {
        return Err(DomainError::DeviceUnavailable(
            "无法读取已连接设备的 Kernel 版本。".to_string(),
        ));
    }
    parse_kernel_release(&output.stdout).map_err(DomainError::DeviceUnavailable)
}

async fn execute_root_manager_command(
    command: ProcessCommand,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<String, DomainError> {
    let output = task::spawn_blocking(move || {
        run_command_with_cancel(command, None, move || cancellation.is_cancelled())
    })
    .await
    .map_err(|_| DomainError::Internal("ROOT 管理器操作任务已中断。".to_string()))?
    .map_err(|error| match error {
        DomainError::UserCancelled(_) => error,
        _ => DomainError::ExternalTool("ROOT 管理器设备操作失败。".to_string()),
    })?;
    if output.exit_code != 0 {
        return Err(DomainError::ExternalTool(
            "ROOT 管理器设备操作失败。".to_string(),
        ));
    }
    Ok(output.stdout)
}

async fn execute_root_patch_command(
    command: ProcessCommand,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<(), DomainError> {
    execute_root_patch_command_output(command, cancellation)
        .await
        .map(|_| ())
}

async fn execute_root_patch_command_output(
    command: ProcessCommand,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<String, DomainError> {
    let output = task::spawn_blocking(move || {
        run_command_with_cancel(command, None, move || cancellation.is_cancelled())
    })
    .await
    .map_err(|_| DomainError::Internal("ROOT 修补任务已中断。".to_string()))?
    .map_err(|error| match error {
        DomainError::UserCancelled(_) => error,
        _ => DomainError::ExternalTool("ROOT 修补设备操作失败。".to_string()),
    })?;
    if output.exit_code != 0 {
        return Err(DomainError::ExternalTool(
            "ROOT 修补设备操作失败。".to_string(),
        ));
    }
    Ok(output.stdout)
}

fn report_root_subprogress(
    context: &OperationContext,
    progress_base: f64,
    progress_span: f64,
    stage_progress: f64,
) {
    context.report_progress_monotonic(progress_base + progress_span * stage_progress);
}

async fn install_root_manager_core(
    manager: RootManager,
    serial: String,
    app_root: PathBuf,
    context: &OperationContext,
    cancellation: tokio_util::sync::CancellationToken,
    progress_base: f64,
    progress_span: f64,
) -> Result<RootManagerInstallDto, DomainError> {
    let manager_label = manager.label();
    context.report_stage(format!("正在准备 {manager_label} 管理器"));
    let resources = VivoRootResourceService::new(app_root, None);
    let requested = resources
        .resolve_manager(manager_resource_key(manager))
        .map_err(|_| DomainError::InvalidOperation("ROOT 管理器资源不可用。".to_string()))?;
    let verified = resources
        .ensure_manager_apk(&requested, &cancellation, None)
        .await
        .map_err(|_| DomainError::InvalidOperation("ROOT 管理器资源校验失败。".to_string()))?;

    context.report_stage(format!("正在安装 {manager_label} 管理器"));
    let install = FileManagerService::bundled()
        .build_install_apk_command(&serial, Path::new(&verified.apk_path))
        .map_err(|_| DomainError::InvalidOperation("ROOT 管理器安装条件无效。".to_string()))?;
    execute_root_manager_command(
        ProcessCommand {
            program: install.program,
            args: install.args,
            working_directory: install.working_directory,
            environment: install.environment,
        },
        cancellation.clone(),
    )
    .await?;

    context.report_stage("正在验证 ROOT 管理器安装状态");
    let verification = build_manager_package_verification_command(&serial, &verified.package_name)
        .map_err(DomainError::InvalidInput)?;
    let package_state = execute_root_manager_command(verification, cancellation.clone()).await?;
    if !package_state.contains("package:") {
        return Err(DomainError::InvalidOperation(
            "未检测到已安装的 ROOT 管理器。".to_string(),
        ));
    }

    context.report_stage("正在启动 ROOT 管理器");
    let launch =
        build_manager_launch_command(&serial, &verified.package_name, &verified.activity_name)
            .map_err(DomainError::InvalidInput)?;
    execute_root_manager_command(launch, cancellation).await?;
    report_root_subprogress(context, progress_base, progress_span, 1.0);

    Ok(RootManagerInstallDto {
        manager_label: manager_label.to_string(),
        summary: format!("{manager_label} 管理器已安装并启动。"),
    })
}

#[allow(clippy::too_many_arguments)]
async fn patch_vivo_ksu_core(
    lease: SessionCapabilityLease,
    manager: RootManager,
    serial: String,
    source: FlashImageInfo,
    partition: &str,
    use_automatic_kmi: bool,
    selected_kmi: Option<String>,
    app_root: PathBuf,
    artifacts: RootPatchedArtifactRuntime,
    context: &OperationContext,
    cancellation: tokio_util::sync::CancellationToken,
    progress_base: f64,
    progress_span: f64,
) -> Result<RootPatchedArtifactDto, DomainError> {
    let kmi = if use_automatic_kmi {
        context.report_stage("正在读取设备 Kernel 版本");
        let release = read_connected_kernel_release(serial.clone(), cancellation.clone()).await?;
        VivoRootResourceService::map_kernel_release(&release)
            .map(str::to_string)
            .map_err(|_| DomainError::InvalidInput("不支持的 ROOT KMI。".to_string()))?
    } else {
        selected_kmi
            .as_deref()
            .ok_or_else(|| DomainError::InvalidInput("请选择受支持的 KMI。".to_string()))
            .and_then(|kmi| {
                VivoRootResourceService::validate_kmi(kmi)
                    .map(str::to_string)
                    .map_err(|_| DomainError::InvalidInput("不支持的 ROOT KMI。".to_string()))
            })?
    };

    context.report_stage("正在准备 ROOT 修补资源");
    let resources = VivoRootResourceService::new(app_root, None);
    let requested = resources
        .resolve_manager(manager_resource_key(manager))
        .map_err(|_| DomainError::InvalidOperation("ROOT 管理器资源不可用。".to_string()))?;
    let verified_manager = resources
        .ensure_manager_apk(&requested, &cancellation, None)
        .await
        .map_err(|_| DomainError::InvalidOperation("ROOT 管理器资源校验失败。".to_string()))?;
    let staging = create_root_patch_staging()?;
    let library_path = staging.join("libksud.so");
    let staged_output = staging.join(format!("patched_{partition}.img"));
    let extract_result = task::spawn_blocking({
        let resources = resources.clone();
        let verified_manager = verified_manager.clone();
        let library_path = library_path.clone();
        move || resources.extract_verified_libksud(&verified_manager, "arm64-v8a", &library_path)
    })
    .await
    .map_err(|_| DomainError::Internal("ROOT 库提取任务已中断。".to_string()))?
    .map_err(|_| DomainError::InvalidOperation("ROOT 管理器库不可用。".to_string()));
    if let Err(error) = extract_result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    let patch_result = async {
        let commands = build_vivo_ksu_patch_commands(
            &serial,
            &library_path,
            Path::new(&source.path),
            &staged_output,
            &kmi,
            partition,
        )
        .map_err(DomainError::InvalidInput)?;
        for (index, command) in commands.into_iter().enumerate() {
            context.report_stage(match index {
                0 => "正在上传 ROOT 修补工具",
                1 => "正在上传 boot 镜像",
                2 => "正在处理 boot 镜像",
                _ => "正在获取修补后的 boot 镜像",
            });
            execute_root_patch_command(command, cancellation.clone()).await?;
            report_root_subprogress(
                context,
                progress_base,
                progress_span,
                (index + 1) as f64 / 5.0,
            );
        }
        task::spawn_blocking({
            let source = source.clone();
            let staged_output = staged_output.clone();
            move || validate_patched_root_image(&source, &staged_output).map_err(|_| ())
        })
        .await
        .map_err(|_| DomainError::Internal("ROOT 产物校验任务已中断。".to_string()))?
        .map_err(|_| DomainError::InvalidOperation("ROOT 修补产物无效。".to_string()))
    }
    .await;
    let cleanup = build_vivo_ksu_patch_cleanup_command(&serial, partition)
        .map_err(DomainError::InvalidInput)?;
    let _ = execute_root_patch_command(cleanup, tokio_util::sync::CancellationToken::new()).await;
    let flash_partition = quick_flash_partition_from_name(partition)
        .ok_or_else(|| DomainError::InvalidInput("不支持的 ROOT boot 分区名。".to_string()))?;
    let artifact = publish_root_patch_candidate(
        &artifacts,
        lease,
        RootImageKind::InitBoot,
        patch_result,
        flash_partition,
        staging,
    )?;
    report_root_subprogress(context, progress_base, progress_span, 1.0);
    Ok(artifact)
}

#[allow(clippy::too_many_arguments)]
async fn patch_official_vendor_boot_core(
    lease: SessionCapabilityLease,
    serial: String,
    source: FlashImageInfo,
    app_root: PathBuf,
    artifacts: RootPatchedArtifactRuntime,
    context: &OperationContext,
    cancellation: tokio_util::sync::CancellationToken,
    progress_base: f64,
    progress_span: f64,
) -> Result<RootPatchedArtifactDto, DomainError> {
    let token = format!(
        "vendor-{}",
        ROOT_PATCH_ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let remote_root =
        vendor_boot_remote_root_from_token(&token).map_err(DomainError::InvalidInput)?;
    let staging = create_root_patch_staging()?;
    let staged_output = staging.join("patched_vendor_boot.img");
    let patch_result: Result<FlashImageInfo, DomainError> = async {
        context.report_stage("正在准备 vendor_boot 修补资源");
        let resources = VivoRootResourceService::new(app_root, None);
        let tool = resources.resolve_magiskboot();
        resources
            .verify_root_tool(&tool)
            .map_err(|_| DomainError::InvalidOperation("ROOT 修补工具不可用。".to_string()))?;
        let setup = build_vendor_boot_setup_commands(
            &serial,
            Path::new(&tool.path),
            Path::new(&source.path),
            &token,
        )
        .map_err(DomainError::InvalidInput)?;
        let mut unpack_output = String::new();
        for (index, command) in setup.into_iter().enumerate() {
            context.report_stage(match index {
                0 => "正在创建 vendor_boot 工作目录",
                1 => "正在上传 vendor_boot 修补工具",
                2 => "正在上传 vendor_boot 镜像",
                _ => "正在解包 vendor_boot 镜像",
            });
            let output = execute_root_patch_command_output(command, cancellation.clone()).await?;
            if index == 3 {
                unpack_output = output;
            }
        }
        if !unpack_output.contains("vendor_ramdisk/ramdisk.cpio") {
            return Err(DomainError::InvalidOperation(
                "vendor_boot 未包含有效 ramdisk。".to_string(),
            ));
        }
        context.report_stage("正在读取 vendor_boot 模块目录");
        let listing = execute_root_patch_command_output(
            build_vendor_boot_module_listing_command(&serial, &remote_root)
                .map_err(DomainError::InvalidInput)?,
            cancellation.clone(),
        )
        .await?;
        for directory in resolve_vendor_boot_module_directories(&listing) {
            for file_name in ["modules.load", "modules.load.recovery", "modules.softdep"] {
                let command = build_vendor_boot_module_update_command(
                    &serial,
                    &remote_root,
                    &directory,
                    file_name,
                )
                .map_err(DomainError::InvalidInput)?;
                let update = execute_root_patch_command(command, cancellation.clone()).await;
                if let Err(error) = update {
                    if matches!(error, DomainError::UserCancelled(_)) || directory == "lib/modules"
                    {
                        return Err(error);
                    }
                }
            }
        }
        context.report_stage("正在重打包 vendor_boot 镜像");
        let repack_pull =
            build_vendor_boot_repack_pull_commands(&serial, &remote_root, &staged_output)
                .map_err(DomainError::InvalidInput)?;
        let repack_output =
            execute_root_patch_command_output(repack_pull[0].clone(), cancellation.clone()).await?;
        if !repack_output.contains("REPACKED") {
            return Err(DomainError::InvalidOperation(
                "vendor_boot 重打包失败。".to_string(),
            ));
        }
        context.report_stage("正在获取修补后的 vendor_boot 镜像");
        execute_root_patch_command(repack_pull[1].clone(), cancellation.clone()).await?;
        task::spawn_blocking({
            let source = source.clone();
            let staged_output = staged_output.clone();
            move || validate_patched_root_image(&source, &staged_output).map_err(|_| ())
        })
        .await
        .map_err(|_| DomainError::Internal("ROOT 产物校验任务已中断。".to_string()))?
        .map_err(|_| DomainError::InvalidOperation("ROOT 修补产物无效。".to_string()))
    }
    .await;
    let cleanup_serial = serial.clone();
    let cleanup_remote_root = remote_root.clone();
    let patch_result = finalize_vendor_boot_workflow(patch_result, staging.clone(), async move {
        if let Ok(cleanup) =
            build_vendor_boot_cleanup_command(&cleanup_serial, &cleanup_remote_root)
        {
            let _ = execute_root_patch_command(cleanup, tokio_util::sync::CancellationToken::new())
                .await;
        }
        Ok(())
    })
    .await;
    let artifact = publish_root_patch_candidate(
        &artifacts,
        lease,
        RootImageKind::VendorBoot,
        patch_result,
        QuickFlashPartition::VendorBoot,
        staging,
    )?;
    report_root_subprogress(context, progress_base, progress_span, 1.0);
    Ok(artifact)
}

fn root_preflight_response(
    request: RootPatchPreflightRequest,
) -> Result<RootPatchReadiness, String> {
    RootService::new()
        .evaluate_preflight(request)
        .map_err(|error| match error {
            nwflash_domain::DomainError::InvalidInput(message)
            | nwflash_domain::DomainError::InvalidOperation(message) => message,
            _ => "ROOT 预检失败，请检查所选镜像和设备状态。".to_string(),
        })
}

fn build_root_automatic_execution_request<'a>(
    source: &'a SafeFlashPreparedSource,
    options: &'a SafeFlashBuildOptions,
    serial: &'a str,
) -> SafeFlashExecutionRequest<'a> {
    SafeFlashExecutionRequest {
        source,
        options,
        serial,
        transition_to_fastbootd: true,
    }
}

#[cfg(test)]
fn root_preflight_from_runtime(
    runtime: &RootImageRuntime,
    options: RootPreflightOptionsDto,
    connected_kernel_release: Option<String>,
) -> Result<RootPatchReadiness, String> {
    let lease = runtime.capture_lease()?;
    root_preflight_from_runtime_with_lease(runtime, lease, options, connected_kernel_release)
}

fn root_preflight_from_runtime_with_lease(
    runtime: &RootImageRuntime,
    lease: SessionCapabilityLease,
    options: RootPreflightOptionsDto,
    connected_kernel_release: Option<String>,
) -> Result<RootPatchReadiness, String> {
    let init_boot = match options.init_boot_id.as_deref() {
        Some(id) => Some(
            runtime
                .get_selection_with_lease(lease, RootImageKind::InitBoot, id)?
                .image,
        ),
        None => None,
    };
    let vendor_boot = match options.vendor_boot_id.as_deref() {
        Some(id) => Some(
            runtime
                .get_selection_with_lease(lease, RootImageKind::VendorBoot, id)?
                .image,
        ),
        None => None,
    };
    root_preflight_response(RootPatchPreflightRequest {
        manager: options.manager,
        init_boot,
        vendor_boot,
        use_automatic_kmi: options.use_automatic_kmi,
        connected_kernel_release,
        selected_kmi: options.selected_kmi,
    })
}

#[tauri::command]
pub async fn root_preflight(
    state: State<'_, AppState>,
    options: RootPreflightOptionsDto,
) -> Result<RootPatchReadiness, String> {
    let lease = state
        .session_capabilities
        .capture()
        .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())?;
    let device_runtime = state.device_runtime.clone();
    let runtime = state.root_image_runtime.clone();
    let readiness = Arc::new(Mutex::new(None));
    let readiness_for_operation = readiness.clone();
    state
        .operation_coordinator
        .run_async(
            OperationKind::Hashing,
            "ROOT 预检",
            move |context, cancellation| async move {
                context.report_stage("正在检查 ROOT 条件");
                let connected_kernel_release = if options.use_automatic_kmi {
                    context.report_stage("正在读取设备 Kernel 版本");
                    let serial = device_runtime
                        .active_adb_serial()
                        .map_err(DomainError::DeviceUnavailable)?;
                    Some(read_connected_kernel_release(serial.clone(), cancellation.clone()).await?)
                } else {
                    None
                };
                let result = root_preflight_from_runtime_with_lease(
                    &runtime,
                    lease,
                    options,
                    connected_kernel_release,
                )
                .map_err(DomainError::InvalidOperation)?;
                *readiness_for_operation
                    .lock()
                    .map_err(|_| DomainError::Internal("ROOT 预检结果锁不可用。".to_string()))? =
                    Some(result);
                context.report_progress_monotonic(1.0);
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;
    let result = readiness
        .lock()
        .map_err(|_| "ROOT 预检结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "ROOT 预检未产生结果。".to_string());
    result
}

#[tauri::command]
pub async fn root_install_manager(
    state: State<'_, AppState>,
    manager: RootManager,
) -> Result<RootManagerInstallDto, String> {
    let lease = state
        .session_capabilities
        .capture()
        .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())?;
    let capability_scope = state.session_capabilities.clone();
    let device_runtime = state.device_runtime.clone();
    let app_root = nwflash_windows::bundled_resource_root();
    let result = Arc::new(Mutex::new(None));
    let result_for_operation = result.clone();

    state
        .operation_coordinator
        .run_async(
            OperationKind::Installing,
            "安装 ROOT 管理器",
            move |context, cancellation| async move {
                let serial = device_runtime
                    .active_adb_serial()
                    .map_err(DomainError::DeviceUnavailable)?;
                let installation = install_root_manager_core(
                    manager,
                    serial,
                    app_root,
                    &context,
                    cancellation,
                    0.0,
                    1.0,
                )
                .await?;
                if !capability_scope.is_current(lease) {
                    return Err(DomainError::InvalidOperation(
                        ROOT_CAPABILITY_UNAVAILABLE.to_string(),
                    ));
                }
                *result_for_operation.lock().map_err(|_| {
                    DomainError::Internal("ROOT 管理器结果锁不可用。".to_string())
                })? = Some(installation);
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;

    let installation = result
        .lock()
        .map_err(|_| "ROOT 管理器结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "ROOT 管理器安装未产生结果。".to_string());
    installation
}

#[tauri::command]
pub async fn root_patch_vivo_ksu(
    state: State<'_, AppState>,
    options: RootVivoKsuPatchOptionsDto,
) -> Result<RootPatchedArtifactDto, String> {
    let lease = state
        .session_capabilities
        .capture()
        .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())?;
    let device_runtime = state.device_runtime.clone();
    let (source, partition) = state
        .root_image_runtime
        .get_boot_with_target_with_lease(lease, &options.init_boot_id)?;
    let app_root = nwflash_windows::bundled_resource_root();
    let artifacts = state.root_patched_artifacts.clone();
    let result = Arc::new(Mutex::new(None));
    let result_for_operation = result.clone();

    state
        .operation_coordinator
        .run_async(
            OperationKind::Hashing,
            format!("修补 Vivo KSU {partition}"),
            move |context, cancellation| async move {
                let serial = device_runtime
                    .active_adb_serial()
                    .map_err(DomainError::DeviceUnavailable)?;
                let manager = options.manager.unwrap_or(RootManager::VivoKsu);
                let partition = partition.clone();
                let artifact = patch_vivo_ksu_core(
                    lease,
                    manager,
                    serial,
                    source,
                    &partition,
                    options.use_automatic_kmi,
                    options.selected_kmi,
                    app_root,
                    artifacts,
                    &context,
                    cancellation,
                    0.0,
                    1.0,
                )
                .await?;
                *result_for_operation
                    .lock()
                    .map_err(|_| DomainError::Internal("ROOT 修补结果锁不可用。".to_string()))? =
                    Some(artifact);
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;

    let artifact = result
        .lock()
        .map_err(|_| "ROOT 修补结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "ROOT 修补未产生工件。".to_string());
    artifact
}

#[tauri::command]
pub async fn root_patch_official_vendor_boot(
    state: State<'_, AppState>,
    options: RootOfficialVendorBootPatchOptionsDto,
) -> Result<RootPatchedArtifactDto, String> {
    let lease = state
        .session_capabilities
        .capture()
        .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())?;
    let device_runtime = state.device_runtime.clone();
    let source = state
        .root_image_runtime
        .get_selection_with_lease(lease, RootImageKind::VendorBoot, &options.vendor_boot_id)?
        .image;
    let app_root = nwflash_windows::bundled_resource_root();
    let artifacts = state.root_patched_artifacts.clone();
    let result = Arc::new(Mutex::new(None));
    let result_for_operation = result.clone();
    state
        .operation_coordinator
        .run_async(
            OperationKind::Hashing,
            "修补官方 KernelSU vendor_boot",
            move |context, cancellation| async move {
                let serial = device_runtime
                    .active_adb_serial()
                    .map_err(DomainError::DeviceUnavailable)?;
                let artifact = patch_official_vendor_boot_core(
                    lease,
                    serial,
                    source,
                    app_root,
                    artifacts,
                    &context,
                    cancellation,
                    0.0,
                    1.0,
                )
                .await?;
                *result_for_operation
                    .lock()
                    .map_err(|_| DomainError::Internal("ROOT 修补结果锁不可用。".to_string()))? =
                    Some(artifact);
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;
    let artifact = result
        .lock()
        .map_err(|_| "ROOT 修补结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "ROOT 修补未产生工件。".to_string());
    artifact
}

#[tauri::command]
pub fn root_prepare_patched_artifact_flash(
    state: State<'_, AppState>,
    artifact_id: String,
) -> Result<RootPatchedFlashConfirmationDto, String> {
    let lease = state
        .session_capabilities
        .capture()
        .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())?;
    let artifact = state
        .root_patched_artifacts
        .get_with_lease(lease, &artifact_id)?;
    let plan = crate::commands::quick_flash::build_preset_execution_plan(
        &state.device_runtime,
        &artifact.image.path,
        artifact.partition,
    )?;
    let partition = artifact.partition.partition_name().to_string();
    let task_count = plan.tasks.len();
    state
        .root_patched_artifacts
        .prepare_flash_with_lease(lease, artifact_id, plan)?;
    Ok(RootPatchedFlashConfirmationDto {
        partition,
        task_count,
    })
}

#[tauri::command]
pub async fn root_execute_patched_artifact_flash(
    state: State<'_, AppState>,
    artifact_id: String,
) -> Result<crate::commands::quick_flash::CommandExecutionResultDto, String> {
    root_execute_patched_artifact_flash_inner(&state, artifact_id).await
}

async fn root_execute_patched_artifact_flash_inner(
    state: &AppState,
    artifact_id: String,
) -> Result<crate::commands::quick_flash::CommandExecutionResultDto, String> {
    let capability_scope = state.session_capabilities.clone();
    let root_patched_artifacts = state.root_patched_artifacts.clone();
    crate::commands::quick_flash::quick_flash_execute_with_plan_provider(
        state,
        "快速刷写 ROOT 修补镜像".to_string(),
        move || {
            let lease = capability_scope
                .capture()
                .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())?;
            let plan =
                root_patched_artifacts.take_prepared_flash_with_lease(lease, &artifact_id)?;
            Ok(crate::commands::quick_flash::QuickFlashExecutionRequest {
                plan,
                auto_reboot: false,
                switch_to_slot: None,
            })
        },
    )
    .await
}

#[tauri::command]
pub async fn root_run_automatic(
    state: State<'_, AppState>,
    options: RootAutomaticOptionsDto,
) -> Result<RootAutomaticResultDto, String> {
    let lease = state
        .session_capabilities
        .capture()
        .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())?;
    let device_runtime = state.device_runtime.clone();
    let image_runtime = state.root_image_runtime.clone();
    let artifacts = state.root_patched_artifacts.clone();
    let app_root = nwflash_windows::bundled_resource_root();
    let result = Arc::new(Mutex::new(None));
    let result_for_operation = result.clone();

    state
        .operation_coordinator
        .run_async(
            OperationKind::Installing,
            "ROOT 自动流程",
            move |context, cancellation| async move {
                let mut selection = image_runtime
                    .take_automatic_with_lease(lease, options)
                    .map_err(DomainError::InvalidOperation)?;
                let manager = selection.manager;
                let stage_plan = automatic_root_stage_plan(manager);
                let mut artifact_ids = Vec::with_capacity(stage_plan.len().saturating_sub(2));

                for (index, stage) in stage_plan.iter().copied().enumerate() {
                    let serial = device_runtime
                        .active_adb_serial()
                        .map_err(DomainError::DeviceUnavailable)?;
                    let progress_base = index as f64 / stage_plan.len() as f64;
                    let progress_span = 1.0 / stage_plan.len() as f64;
                    match stage {
                        AutomaticRootStage::InstallManager => {
                            context.report_stage_with_kind(
                                format!("ROOT 自动流程: 正在安装 {}", manager.label()),
                                OperationKind::Installing,
                            );
                            install_root_manager_core(
                                manager,
                                serial.clone(),
                                app_root.clone(),
                                &context,
                                cancellation.clone(),
                                progress_base,
                                progress_span,
                            )
                            .await?;
                        }
                        AutomaticRootStage::PatchInitBoot => {
                            context.report_stage_with_kind(
                                format!(
                                    "ROOT 自动流程: 正在修补 {}",
                                    selection.boot_partition_name
                                ),
                                OperationKind::Hashing,
                            );
                            let boot_partition_name = selection.boot_partition_name.clone();
                            let artifact = patch_vivo_ksu_core(
                                lease,
                                manager,
                                serial.clone(),
                                selection.init_boot.clone(),
                                &boot_partition_name,
                                selection.use_automatic_kmi,
                                selection.selected_kmi.clone(),
                                app_root.clone(),
                                artifacts.clone(),
                                &context,
                                cancellation.clone(),
                                progress_base,
                                progress_span,
                            )
                            .await?;
                            artifact_ids.push(artifact.artifact_id);
                        }
                        AutomaticRootStage::PatchVendorBoot => {
                            context.report_stage_with_kind(
                                "ROOT 自动流程: 正在修补 vendor_boot",
                                OperationKind::Hashing,
                            );
                            let source = selection.vendor_boot.take().ok_or_else(|| {
                                DomainError::InvalidOperation(
                                    "官方 KernelSU 全自动流程需要当前 vendor_boot 镜像。"
                                        .to_string(),
                                )
                            })?;
                            let artifact = patch_official_vendor_boot_core(
                                lease,
                                serial.clone(),
                                source,
                                app_root.clone(),
                                artifacts.clone(),
                                &context,
                                cancellation.clone(),
                                progress_base,
                                progress_span,
                            )
                            .await?;
                            artifact_ids.push(artifact.artifact_id);
                        }
                        AutomaticRootStage::FlashFastbootd => {
                            context.report_stage_with_kind(
                                "ROOT 自动流程: 正在等待并刷写 ROOT 镜像",
                                OperationKind::Flashing,
                            );
                            let source = automatic_root_flash_source_with_lease(
                                &artifacts,
                                lease,
                                manager,
                                &artifact_ids,
                            )
                            .map_err(DomainError::InvalidOperation)?;
                            let build_options = SafeFlashBuildOptions {
                                serial: serial.clone(),
                                is_safe_flash: false,
                                is_keep_root: false,
                                wipe_data: false,
                                wipe_data_image_path: None,
                                slot_mode: SafeFlashSlotMode::CurrentSlot,
                                current_slot: None,
                            };
                            let stage_context = context.clone();
                            let progress_context = context.clone();
                            let stage_cancellation = cancellation.clone();
                            let execution = task::spawn_blocking(move || {
                                SafeFlashExecutionService::system().execute(
                                    build_root_automatic_execution_request(
                                        &source,
                                        &build_options,
                                        &serial,
                                    ),
                                    || stage_cancellation.is_cancelled(),
                                    |stage| stage_context.report_stage(stage),
                                    |progress| {
                                        report_root_subprogress(
                                            &progress_context,
                                            progress_base,
                                            progress_span,
                                            progress,
                                        )
                                    },
                                )
                            })
                            .await
                            .map_err(|error| {
                                DomainError::Internal(format!("ROOT 自动刷写调度失败：{error}"))
                            })??;
                            *result_for_operation.lock().map_err(|_| {
                                DomainError::Internal("ROOT 自动刷写结果锁不可用。".to_string())
                            })? = Some(execution);
                        }
                    }
                }
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;

    let execution = result
        .lock()
        .map_err(|_| "ROOT 自动刷写结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "ROOT 自动刷写未返回结果。".to_string())?;
    Ok(RootAutomaticResultDto {
        flashed_partition_count: execution.flashed_partition_count,
        command_count: execution.command_count,
        status: "ROOT 全自动流程已完成。".to_string(),
    })
}

#[tauri::command]
pub async fn root_select_image(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    kind: RootImageKind,
) -> Result<RootImageSelectionDto, String> {
    let lease = state
        .session_capabilities
        .capture()
        .map_err(|_| ROOT_CAPABILITY_UNAVAILABLE.to_string())?;
    let path = select_root_image(&app_handle).await?;
    let image_runtime = state.root_image_runtime.clone();
    let inspected =
        inspect_root_image_through_coordinator(&state.operation_coordinator, path).await?;
    image_runtime.replace_with_target(lease, kind, inspected, kind.label().to_string())
}

async fn inspect_root_image_through_coordinator(
    coordinator: &OperationCoordinator,
    path: PathBuf,
) -> Result<FlashImageInfo, String> {
    let image = Arc::new(Mutex::new(None));
    let image_for_operation = image.clone();
    coordinator
        .run_async(
            OperationKind::Hashing,
            "检查 ROOT 镜像",
            move |context, cancellation| async move {
                context.report_stage("正在检查所选 ROOT 镜像");
                let inspect_cancellation = cancellation.clone();
                let inspected = task::spawn_blocking(move || {
                    if inspect_cancellation.is_cancelled() {
                        return Err("ROOT 镜像检查已取消。".to_string());
                    }
                    inspect_root_image(&path)
                })
                .await
                .map_err(|_| DomainError::Internal("ROOT 镜像检查任务已中断。".to_string()))?
                .map_err(DomainError::InvalidOperation)?;
                context.report_stage("ROOT 镜像检查完成。");
                context.report_progress(1.0);
                *image_for_operation.lock().map_err(|_| {
                    DomainError::Internal("ROOT 镜像检查结果锁不可用。".to_string())
                })? = Some(inspected);
                Ok(())
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    let result = image
        .lock()
        .map_err(|_| "ROOT 镜像检查结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "ROOT 镜像检查未产生结果。".to_string())?;
    Ok(result)
}

async fn select_root_image(app_handle: &AppHandle) -> Result<std::path::PathBuf, String> {
    let (sender, receiver) = oneshot::channel();
    app_handle
        .dialog()
        .file()
        .add_filter("ROOT 镜像", &["img", "bin"])
        .pick_file(move |selected| {
            let _ = sender.send(selected.map(|path| path.into_path()));
        });
    receiver
        .await
        .map_err(|_| "ROOT 镜像选择窗口已关闭。".to_string())?
        .transpose()
        .map_err(|_| "无法读取所选 ROOT 镜像。".to_string())?
        .ok_or_else(|| "用户取消选择 ROOT 镜像。".to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        io::Write,
        path::{Path, PathBuf},
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        automatic_root_flash_source, automatic_root_stage_plan, build_adb_kernel_release_command,
        build_manager_launch_command, build_manager_package_verification_command,
        build_root_automatic_execution_request, build_vendor_boot_cleanup_command,
        build_vendor_boot_module_update_command, build_vendor_boot_repack_pull_commands,
        build_vendor_boot_setup_commands, build_vivo_ksu_patch_commands,
        finalize_vendor_boot_workflow, inspect_root_image, inspect_root_image_through_coordinator,
        manager_resource_key, parse_kernel_release, publish_root_patch_candidate,
        quick_flash_partition_from_name, root_execute_patched_artifact_flash_inner,
        root_preflight_from_runtime, root_preflight_response, AutomaticRootStage,
        RootAutomaticOptionsDto, RootImageKind, RootImageRuntime,
        RootOfficialVendorBootPatchOptionsDto, RootPatchedArtifactRuntime, RootPreflightOptionsDto,
        RootVivoKsuPatchOptionsDto,
    };
    use crate::{session_capabilities::SessionCapabilityScope, AppState};
    use futures::future::BoxFuture;
    use nwflash_application::{
        OperationAuthorization, OperationCoordinator, OperationCoordinatorError,
        OperationPermissionGate, RootManager, RootPatchPreflightRequest, SafeFlashBuildOptions,
        SafeFlashPreparedSource,
    };
    use nwflash_domain::{
        DomainError, FlashImageInfo, OperationKind, PartitionExecutionPlan, PartitionOperationKind,
        PartitionTask, PartitionTransportKind, QuickFlashPartition,
    };
    use tokio::sync::Notify;

    struct BlockingRootAuthorization {
        entered: Arc<Notify>,
        release: Arc<Notify>,
        authorization: OperationAuthorization,
    }

    impl OperationPermissionGate for BlockingRootAuthorization {
        fn authorize(
            &self,
            _operation: OperationKind,
            _title: String,
        ) -> BoxFuture<'static, Result<OperationAuthorization, DomainError>> {
            let entered = self.entered.clone();
            let release = self.release.clone();
            let authorization = self.authorization.clone();
            Box::pin(async move {
                entered.notify_one();
                release.notified().await;
                Ok(authorization)
            })
        }
    }

    fn temporary_root_patch_fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "nwflash-root-patched-artifact-runtime-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("ROOT patch fixture should be created");
        root
    }

    fn prepared_root_patch_plan(image: &Path) -> PartitionExecutionPlan {
        PartitionExecutionPlan {
            serial: "DEVICE-A".to_string(),
            transport: PartitionTransportKind::Fastboot,
            operation: PartitionOperationKind::Write,
            tasks: vec![PartitionTask {
                partition_name: "init_boot".to_string(),
                device_path: String::new(),
                image_path: Some(image.to_string_lossy().into_owned()),
                output_path: None,
                size_bytes: Some(4),
            }],
        }
    }

    fn activated_root_image_runtime() -> RootImageRuntime {
        let scope = Arc::new(SessionCapabilityScope::new());
        scope.activate();
        RootImageRuntime::with_scope(scope)
    }

    fn activated_root_artifact_runtime() -> RootPatchedArtifactRuntime {
        let scope = Arc::new(SessionCapabilityScope::new());
        scope.activate();
        RootPatchedArtifactRuntime::with_scope(scope)
    }

    fn prepared_root_flash_is_present(
        runtime: &RootPatchedArtifactRuntime,
        artifact_id: &str,
    ) -> bool {
        runtime
            .state
            .lock()
            .expect("root patched artifact runtime lock should not be poisoned")
            .prepared_flash
            .as_ref()
            .is_some_and(|prepared| prepared.artifact_id == artifact_id)
    }

    #[test]
    fn stale_root_image_and_prepared_artifact_are_rejected_after_reactivation() {
        let scope = Arc::new(SessionCapabilityScope::new());
        let lease = scope.activate();
        let images = RootImageRuntime::with_scope(scope.clone());
        let artifacts = RootPatchedArtifactRuntime::with_scope(scope.clone());
        let root = temporary_root_patch_fixture();
        let image_path = root.join("selected-init_boot.img");
        fs::write(&image_path, b"AAAA").expect("external selected image fixture");
        let image = FlashImageInfo {
            path: image_path.to_string_lossy().into_owned(),
            size_bytes: 4,
        };

        let selection = images
            .replace_with_target(
                lease,
                RootImageKind::InitBoot,
                image.clone(),
                "init_boot".to_string(),
            )
            .expect("current lease should publish a ROOT image");
        let artifact = artifacts
            .replace_with_ownership(
                lease,
                RootImageKind::InitBoot,
                image,
                QuickFlashPartition::InitBoot,
                None,
            )
            .expect("current lease should publish a ROOT artifact");
        artifacts
            .prepare_flash_with_lease(
                lease,
                artifact.artifact_id.clone(),
                prepared_root_patch_plan(&image_path),
            )
            .expect("current lease should publish a prepared ROOT flash plan");

        scope.invalidate(|| {});
        scope.activate();

        assert!(images.get(RootImageKind::InitBoot, &selection.id).is_err());
        assert!(artifacts
            .take_prepared_flash(&artifact.artifact_id)
            .is_err());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn root_clear_owned_collects_artifact_staging_without_claiming_selected_image() {
        let scope = Arc::new(SessionCapabilityScope::new());
        let lease = scope.activate();
        let images = RootImageRuntime::with_scope(scope.clone());
        let artifacts = RootPatchedArtifactRuntime::with_scope(scope.clone());
        let root = temporary_root_patch_fixture();
        let external_image = root.join("external-init_boot.img");
        fs::write(&external_image, b"external").expect("external image fixture");
        images
            .replace_with_target(
                lease,
                RootImageKind::InitBoot,
                FlashImageInfo {
                    path: external_image.to_string_lossy().into_owned(),
                    size_bytes: 8,
                },
                "init_boot".to_string(),
            )
            .expect("current lease should publish the external selection");

        let owned_staging = root.join("owned-artifact");
        fs::create_dir_all(&owned_staging).expect("owned staging fixture");
        let patched_image = owned_staging.join("patched.img");
        fs::write(&patched_image, b"patched").expect("patched image fixture");
        artifacts
            .replace_owned(
                lease,
                RootImageKind::InitBoot,
                FlashImageInfo {
                    path: patched_image.to_string_lossy().into_owned(),
                    size_bytes: 7,
                },
                QuickFlashPartition::InitBoot,
                owned_staging.clone(),
            )
            .expect("current lease should publish the owned artifact");

        let (image_roots, artifact_roots) =
            scope.invalidate(|| (images.clear_owned(), artifacts.clear_owned()));

        assert!(image_roots.is_empty());
        assert_eq!(artifact_roots, vec![owned_staging.clone()]);
        for owned_root in artifact_roots {
            fs::remove_dir_all(owned_root).expect("caller should delete returned owned roots");
        }
        assert!(external_image.exists());
        assert!(!owned_staging.exists());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn app_state_root_revocation_deletes_owned_staging_but_not_selected_external_image() {
        let state = AppState::new();
        let lease = state.session_capabilities.activate();
        let root = temporary_root_patch_fixture();
        let external_image = root.join("external-init_boot.img");
        fs::write(&external_image, b"external").expect("external image fixture");
        let selection = state
            .root_image_runtime
            .replace_with_target(
                lease,
                RootImageKind::InitBoot,
                FlashImageInfo {
                    path: external_image.to_string_lossy().into_owned(),
                    size_bytes: 8,
                },
                "init_boot".to_string(),
            )
            .expect("current lease should publish the external image selection");

        let owned_staging = root.join("owned-artifact");
        fs::create_dir_all(&owned_staging).expect("owned staging fixture");
        let patched_image = owned_staging.join("patched.img");
        fs::write(&patched_image, b"patched").expect("patched image fixture");
        state
            .root_patched_artifacts
            .replace_with_ownership(
                lease,
                RootImageKind::InitBoot,
                FlashImageInfo {
                    path: patched_image.to_string_lossy().into_owned(),
                    size_bytes: 7,
                },
                QuickFlashPartition::InitBoot,
                Some(owned_staging.clone()),
            )
            .expect("current lease should publish the owned artifact");
        let idle_lease = state
            .operation_coordinator
            .try_acquire_idle()
            .expect("idle state should grant teardown admission");

        state.revoke_root_capabilities(&idle_lease);

        assert!(state.session_capabilities.capture().is_err());
        assert!(state
            .root_image_runtime
            .get(RootImageKind::InitBoot, &selection.id)
            .is_err());
        assert!(external_image.exists());
        assert!(!owned_staging.exists());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn rejected_stale_artifact_commit_deletes_only_its_candidate_staging() {
        let scope = Arc::new(SessionCapabilityScope::new());
        let lease = scope.activate();
        let images = RootImageRuntime::with_scope(scope.clone());
        let artifacts = RootPatchedArtifactRuntime::with_scope(scope.clone());
        let root = temporary_root_patch_fixture();
        let external_image = root.join("external-init_boot.img");
        fs::write(&external_image, b"external").expect("external image fixture");
        images
            .replace_with_target(
                lease,
                RootImageKind::InitBoot,
                FlashImageInfo {
                    path: external_image.to_string_lossy().into_owned(),
                    size_bytes: 8,
                },
                "init_boot".to_string(),
            )
            .expect("current lease should publish the external image selection");
        let candidate_staging = root.join("late-candidate");
        fs::create_dir_all(&candidate_staging).expect("candidate staging fixture");
        let patched_image = candidate_staging.join("patched.img");
        fs::write(&patched_image, b"patched").expect("patched image fixture");
        scope.invalidate(|| {});
        let result = artifacts.replace_owned(
            lease,
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: patched_image.to_string_lossy().into_owned(),
                size_bytes: 7,
            },
            QuickFlashPartition::InitBoot,
            candidate_staging.clone(),
        );

        assert!(result.is_err());
        assert!(!candidate_staging.exists());
        assert!(external_image.exists());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn automatic_root_consumption_rejects_a_replacement_between_snapshot_and_take() {
        let scope = Arc::new(SessionCapabilityScope::new());
        let lease = scope.activate();
        let runtime = RootImageRuntime::with_scope(scope);
        let root = temporary_root_patch_fixture();
        let initial_path = root.join("initial.img");
        let replacement_path = root.join("replacement.img");
        fs::write(&initial_path, b"initial").expect("initial image fixture");
        fs::write(&replacement_path, b"replacement").expect("replacement image fixture");
        let initial = runtime
            .replace_with_target(
                lease,
                RootImageKind::InitBoot,
                FlashImageInfo {
                    path: initial_path.to_string_lossy().into_owned(),
                    size_bytes: 7,
                },
                "init_boot".to_string(),
            )
            .expect("initial selection should publish");
        let initial_id = initial.id.clone();
        let mut replacement_id = None;

        let result = runtime.take_automatic_with_lease_and_observer(
            lease,
            RootAutomaticOptionsDto {
                manager: RootManager::VivoKsu,
                init_boot_id: initial.id,
                vendor_boot_id: None,
                use_automatic_kmi: false,
                selected_kmi: Some("android14-6.1".to_string()),
            },
            |init_boot, vendor_boot| {
                assert_eq!(init_boot.id, initial_id);
                assert!(vendor_boot.is_none());
                let replacement = runtime.replace_with_target(
                    lease,
                    RootImageKind::InitBoot,
                    FlashImageInfo {
                        path: replacement_path.to_string_lossy().into_owned(),
                        size_bytes: 11,
                    },
                    "init_boot".to_string(),
                )?;
                replacement_id = Some(replacement.id);
                Ok(())
            },
        );

        assert!(result.is_err());
        assert!(runtime
            .get(
                RootImageKind::InitBoot,
                replacement_id
                    .as_deref()
                    .expect("replacement should publish during verification"),
            )
            .is_ok());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn prepared_root_plan_rejects_its_old_epoch_without_artifact_lookup() {
        let scope = Arc::new(SessionCapabilityScope::new());
        let lease = scope.activate();
        let runtime = RootPatchedArtifactRuntime::with_scope(scope.clone());
        let artifact = runtime.replace(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: r"C:\private\root-stage\patched-init_boot.img".to_string(),
                size_bytes: 4,
            },
            QuickFlashPartition::InitBoot,
        );
        runtime
            .prepare_flash_with_lease(
                lease,
                artifact.artifact_id.clone(),
                prepared_root_patch_plan(Path::new(r"C:\private\root-stage\patched-init_boot.img")),
            )
            .expect("current epoch should publish prepared plan");

        scope.invalidate(|| {});
        let current = scope.activate();

        assert!(runtime
            .take_prepared_flash_with_lease(current, &artifact.artifact_id)
            .is_err());
    }

    #[test]
    fn automatic_root_execution_uses_the_current_fastbootd_target() {
        let source = SafeFlashPreparedSource {
            staging_root: None,
            partitions: Vec::new(),
            wipe_data_image_path: None,
            has_block_based_content: false,
        };
        let options = SafeFlashBuildOptions {
            serial: "ROOT-DEVICE".to_string(),
            is_safe_flash: false,
            is_keep_root: false,
            wipe_data: false,
            wipe_data_image_path: None,
            slot_mode: nwflash_domain::SafeFlashSlotMode::CurrentSlot,
            current_slot: None,
        };

        let request =
            build_root_automatic_execution_request(&source, &options, options.serial.as_str());

        assert!(request.transition_to_fastbootd);
        assert_eq!(request.options.serial, "ROOT-DEVICE");
        assert_eq!(request.serial, "ROOT-DEVICE");
    }

    #[test]
    fn root_preflight_returns_only_safe_readiness_metadata() {
        let response = root_preflight_response(RootPatchPreflightRequest {
            manager: RootManager::VivoKsu,
            init_boot: Some(FlashImageInfo {
                path: "C:\\private\\init_boot.img".to_string(),
                size_bytes: 1024,
            }),
            vendor_boot: None,
            use_automatic_kmi: true,
            connected_kernel_release: Some("6.1.75-android14".to_string()),
            selected_kmi: None,
        })
        .expect("valid Vivo KSU preflight should succeed");

        assert!(response.can_patch);
        assert!(response.can_run_automatic);
        assert_eq!(response.effective_kmi, "android14-6.1");
        assert!(!serde_json::to_string(&response)
            .expect("response should serialize")
            .contains("private"));
    }

    #[test]
    fn root_image_runtime_returns_an_opaque_handle_and_invalidates_replaced_paths() {
        let runtime = activated_root_image_runtime();
        let first = runtime.replace_default(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: "C:\\private\\first-init_boot.img".to_string(),
                size_bytes: 1024,
            },
        );
        let replacement = runtime.replace_default(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: "C:\\private\\second-init_boot.img".to_string(),
                size_bytes: 2048,
            },
        );

        let dto = serde_json::to_string(&replacement).expect("selection DTO should serialize");
        assert!(!dto.contains("private"));
        assert!(runtime.get(RootImageKind::InitBoot, &first.id).is_err());
        assert_eq!(
            runtime
                .get(RootImageKind::InitBoot, &replacement.id)
                .expect("current opaque ID should resolve internally")
                .size_bytes,
            2048
        );
    }

    #[test]
    fn root_image_selection_remains_usable_after_the_current_device_changes() {
        let runtime = activated_root_image_runtime();
        let original_target = "DEVICE-A";
        let selection = runtime.replace_default(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: r"C:\private\device-a-init_boot.img".to_string(),
                size_bytes: 1024,
            },
        );
        let current_target = "DEVICE-B";
        assert_ne!(current_target, original_target);

        let stored = runtime
            .get_selection(RootImageKind::InitBoot, &selection.id)
            .expect("a current opaque ROOT image must be target-neutral");
        assert_eq!(stored.image.size_bytes, 1024);
        assert!(!format!("{stored:?}").contains("device_serial"));
    }

    #[test]
    fn selected_root_image_allows_same_size_changed_bytes_before_use() {
        let root = std::env::temp_dir().join(format!(
            "nwflash-root-target-neutral-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("test root should be created");
        let image_path = root.join("init_boot.img");
        fs::write(&image_path, b"AAAA").expect("image should be written");

        let runtime = activated_root_image_runtime();
        let image = inspect_root_image(&image_path).expect("image should be inspected");
        let selection = runtime.replace_default(RootImageKind::InitBoot, image);
        fs::write(&image_path, b"BBBB").expect("same-size replacement should be written");

        let resolved = runtime
            .get(RootImageKind::InitBoot, &selection.id)
            .expect("a current opaque selection must not depend on historical file bytes");
        assert_eq!(resolved.path, image_path.to_string_lossy());
        assert_eq!(resolved.size_bytes, 4);
        fs::remove_dir_all(root).expect("test root should be removed");
    }

    #[test]
    fn root_image_runtime_boot_slot_carries_cloud_extraction_partition_name() {
        let runtime = activated_root_image_runtime();
        // 本地选择默认 init_boot。
        let local = runtime.replace_default(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: "C:\\private\\local-init_boot.img".to_string(),
                size_bytes: 1024,
            },
        );
        let (_, local_partition) = runtime
            .get_boot_with_target(&local.id)
            .expect("local init_boot selection should resolve");
        assert_eq!(local_partition, "init_boot");

        // 云端提取的 boot 槽位可能是 boot 分区名。
        let cloud = runtime.replace(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: "C:\\private\\cloud-boot.img".to_string(),
                size_bytes: 2048,
            },
            "boot".to_string(),
        );
        let (_, cloud_partition) = runtime
            .get_boot_with_target(&cloud.id)
            .expect("cloud boot selection should resolve");
        assert_eq!(cloud_partition, "boot");
        // 旧选择被替换为失效。
        assert!(runtime.get(RootImageKind::InitBoot, &local.id).is_err());
        assert_eq!(
            quick_flash_partition_from_name("boot"),
            Some(QuickFlashPartition::Boot)
        );
        assert_eq!(
            quick_flash_partition_from_name("init_boot"),
            Some(QuickFlashPartition::InitBoot)
        );
        assert_eq!(
            quick_flash_partition_from_name("vendor_boot"),
            Some(QuickFlashPartition::VendorBoot)
        );
        assert_eq!(quick_flash_partition_from_name("system"), None);
    }

    #[test]
    fn root_preflight_resolves_only_current_opaque_image_ids() {
        let runtime = activated_root_image_runtime();
        let init_boot = runtime.replace_default(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: "C:\\private\\init_boot.img".to_string(),
                size_bytes: 1024,
            },
        );

        let readiness = root_preflight_from_runtime(
            &runtime,
            RootPreflightOptionsDto {
                manager: RootManager::VivoKsu,
                init_boot_id: Some(init_boot.id),
                vendor_boot_id: None,
                use_automatic_kmi: false,
                selected_kmi: Some("android14-6.1".to_string()),
            },
            None,
        )
        .expect("current opaque init_boot selection should preflight");

        assert!(readiness.can_patch);
        assert!(!serde_json::to_string(&readiness)
            .expect("readiness should serialize")
            .contains("private"));
    }

    #[test]
    fn root_image_inspection_accepts_non_empty_img_and_rejects_empty_or_non_image_files() {
        let root = std::env::temp_dir().join(format!(
            "nwflash-root-image-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("test root should be created");
        let image_path = root.join("init_boot.img");
        let empty_path = root.join("vendor_boot.img");
        let invalid_path = root.join("notes.txt");
        File::create(&image_path)
            .and_then(|mut file| file.write_all(b"image"))
            .expect("image fixture should be written");
        File::create(&empty_path).expect("empty fixture should be written");
        File::create(&invalid_path)
            .and_then(|mut file| file.write_all(b"notes"))
            .expect("invalid fixture should be written");

        assert_eq!(
            inspect_root_image(&image_path)
                .expect("non-empty img should be accepted")
                .size_bytes,
            5
        );
        assert!(inspect_root_image(&empty_path).is_err());
        assert!(inspect_root_image(&invalid_path).is_err());
        fs::remove_dir_all(root).expect("test root should be removed");
    }

    #[test]
    fn automatic_kmi_uses_a_parameterized_adb_kernel_release_command() {
        let command = build_adb_kernel_release_command("ADB-ROOT-1")
            .expect("current ADB serial should build a kernel query");

        assert_eq!(
            command.program,
            nwflash_windows::bundled_platform_tool("adb.exe")
        );
        assert_eq!(
            command.args,
            vec!["-s", "ADB-ROOT-1", "shell", "uname", "-r"]
        );
    }

    #[test]
    fn kernel_release_parser_accepts_one_release_and_rejects_untrusted_output() {
        assert_eq!(
            parse_kernel_release("6.1.75-android14\r\n").unwrap(),
            "6.1.75-android14"
        );
        assert!(parse_kernel_release("6.1.75\nsecond-line").is_err());
        assert!(parse_kernel_release("release;reboot").is_err());
    }

    #[test]
    fn root_preflight_options_reject_frontend_supplied_device_kernel_release() {
        let result = serde_json::from_value::<RootPreflightOptionsDto>(serde_json::json!({
            "manager": "vivoKsu",
            "initBootId": "root-image-init_boot-1",
            "vendorBootId": null,
            "useAutomaticKmi": true,
            "connectedKernelRelease": "6.1.75-android14",
            "selectedKmi": null
        }));

        assert!(result.is_err());
    }

    #[test]
    fn manager_installation_uses_only_fixed_resource_and_package_commands() {
        assert_eq!(manager_resource_key(RootManager::VivoKsu), "KSU");
        assert_eq!(
            manager_resource_key(RootManager::OfficialKernelSu),
            "OfficialKsu"
        );

        let verify = build_manager_package_verification_command("ADB-ROOT-1", "me.inkdye.vivoksu")
            .expect("fixed Vivo KSU package should build a verification command");
        assert_eq!(
            verify.program,
            nwflash_windows::bundled_platform_tool("adb.exe")
        );
        assert_eq!(
            verify.args,
            vec![
                "-s",
                "ADB-ROOT-1",
                "shell",
                "pm",
                "path",
                "me.inkdye.vivoksu",
            ]
        );

        let launch = build_manager_launch_command(
            "ADB-ROOT-1",
            "me.inkdye.vivoksu",
            "me.inkdye.vivoksu.ui.MainActivity",
        )
        .expect("fixed Vivo KSU activity should build a launch command");
        assert_eq!(
            launch.args,
            vec![
                "-s",
                "ADB-ROOT-1",
                "shell",
                "am",
                "start",
                "-n",
                "me.inkdye.vivoksu/me.inkdye.vivoksu.ui.MainActivity",
            ]
        );
    }

    #[test]
    fn vivo_ksu_patch_uses_the_fixed_remote_workspace_and_validated_kmi() {
        let commands = build_vivo_ksu_patch_commands(
            "ADB-ROOT-1",
            Path::new(r"C:\\private\\work\\libksud.so"),
            Path::new(r"C:\\private\\images\\init_boot.img"),
            Path::new(r"C:\\private\\stage\\patched_init_boot.img"),
            "android14-6.1",
            "init_boot",
        )
        .expect("supported KMI should produce controlled patch commands");

        assert_eq!(commands.len(), 4);
        assert_eq!(
            commands[0].program,
            nwflash_windows::bundled_platform_tool("adb.exe")
        );
        assert_eq!(
            commands[0].args,
            vec![
                "-s",
                "ADB-ROOT-1",
                "push",
                r"C:\\private\\work\\libksud.so",
                "/data/local/tmp/vivoksu_libksud.so",
            ]
        );
        assert_eq!(
            commands[2].args,
            vec![
                "-s",
                "ADB-ROOT-1",
                "shell",
                "-T",
                "sh",
                "-c",
                "cd /data/local/tmp && chmod 755 vivoksu_libksud.so && TMPDIR=/data/local/tmp ./vivoksu_libksud.so boot-patch -b vivoksu_init_boot.img --out /data/local/tmp --out-name vivoksu_patched_init_boot.img --partition init_boot --kmi android14-6.1",
            ]
        );
        assert_eq!(
            commands[3].args,
            vec![
                "-s",
                "ADB-ROOT-1",
                "pull",
                "/data/local/tmp/vivoksu_patched_init_boot.img",
                r"C:\\private\\stage\\patched_init_boot.img",
            ]
        );
        assert!(build_vivo_ksu_patch_commands(
            "ADB-ROOT-1",
            Path::new("library.so"),
            Path::new("init_boot.img"),
            Path::new("patched.img"),
            "android14-6.1;reboot",
            "init_boot",
        )
        .is_err());
    }

    #[test]
    fn vivo_ksu_patch_parameterizes_the_boot_partition_name() {
        let commands = build_vivo_ksu_patch_commands(
            "ADB-ROOT-1",
            Path::new(r"C:\\private\\work\\libksud.so"),
            Path::new(r"C:\\private\\images\\boot.img"),
            Path::new(r"C:\\private\\stage\\patched_boot.img"),
            "android14-6.1",
            "boot",
        )
        .expect("boot partition should build controlled patch commands");

        assert_eq!(commands.len(), 4);
        assert_eq!(
            commands[2].args,
            vec![
                "-s",
                "ADB-ROOT-1",
                "shell",
                "-T",
                "sh",
                "-c",
                "cd /data/local/tmp && chmod 755 vivoksu_libksud.so && TMPDIR=/data/local/tmp ./vivoksu_libksud.so boot-patch -b vivoksu_boot.img --out /data/local/tmp --out-name vivoksu_patched_boot.img --partition boot --kmi android14-6.1",
            ]
        );
        assert_eq!(
            commands[3].args,
            vec![
                "-s",
                "ADB-ROOT-1",
                "pull",
                "/data/local/tmp/vivoksu_patched_boot.img",
                r"C:\\private\\stage\\patched_boot.img",
            ]
        );
        // 不支持的 boot 分区名被拒绝。
        assert!(build_vivo_ksu_patch_commands(
            "ADB-ROOT-1",
            Path::new("library.so"),
            Path::new("boot.img"),
            Path::new("patched.img"),
            "android14-6.1",
            "li_boot;reboot",
        )
        .is_err());
    }

    #[test]
    fn root_patched_artifacts_are_opaque_and_bound_to_the_matching_quick_flash_partition() {
        let runtime = activated_root_artifact_runtime();
        let first = runtime.replace(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: r"C:\\private\\root-stage\\patched-init_boot.img".to_string(),
                size_bytes: 2048,
            },
            QuickFlashPartition::InitBoot,
        );
        let replacement = runtime.replace(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: r"C:\\private\\root-stage\\patched-init_boot-new.img".to_string(),
                size_bytes: 4096,
            },
            QuickFlashPartition::InitBoot,
        );

        assert!(first.artifact_id.starts_with("root-patch-"));
        assert_eq!(replacement.partition, "init_boot");
        assert!(!serde_json::to_string(&replacement)
            .expect("patch artifact DTO should serialize")
            .contains("private"));
        assert!(runtime.get(&first.artifact_id).is_err());
        assert_eq!(
            runtime
                .get(&replacement.artifact_id)
                .expect("current opaque patch artifact should resolve internally")
                .partition,
            QuickFlashPartition::InitBoot
        );
    }

    #[test]
    fn root_patch_artifact_lookup_allows_same_size_changed_bytes() {
        let root = temporary_root_patch_fixture();
        let image = root.join("patched.img");
        fs::write(&image, b"AAAA").expect("fixture image");
        let runtime = activated_root_artifact_runtime();
        let dto = runtime.replace(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: image.to_string_lossy().into_owned(),
                size_bytes: 4,
            },
            QuickFlashPartition::InitBoot,
        );

        fs::write(&image, b"BBBB").expect("same-size replacement");

        let artifact = runtime
            .get(&dto.artifact_id)
            .expect("a current opaque artifact must not depend on historical file bytes");
        assert_eq!(artifact.image.path, image.to_string_lossy());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn root_patch_artifact_change_does_not_block_prepared_plan_consumption() {
        let root = temporary_root_patch_fixture();
        let image = root.join("patched.img");
        fs::write(&image, b"AAAA").expect("fixture image");
        let runtime = activated_root_artifact_runtime();
        let dto = runtime.replace(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: image.to_string_lossy().into_owned(),
                size_bytes: 4,
            },
            QuickFlashPartition::InitBoot,
        );
        runtime.prepare_flash(dto.artifact_id.clone(), prepared_root_patch_plan(&image));

        fs::write(&image, b"BBBB").expect("same-size replacement");

        assert_eq!(
            runtime
                .take_prepared_flash(&dto.artifact_id)
                .expect("a current prepared plan must not depend on historical artifact bytes")
                .tasks[0]
                .partition_name,
            "init_boot"
        );
        assert!(runtime.take_prepared_flash(&dto.artifact_id).is_err());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn automatic_root_patched_artifact_source_allows_same_size_changed_bytes() {
        let root = temporary_root_patch_fixture();
        let image = root.join("patched.img");
        fs::write(&image, b"AAAA").expect("fixture image");
        let runtime = activated_root_artifact_runtime();
        let dto = runtime.replace(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: image.to_string_lossy().into_owned(),
                size_bytes: 4,
            },
            QuickFlashPartition::InitBoot,
        );

        fs::write(&image, b"BBBB").expect("same-size replacement");

        let source =
            automatic_root_flash_source(&runtime, RootManager::VivoKsu, &[dto.artifact_id])
                .expect("a current opaque artifact must remain usable by automatic ROOT");
        assert_eq!(source.partitions[0].image_path, image.to_string_lossy());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn automatic_root_patched_artifact_source_accepts_unchanged_current_bytes() {
        let root = temporary_root_patch_fixture();
        let image = root.join("patched.img");
        fs::write(&image, b"AAAA").expect("fixture image");
        let runtime = activated_root_artifact_runtime();
        let dto = runtime.replace(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: image.to_string_lossy().into_owned(),
                size_bytes: 4,
            },
            QuickFlashPartition::InitBoot,
        );

        let source =
            automatic_root_flash_source(&runtime, RootManager::VivoKsu, &[dto.artifact_id])
                .expect("unchanged current artifact should remain usable");

        assert_eq!(source.partitions.len(), 1);
        assert_eq!(source.partitions[0].image_path, image.to_string_lossy());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn root_patch_artifact_remains_usable_after_the_current_device_changes() {
        let runtime = activated_root_artifact_runtime();
        let original_target = "DEVICE-A";
        let artifact = runtime.replace(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: r"C:\private\root-stage\device-a-patched.img".to_string(),
                size_bytes: 2048,
            },
            QuickFlashPartition::InitBoot,
        );
        let current_target = "DEVICE-B";
        assert_ne!(current_target, original_target);

        let stored = runtime
            .get(&artifact.artifact_id)
            .expect("a current opaque ROOT artifact must be target-neutral");
        assert_eq!(stored.image.size_bytes, 2048);
        assert!(!format!("{stored:?}").contains("device_serial"));
    }

    #[test]
    fn automatic_root_flash_source_accepts_current_opaque_init_and_vendor_artifacts_in_order() {
        let runtime = activated_root_artifact_runtime();
        let init_boot = runtime.replace(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: r"C:\\private\\root-stage\\patched-init_boot.img".to_string(),
                size_bytes: 2048,
            },
            QuickFlashPartition::InitBoot,
        );
        let vendor_boot = runtime.replace(
            RootImageKind::VendorBoot,
            FlashImageInfo {
                path: r"C:\\private\\root-stage\\patched-vendor_boot.img".to_string(),
                size_bytes: 4096,
            },
            QuickFlashPartition::VendorBoot,
        );

        let source = automatic_root_flash_source(
            &runtime,
            RootManager::OfficialKernelSu,
            &[init_boot.artifact_id, vendor_boot.artifact_id],
        )
        .expect("current opaque ROOT artifacts should form one automatic flash source");

        assert_eq!(source.partitions.len(), 2);
        assert_eq!(source.partitions[0].partition_name, "init_boot");
        assert_eq!(source.partitions[1].partition_name, "vendor_boot");
        assert!(automatic_root_flash_source(
            &runtime,
            RootManager::VivoKsu,
            &["forged-artifact".to_string()],
        )
        .is_err());
    }

    #[test]
    fn automatic_root_flash_source_accepts_boot_fallback_for_vivo_ksu() {
        let runtime = activated_root_artifact_runtime();
        // 云端提取的 boot 槽位回退到 boot 分区（设备无 init_boot）。
        let boot = runtime.replace(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: r"C:\\private\\root-stage\\patched-boot.img".to_string(),
                size_bytes: 2048,
            },
            QuickFlashPartition::Boot,
        );

        let source =
            automatic_root_flash_source(&runtime, RootManager::VivoKsu, &[boot.artifact_id])
                .expect("boot-fallback artifact should form an automatic Vivo KSU flash source");

        assert_eq!(source.partitions.len(), 1);
        assert_eq!(source.partitions[0].partition_name, "boot");
    }

    #[test]
    fn automatic_root_flash_source_still_rejects_non_boot_first_slots() {
        let runtime = activated_root_artifact_runtime();
        // 造假一个既不是 init_boot 也不是 boot 的工件 -> 自动流程应拒绝。
        let artifact = runtime.replace(
            RootImageKind::VendorBoot,
            FlashImageInfo {
                path: r"C:\\private\\root-stage\\patched-vendor_boot.img".to_string(),
                size_bytes: 4096,
            },
            QuickFlashPartition::VendorBoot,
        );
        assert!(automatic_root_flash_source(
            &runtime,
            RootManager::VivoKsu,
            &[artifact.artifact_id],
        )
        .is_err());
    }

    #[test]
    fn automatic_root_request_enforces_manager_images_and_consumes_current_handles_once() {
        let runtime = activated_root_image_runtime();
        let init_boot = runtime.replace_default(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: r"C:\\private\\images\\init_boot.img".to_string(),
                size_bytes: 2048,
            },
        );

        let missing_vendor = runtime.take_automatic(RootAutomaticOptionsDto {
            manager: RootManager::OfficialKernelSu,
            init_boot_id: init_boot.id.clone(),
            vendor_boot_id: None,
            use_automatic_kmi: false,
            selected_kmi: Some("android14-6.1".to_string()),
        });
        assert!(missing_vendor
            .expect_err("Official KernelSU automatic ROOT requires vendor_boot")
            .contains("vendor_boot"));
        assert!(runtime.get(RootImageKind::InitBoot, &init_boot.id).is_ok());

        let vendor_boot = runtime.replace_default(
            RootImageKind::VendorBoot,
            FlashImageInfo {
                path: r"C:\\private\\images\\vendor_boot.img".to_string(),
                size_bytes: 4096,
            },
        );
        let options = RootAutomaticOptionsDto {
            manager: RootManager::OfficialKernelSu,
            init_boot_id: init_boot.id.clone(),
            vendor_boot_id: Some(vendor_boot.id.clone()),
            use_automatic_kmi: false,
            selected_kmi: Some("android14-6.1".to_string()),
        };
        let selection = runtime
            .take_automatic(options.clone())
            .expect("current manager-bound selections should be consumed");
        assert_eq!(selection.manager, RootManager::OfficialKernelSu);
        assert!(selection.vendor_boot.is_some());
        assert!(runtime
            .take_automatic(options)
            .expect_err("automatic selection replay must be rejected")
            .contains("失效"));
    }

    #[test]
    fn automatic_root_request_rejects_forged_stale_and_cross_manager_handles() {
        let runtime = activated_root_image_runtime();
        let stale = runtime.replace_default(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: r"C:\\private\\images\\stale-init_boot.img".to_string(),
                size_bytes: 1024,
            },
        );
        let current = runtime.replace_default(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: r"C:\\private\\images\\current-init_boot.img".to_string(),
                size_bytes: 2048,
            },
        );
        let vendor = runtime.replace_default(
            RootImageKind::VendorBoot,
            FlashImageInfo {
                path: r"C:\\private\\images\\vendor_boot.img".to_string(),
                size_bytes: 4096,
            },
        );
        let options =
            |init_boot_id: String, vendor_boot_id: Option<String>| RootAutomaticOptionsDto {
                manager: RootManager::VivoKsu,
                init_boot_id,
                vendor_boot_id,
                use_automatic_kmi: true,
                selected_kmi: None,
            };

        assert!(runtime
            .take_automatic(options(stale.id, None))
            .expect_err("replaced selections must be stale")
            .contains("失效"));
        assert!(runtime
            .take_automatic(options("forged-image".to_string(), None))
            .expect_err("forged selections must be rejected")
            .contains("失效"));
        assert!(runtime
            .take_automatic(options(current.id, Some(vendor.id)))
            .expect_err("Vivo KSU must not accept a vendor_boot selection")
            .contains("Vivo KSU"));
    }

    #[test]
    fn automatic_root_transaction_stage_plan_covers_one_complete_manager_workflow() {
        assert_eq!(
            automatic_root_stage_plan(RootManager::VivoKsu),
            &[
                AutomaticRootStage::InstallManager,
                AutomaticRootStage::PatchInitBoot,
                AutomaticRootStage::FlashFastbootd,
            ]
        );
        assert_eq!(
            automatic_root_stage_plan(RootManager::OfficialKernelSu),
            &[
                AutomaticRootStage::InstallManager,
                AutomaticRootStage::PatchInitBoot,
                AutomaticRootStage::PatchVendorBoot,
                AutomaticRootStage::FlashFastbootd,
            ]
        );
    }

    #[test]
    fn automatic_root_browser_contract_rejects_paths_serials_and_artifact_ids() {
        let accepted = serde_json::from_value::<RootAutomaticOptionsDto>(serde_json::json!({
            "manager": "officialKernelSu",
            "initBootId": "root-image-init_boot-1",
            "vendorBootId": "root-image-vendor_boot-1",
            "useAutomaticKmi": false,
            "selectedKmi": "android14-6.1"
        }));
        assert!(accepted.is_ok());

        for forbidden in ["serial", "imagePath", "artifactIds", "managerApkPath"] {
            let mut value = serde_json::json!({
                "manager": "vivoKsu",
                "initBootId": "root-image-init_boot-1",
                "vendorBootId": null,
                "useAutomaticKmi": true,
                "selectedKmi": null
            });
            value[forbidden] = serde_json::json!("forbidden");
            assert!(serde_json::from_value::<RootAutomaticOptionsDto>(value).is_err());
        }
    }

    #[test]
    fn vivo_ksu_patch_options_reject_paths_and_only_accept_the_selected_root_image_id() {
        let accepted = serde_json::from_value::<RootVivoKsuPatchOptionsDto>(serde_json::json!({
            "initBootId": "root-image-init_boot-7",
            "useAutomaticKmi": false,
            "selectedKmi": "android14-6.1"
        }));
        assert!(accepted.is_ok());

        let rejected = serde_json::from_value::<RootVivoKsuPatchOptionsDto>(serde_json::json!({
            "initBootId": "root-image-init_boot-7",
            "useAutomaticKmi": false,
            "selectedKmi": "android14-6.1",
            "apkPath": "C:\\private\\manager.apk"
        }));
        assert!(rejected.is_err());
    }

    #[test]
    fn root_patch_flash_preflight_is_bound_to_one_current_artifact_and_consumed_once() {
        let runtime = activated_root_artifact_runtime();
        let artifact = runtime.replace(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: r"C:\\private\\root-stage\\patched-init_boot.img".to_string(),
                size_bytes: 2048,
            },
            QuickFlashPartition::InitBoot,
        );
        let plan = PartitionExecutionPlan {
            serial: "FAST-ROOT-1".to_string(),
            transport: PartitionTransportKind::Fastboot,
            operation: PartitionOperationKind::Write,
            tasks: vec![PartitionTask {
                partition_name: "init_boot".to_string(),
                device_path: "".to_string(),
                image_path: Some(r"C:\\private\\root-stage\\patched-init_boot.img".to_string()),
                output_path: None,
                size_bytes: Some(2048),
            }],
        };

        runtime.prepare_flash(artifact.artifact_id.clone(), plan.clone());
        assert!(runtime.take_prepared_flash("root-patch-other").is_err());
        assert_eq!(
            runtime
                .take_prepared_flash(&artifact.artifact_id)
                .expect("matching artifact should consume its Root flash preflight")
                .tasks[0]
                .partition_name,
            "init_boot"
        );
        assert!(runtime.take_prepared_flash(&artifact.artifact_id).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn denied_root_patched_flash_preserves_capability_for_authorized_retry() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut state = AppState::new();
        state.operation_coordinator = OperationCoordinator::new(
            None,
            Some(Arc::new(BlockingRootAuthorization {
                entered: entered.clone(),
                release: release.clone(),
                authorization: OperationAuthorization::deny("test denial"),
            })),
            None,
            None,
            None,
        );
        state.session_capabilities.activate();
        let artifact = state.root_patched_artifacts.replace(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: r"C:\test-only\patched-init_boot.img".to_string(),
                size_bytes: 4,
            },
            QuickFlashPartition::InitBoot,
        );
        let mut plan = prepared_root_patch_plan(Path::new(r"C:\test-only\patched-init_boot.img"));
        plan.transport = PartitionTransportKind::Automatic;
        state
            .root_patched_artifacts
            .prepare_flash(artifact.artifact_id.clone(), plan);

        let execution =
            root_execute_patched_artifact_flash_inner(&state, artifact.artifact_id.clone());
        let observe_pending_authorization = async {
            entered.notified().await;
            assert!(prepared_root_flash_is_present(
                &state.root_patched_artifacts,
                &artifact.artifact_id
            ));
            assert!(matches!(
                state.operation_coordinator.try_acquire_idle(),
                Err(OperationCoordinatorError::InProgress)
            ));
            release.notify_one();
        };
        let (denied, ()) = tokio::join!(execution, observe_pending_authorization);

        assert!(denied.is_err());
        assert!(prepared_root_flash_is_present(
            &state.root_patched_artifacts,
            &artifact.artifact_id
        ));

        state.operation_coordinator = OperationCoordinator::default();
        let retry =
            root_execute_patched_artifact_flash_inner(&state, artifact.artifact_id.clone()).await;

        assert!(retry.is_err());
        assert!(!prepared_root_flash_is_present(
            &state.root_patched_artifacts,
            &artifact.artifact_id
        ));
    }

    #[test]
    fn replacing_owned_root_patch_artifacts_only_cleans_the_superseded_owned_staging() {
        let root = std::env::temp_dir().join(format!(
            "nwflash-root-patch-runtime-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        let external = root.join("external");
        let first = root.join("first-owned");
        let second = root.join("second-owned");
        fs::create_dir_all(&external).expect("external fixture should be created");
        fs::create_dir_all(&first).expect("first owned staging should be created");
        fs::create_dir_all(&second).expect("second owned staging should be created");
        let scope = Arc::new(SessionCapabilityScope::new());
        let lease = scope.activate();
        let runtime = RootPatchedArtifactRuntime::with_scope(scope);

        runtime.replace(
            RootImageKind::InitBoot,
            FlashImageInfo {
                path: external.join("external.img").to_string_lossy().into_owned(),
                size_bytes: 1,
            },
            QuickFlashPartition::InitBoot,
        );
        runtime
            .replace_owned(
                lease,
                RootImageKind::InitBoot,
                FlashImageInfo {
                    path: first.join("patched.img").to_string_lossy().into_owned(),
                    size_bytes: 1,
                },
                QuickFlashPartition::InitBoot,
                first.clone(),
            )
            .expect("current lease should publish first owned artifact");
        runtime
            .replace_owned(
                lease,
                RootImageKind::InitBoot,
                FlashImageInfo {
                    path: second.join("patched.img").to_string_lossy().into_owned(),
                    size_bytes: 1,
                },
                QuickFlashPartition::InitBoot,
                second.clone(),
            )
            .expect("current lease should publish second owned artifact");

        assert!(external.is_dir());
        assert!(!first.exists());
        assert!(second.is_dir());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn failed_root_patch_candidate_preserves_prior_owned_artifact_and_prepared_plan() {
        let root = temporary_root_patch_fixture();
        let prior_root = root.join("prior-owned");
        let candidate_root = root.join("failed-candidate-owned");
        let unrelated_root = root.join("unrelated");
        fs::create_dir_all(&prior_root).expect("prior owned staging should be created");
        fs::create_dir_all(&candidate_root).expect("candidate owned staging should be created");
        fs::create_dir_all(&unrelated_root).expect("unrelated directory should be created");
        let prior_image = prior_root.join("patched.img");
        fs::write(&prior_image, b"AAAA").expect("prior owned artifact should be written");
        fs::write(candidate_root.join("patched.img"), b"BBBB")
            .expect("failed candidate should be written");
        let scope = Arc::new(SessionCapabilityScope::new());
        let lease = scope.activate();
        let runtime = RootPatchedArtifactRuntime::with_scope(scope);
        let prior = runtime
            .replace_owned(
                lease,
                RootImageKind::InitBoot,
                FlashImageInfo {
                    path: prior_image.to_string_lossy().into_owned(),
                    size_bytes: 4,
                },
                QuickFlashPartition::InitBoot,
                prior_root.clone(),
            )
            .expect("current lease should publish prior owned artifact");
        runtime.prepare_flash(
            prior.artifact_id.clone(),
            prepared_root_patch_plan(&prior_image),
        );

        let result = publish_root_patch_candidate(
            &runtime,
            lease,
            RootImageKind::InitBoot,
            Err::<FlashImageInfo, _>(DomainError::InvalidOperation(
                "candidate validation failed".to_string(),
            )),
            QuickFlashPartition::InitBoot,
            candidate_root.clone(),
        );

        assert!(matches!(result, Err(DomainError::InvalidOperation(_))));
        assert!(!candidate_root.exists());
        assert!(prior_root.is_dir());
        assert!(unrelated_root.is_dir());
        assert_eq!(
            runtime
                .get(&prior.artifact_id)
                .expect("failed candidate must not replace the prior artifact")
                .image
                .path,
            prior_image.to_string_lossy()
        );
        assert_eq!(
            runtime
                .take_prepared_flash(&prior.artifact_id)
                .expect("failed candidate must not consume the prior prepared plan")
                .tasks[0]
                .partition_name,
            "init_boot"
        );
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn vendor_boot_setup_uses_a_rust_owned_remote_workspace_and_parameterized_adb_calls() {
        let commands = build_vendor_boot_setup_commands(
            "ADB-ROOT-1",
            Path::new(r"C:\\private\\tools\\magiskboot.so"),
            Path::new(r"C:\\private\\images\\vendor_boot.img"),
            "vendor-42",
        )
        .expect("a safe workspace token should build vendor_boot setup commands");

        assert_eq!(commands.len(), 4);
        assert_eq!(
            commands[0].args,
            vec![
                "-s",
                "ADB-ROOT-1",
                "shell",
                "mkdir",
                "-p",
                "/data/local/tmp/nwflash_vendor_boot_vendor-42"
            ]
        );
        assert_eq!(
            commands[1].args,
            vec![
                "-s",
                "ADB-ROOT-1",
                "push",
                r"C:\\private\\tools\\magiskboot.so",
                "/data/local/tmp/nwflash_vendor_boot_vendor-42/magiskboot"
            ]
        );
        assert_eq!(
            commands[2].args,
            vec![
                "-s",
                "ADB-ROOT-1",
                "push",
                r"C:\\private\\images\\vendor_boot.img",
                "/data/local/tmp/nwflash_vendor_boot_vendor-42/vendor_boot.img"
            ]
        );
        assert!(commands[3]
            .args
            .last()
            .is_some_and(|script| script.contains("magiskboot unpack vendor_boot.img")));
        assert!(build_vendor_boot_setup_commands(
            "ADB-ROOT-1",
            Path::new("magiskboot"),
            Path::new("vendor_boot.img"),
            "vendor;reboot",
        )
        .is_err());
    }

    #[test]
    fn vendor_boot_cleanup_only_targets_a_validated_rust_owned_workspace() {
        let cleanup = build_vendor_boot_cleanup_command(
            "ADB-ROOT-1",
            "/data/local/tmp/nwflash_vendor_boot_vendor-42",
        )
        .expect("a Rust-owned vendor_boot workspace should build cleanup");

        assert_eq!(
            cleanup.program,
            nwflash_windows::bundled_platform_tool("adb.exe")
        );
        assert_eq!(
            cleanup.args,
            vec![
                "-s",
                "ADB-ROOT-1",
                "shell",
                "rm",
                "-rf",
                "/data/local/tmp/nwflash_vendor_boot_vendor-42",
            ]
        );
        assert!(build_vendor_boot_cleanup_command("ADB-ROOT-1", "/data/local/tmp").is_err());
        assert!(build_vendor_boot_cleanup_command(
            "ADB-ROOT-1",
            "/data/local/tmp/nwflash_vendor_boot_vendor-42;reboot",
        )
        .is_err());
    }

    #[tokio::test]
    async fn vendor_boot_finalization_attempts_cleanup_and_removes_staging_for_errors_and_cancellation(
    ) {
        let root = std::env::temp_dir().join(format!(
            "nwflash-vendor-boot-finalization-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("fixture root should be created");

        for (name, workflow_error) in [
            (
                "error",
                DomainError::InvalidOperation("patch failed".to_string()),
            ),
            (
                "cancelled",
                DomainError::UserCancelled("patch cancelled".to_string()),
            ),
        ] {
            let staging = root.join(format!("{name}-staging"));
            let cleanup_marker = root.join(format!("{name}-cleanup-attempted"));
            fs::create_dir_all(&staging).expect("owned staging should be created");
            fs::write(staging.join("patched_vendor_boot.img"), b"partial")
                .expect("partial artifact should be written");

            let cleanup_marker_for_attempt = cleanup_marker.clone();
            let result = finalize_vendor_boot_workflow(
                Err::<(), _>(workflow_error.clone()),
                staging.clone(),
                async move {
                    fs::write(cleanup_marker_for_attempt, b"attempted")
                        .expect("cleanup attempt marker should be written");
                    Err(DomainError::ExternalTool("cleanup failed".to_string()))
                },
            )
            .await;

            assert_eq!(
                result.expect_err("the workflow error should remain primary"),
                workflow_error
            );
            assert!(cleanup_marker.is_file());
            assert!(!staging.exists());
        }

        fs::remove_dir_all(root).expect("fixture root should be removed");
    }

    #[test]
    fn vendor_boot_module_update_only_accepts_whitelisted_module_lists() {
        let command = build_vendor_boot_module_update_command(
            "ADB-ROOT-1",
            "/data/local/tmp/nwflash_vendor_boot_vendor-42",
            "lib/modules/6.1.75-android14-gki",
            "modules.softdep",
        )
        .expect("a filtered GKI modules directory and known module list should build");

        assert_eq!(
            command.program,
            nwflash_windows::bundled_platform_tool("adb.exe")
        );
        assert!(command.args.last().is_some_and(|script| script
            .contains("sed -i '/softdep[[:space:]]\\+vr[[:space:]]\\+pre/d' modules.softdep")));
        assert!(build_vendor_boot_module_update_command(
            "ADB-ROOT-1",
            "/data/local/tmp/nwflash_vendor_boot_vendor-42",
            "lib/modules/6.1.75-gki;reboot",
            "modules.load",
        )
        .is_err());
        assert!(build_vendor_boot_module_update_command(
            "ADB-ROOT-1",
            "/data/local/tmp/nwflash_vendor_boot_vendor-42",
            "lib/modules",
            "evil.list",
        )
        .is_err());
    }

    #[test]
    fn vendor_boot_repack_and_pull_only_target_the_owned_workspace_output() {
        let commands = build_vendor_boot_repack_pull_commands(
            "ADB-ROOT-1",
            "/data/local/tmp/nwflash_vendor_boot_vendor-42",
            Path::new(r"C:\\private\\root-stage\\patched_vendor_boot.img"),
        )
        .expect("owned workspace should build repack and pull commands");

        assert_eq!(commands.len(), 2);
        assert!(commands[0]
            .args
            .last()
            .is_some_and(|script| script.contains(
                "magiskboot repack vendor_boot.img 2>&1 && test -f new-boot.img && echo REPACKED"
            )));
        assert_eq!(
            commands[1].args,
            vec![
                "-s",
                "ADB-ROOT-1",
                "pull",
                "/data/local/tmp/nwflash_vendor_boot_vendor-42/new-boot.img",
                r"C:\\private\\root-stage\\patched_vendor_boot.img"
            ]
        );
        assert!(build_vendor_boot_repack_pull_commands(
            "ADB-ROOT-1",
            "/data/local/tmp/other",
            Path::new("patched.img"),
        )
        .is_err());
    }

    #[test]
    fn official_vendor_boot_patch_options_only_accept_the_selected_opaque_image_id() {
        let accepted =
            serde_json::from_value::<RootOfficialVendorBootPatchOptionsDto>(serde_json::json!({
                "vendorBootId": "root-image-vendor_boot-7"
            }));
        assert!(accepted.is_ok());
        let rejected =
            serde_json::from_value::<RootOfficialVendorBootPatchOptionsDto>(serde_json::json!({
                "vendorBootId": "root-image-vendor_boot-7",
                "remoteRoot": "/data/local/tmp/evil"
            }));
        assert!(rejected.is_err());
    }

    struct CapturingUsageReporter {
        entries: std::sync::Mutex<Vec<nwflash_domain::UsageLogEntry>>,
    }

    impl nwflash_application::UsageReporter for CapturingUsageReporter {
        fn record(&self, entry: nwflash_domain::UsageLogEntry) {
            self.entries
                .lock()
                .expect("usage entry capture lock should not be poisoned")
                .push(entry);
        }
    }

    #[tokio::test]
    async fn root_image_inspection_records_usage_details_through_the_coordinator() {
        let root = std::env::temp_dir().join(format!(
            "nwflash-root-select-usage-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("test root should be created");
        let image_path = root.join("init_boot.img");
        fs::write(&image_path, b"AAAA").expect("image should be written");

        let reporter = Arc::new(CapturingUsageReporter {
            entries: std::sync::Mutex::new(Vec::new()),
        });
        let coordinator = OperationCoordinator::new(None, None, Some(reporter.clone()), None, None);
        let inspected = inspect_root_image_through_coordinator(&coordinator, image_path.clone())
            .await
            .expect("coordinated ROOT image inspection should succeed");
        assert!(inspected.size_bytes > 0);

        let entries = reporter
            .entries
            .lock()
            .expect("usage entry capture lock should not be poisoned");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].operation, "Hashing");
        assert_eq!(entries[0].title, "检查 ROOT 镜像");
        assert_eq!(entries[0].status, "success");
        assert!(entries[0]
            .details
            .iter()
            .any(|detail| detail.message == "ROOT 镜像检查完成。"));

        fs::remove_dir_all(&root).expect("test root should be removed");
    }
}
