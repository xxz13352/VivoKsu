//! Infrastructure adapters for network, filesystem, and resource provisioning.

pub mod api_client;
pub mod auth;
pub mod embedded_assets;
pub mod firmware_extract;
pub mod firmware_package;
pub mod operation_log;
pub mod ota_download;
pub mod paths;
pub mod payload_dumper;
pub mod payload_provisioner;
mod pinned_tls;
pub mod preferences;
pub mod remote_assets;
pub mod remote_firmware;
pub mod resource_downloader;
pub mod root_patch;
pub mod root_resources;
pub mod scrcpy_provisioner;
mod session_security;
mod trace_facade;
pub mod trace_http;
pub mod trace_spool;
pub mod trace_uploader;
pub mod usage_reporter;
pub mod vendor_boot;
pub mod version_client;
pub mod vivo_firmware;

pub use api_client::{
    CloudflareClient, CloudflareError, CloudflareResult, HeartbeatResult, IntegrityReportPhase,
    IntegrityReportReason, IntegrityReportRequest, LoginRequest, LoginResult, OnlineSession,
    OperationAuthorization, RomResolveResponse, UpdateRequiredInfo, UsageLogUploadResponse,
    DEFAULT_APP_VERSION, DEFAULT_BASE_URL,
};
pub use auth::{AuthService, AuthSession, HeartbeatAdmission};
pub use embedded_assets::{wipe_data_size_bytes, write_wipe_data_image, EmbeddedAssetError};
pub use firmware_extract::{
    FirmwareExtractionError, FirmwareFormat, FirmwareFormatDetector,
    FirmwarePackageExtractionResult, FirmwarePackageExtractionService,
};
pub use firmware_package::{FirmwarePackageError, FirmwarePackageInspector};
pub use operation_log::OperationLogStore;
pub use ota_download::{
    build_download_target_path, download_to_file, download_to_file_with_cancellation,
    plan_ota_download, staging_download_path, validate_available_space, OtaDiskSpaceProvider,
    OtaDownloadError, OtaDownloadPlan, OtaDownloadPlanningError, OtaDownloadProgress,
    OtaDownloadProgressSink, OtaDownloader, SystemOtaDiskSpaceProvider,
    OTA_DOWNLOAD_MEMORY_CAP_BYTES, OTA_DOWNLOAD_PROGRESS_INTERVAL,
};
pub use paths::{resource_root, try_make_writable};
pub use payload_dumper::{
    collect_payload_extraction_results, collect_required_payload_extraction_results,
    parse_payload_metadata, validate_partition_name, PayloadDumperCommand, PayloadDumperError,
};
pub use payload_provisioner::{PayloadDumperProvisioner, PayloadProvisionError};
#[cfg(debug_assertions)]
pub use pinned_tls::{
    ApiTlsPolicy, PinnedApiClient, SignedPinsetEnvelope, API_HOST, BUILTIN_LEAF_SPKI_PIN,
    BUILTIN_WE1_SPKI_PIN, EMBEDDED_PINSET_VERSION_FLOOR,
};
pub use pinned_tls::{IntegrityFailure, PinsetClaims};
pub use preferences::{ToolPathPreferences, ToolPathSettings};
pub use remote_assets::{
    github_download_url, is_known_manager_key, manager_apk_filename, manager_apk_sha256,
    RemoteAssetSpec, MANAGER_KEY_KSU, MANAGER_KEY_OFFICIAL, PAYLOAD_DUMPER_ASSET_NAME,
    PAYLOAD_DUMPER_EXECUTABLE_NAME, PAYLOAD_DUMPER_SHA256, ROOT_MANAGER_APK_KSU,
    ROOT_MANAGER_APK_OFFICIAL, ROOT_MANAGER_SHA256_KSU, ROOT_MANAGER_SHA256_OFFICIAL,
    SUPPORTED_KERNEL_RELEASE_FAMILIES,
};
pub use resource_downloader::{ProgressSink, RemoteAssetDownloader, ResourceDownloadError};
pub use root_patch::{
    validate_patched_root_image, RootPatchArtifactError, RootPatchArtifactService,
    MAX_ROOT_PATCH_GROWTH_BYTES, ROOT_PATCH_OUTPUT_FOLDER,
};
pub use root_resources::{
    RootResourceError, VivoRootLibrarySpec, VivoRootManagerResource, VivoRootResourceService,
    VivoRootToolResource,
};
pub use scrcpy_provisioner::{ScrcpyProvisionError, ScrcpyProvisioner};
pub use session_security::{compiled_build_id, ProcessIdentity, SecretToken};
pub use trace_http::{
    TraceHttpAck, TraceHttpApiError, TraceHttpError, TraceHttpOutcome, TraceHttpResult,
    TraceHttpUpdateRequired, TraceSafeId, TraceSafeRejectedItem,
};
pub use vendor_boot::resolve_vendor_boot_module_directories;
pub use version_client::{VersionCheckResult, VersionClient};
pub use vivo_firmware::{
    VivoFirmwareEntry, VivoFirmwareError, VivoFirmwareExtractor, VivoFirmwareProgress,
};
