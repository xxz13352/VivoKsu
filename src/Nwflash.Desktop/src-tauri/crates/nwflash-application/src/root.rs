use std::path::Path;

use nwflash_domain::{DomainError, FlashImageInfo};
use nwflash_infrastructure::VivoRootResourceService;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RootManager {
    VivoKsu,
    OfficialKernelSu,
}

impl RootManager {
    pub fn label(self) -> &'static str {
        match self {
            Self::VivoKsu => "Vivo KSU",
            Self::OfficialKernelSu => "官方 KernelSU",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RootPatchPreflightRequest {
    pub manager: RootManager,
    pub init_boot: Option<FlashImageInfo>,
    pub vendor_boot: Option<FlashImageInfo>,
    pub use_automatic_kmi: bool,
    pub connected_kernel_release: Option<String>,
    pub selected_kmi: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootPatchReadiness {
    pub manager_label: String,
    pub effective_kmi: String,
    pub can_patch: bool,
    pub can_run_automatic: bool,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RootService;

impl RootService {
    pub fn new() -> Self {
        Self
    }

    pub fn evaluate_preflight(
        &self,
        request: RootPatchPreflightRequest,
    ) -> Result<RootPatchReadiness, DomainError> {
        let effective_kmi = self.resolve_kmi(&request)?;
        let has_init_boot = request.init_boot.as_ref().is_some_and(valid_root_image);
        let has_vendor_boot = request.vendor_boot.as_ref().is_some_and(valid_root_image);
        let (can_patch, can_run_automatic, summary) = match request.manager {
            RootManager::VivoKsu => (
                has_init_boot,
                has_init_boot,
                if has_init_boot {
                    "已就绪：将修补 init_boot。".to_string()
                } else {
                    "请选择有效的 init_boot .img 或 .bin 镜像。".to_string()
                },
            ),
            RootManager::OfficialKernelSu => {
                let can_patch = has_init_boot || has_vendor_boot;
                let can_run_automatic = has_init_boot && has_vendor_boot;
                let summary = if !can_patch {
                    "请选择有效的 init_boot 或 vendor_boot .img/.bin 镜像。".to_string()
                } else if !can_run_automatic {
                    "全自动需要两份镜像：init_boot 与 vendor_boot。".to_string()
                } else {
                    "已就绪：将修补 init_boot 与 vendor_boot。".to_string()
                };
                (can_patch, can_run_automatic, summary)
            }
        };

        Ok(RootPatchReadiness {
            manager_label: request.manager.label().to_string(),
            effective_kmi,
            can_patch,
            can_run_automatic,
            summary,
        })
    }

    fn resolve_kmi(&self, request: &RootPatchPreflightRequest) -> Result<String, DomainError> {
        if request.use_automatic_kmi {
            let release = request
                .connected_kernel_release
                .as_deref()
                .filter(|release| !release.trim().is_empty())
                .ok_or_else(|| {
                    DomainError::InvalidOperation("未读取到设备 Kernel 版本。".to_string())
                })?;
            return VivoRootResourceService::map_kernel_release(release)
                .map(str::to_string)
                .map_err(|error| DomainError::InvalidInput(error.to_string()));
        }

        let kmi = request
            .selected_kmi
            .as_deref()
            .filter(|kmi| !kmi.trim().is_empty())
            .ok_or_else(|| DomainError::InvalidInput("请选择受支持的 KMI。".to_string()))?;
        VivoRootResourceService::validate_kmi(kmi)
            .map(str::to_string)
            .map_err(|error| DomainError::InvalidInput(error.to_string()))
    }
}

fn valid_root_image(image: &FlashImageInfo) -> bool {
    image.size_bytes > 0
        && Path::new(&image.path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("img") || extension.eq_ignore_ascii_case("bin")
            })
}
