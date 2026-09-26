use nwflash_application::result_to_domain_error;
use nwflash_domain::{DomainError, OperationKind};
use nwflash_infrastructure::{ScrcpyProvisioner, VivoRootResourceService};
use serde::Serialize;
use std::path::PathBuf;
use tauri::State;

/// 所有运行时工具都必须来自安装包，避免运行中联网下载或执行 PATH 中的
/// 未知版本。资源缺失时让调用方给出明确的“重新安装应用”错误。
pub(crate) fn scrcpy_provisioner_with_downloader(app_root: PathBuf) -> ScrcpyProvisioner {
    ScrcpyProvisioner::bundled(app_root)
}

/// 管理器 APK 只使用安装包中的文件。
pub(crate) fn root_resource_service_with_downloader(app_root: PathBuf) -> VivoRootResourceService {
    VivoRootResourceService::new(app_root, None)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceKey {
    Scrcpy,
    KsuManager,
    OfficialKsuManager,
}

impl ResourceKey {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "scrcpy" => Ok(Self::Scrcpy),
            "manager-KSU" => Ok(Self::KsuManager),
            "manager-OfficialKsu" => Ok(Self::OfficialKsuManager),
            _ => Err(format!("不支持的资源项: {value}")),
        }
    }
}

fn validate_resource_selection(keys: Vec<String>) -> Result<Vec<ResourceKey>, String> {
    if keys.is_empty() {
        return Err("请至少选择一个需要安装的资源。".to_string());
    }

    let mut selected = Vec::with_capacity(keys.len());
    for key in keys {
        let key = ResourceKey::parse(&key)?;
        if !selected.contains(&key) {
            selected.push(key);
        }
    }
    Ok(selected)
}

#[tauri::command]
pub async fn resource_install(
    state: State<'_, crate::AppState>,
    keys: Vec<String>,
) -> Result<Vec<String>, String> {
    // 写类命令入口守卫：内置资源校验会下载并落地可执行组件到本机。
    crate::commands::guard::guard_write_command(&state)?;
    let selected = validate_resource_selection(keys)?;
    let app_root = nwflash_windows::bundled_resource_root();
    let completed = selected.clone();

    state
        .operation_coordinator
        .run_shared_async(
            OperationKind::Installing,
            "校验内置组件",
            move |context, cancellation| async move {
                let scrcpy = scrcpy_provisioner_with_downloader(app_root.clone());
                let managers = root_resource_service_with_downloader(app_root);
                let total = selected.len();

                for (index, resource) in selected.into_iter().enumerate() {
                    if cancellation.is_cancelled() {
                        return Err(DomainError::UserCancelled(
                            "用户取消内置组件校验。".to_string(),
                        ));
                    }

                    let label = match resource {
                        ResourceKey::Scrcpy => {
                            context.report_stage("校验内置 scrcpy");
                            scrcpy.ensure_installed(&cancellation, None).await.map_err(
                                |error| {
                                    DomainError::ExternalTool(format!(
                                        "内置 scrcpy 校验失败：{error}"
                                    ))
                                },
                            )?;
                            "scrcpy"
                        }
                        ResourceKey::KsuManager => {
                            context.report_stage("校验内置 KSU 管理器");
                            let manager = managers
                                .resolve_manager("KSU")
                                .map_err(|error| DomainError::ExternalTool(error.to_string()))?;
                            managers
                                .ensure_manager_apk(&manager, &cancellation, None)
                                .await
                                .map_err(|error| {
                                    DomainError::ExternalTool(format!(
                                        "内置 KSU 管理器校验失败：{error}"
                                    ))
                                })?;
                            "manager-KSU"
                        }
                        ResourceKey::OfficialKsuManager => {
                            context.report_stage("校验内置 KernelSU 管理器");
                            let manager = managers
                                .resolve_manager("OfficialKsu")
                                .map_err(|error| DomainError::ExternalTool(error.to_string()))?;
                            managers
                                .ensure_manager_apk(&manager, &cancellation, None)
                                .await
                                .map_err(|error| {
                                    DomainError::ExternalTool(format!(
                                        "内置 KernelSU 管理器校验失败：{error}"
                                    ))
                                })?;
                            "manager-OfficialKsu"
                        }
                    };

                    context.report_stage(format!("{label} 已就绪"));
                    context.report_progress((index + 1) as f64 / total as f64);
                }

                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;

    Ok(completed.into_iter().map(resource_key_name).collect())
}

fn resource_key_name(key: ResourceKey) -> String {
    match key {
        ResourceKey::Scrcpy => "scrcpy",
        ResourceKey::KsuManager => "manager-KSU",
        ResourceKey::OfficialKsuManager => "manager-OfficialKsu",
    }
    .to_string()
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResourceInventoryItemDto {
    pub key: String,
    pub display_name: String,
    pub is_ready: bool,
    pub default_selected: bool,
}

#[tauri::command]
pub fn resource_inventory() -> Vec<ResourceInventoryItemDto> {
    let app_root = nwflash_windows::bundled_resource_root();
    let managers = VivoRootResourceService::new(app_root.clone(), None);
    let scrcpy_ready = ScrcpyProvisioner::bundled(app_root).is_installed();

    build_resource_inventory(
        scrcpy_ready,
        managers.is_manager_apk_installed("KSU"),
        managers.is_manager_apk_installed("OfficialKsu"),
    )
}

fn build_resource_inventory(
    scrcpy_ready: bool,
    ksu_ready: bool,
    official_ksu_ready: bool,
) -> Vec<ResourceInventoryItemDto> {
    [
        ("scrcpy", "scrcpy 投屏", scrcpy_ready),
        ("manager-KSU", "KSU 管理器", ksu_ready),
        ("manager-OfficialKsu", "KernelSU 管理器", official_ksu_ready),
    ]
    .into_iter()
    .map(|(key, display_name, is_ready)| ResourceInventoryItemDto {
        key: key.to_string(),
        display_name: display_name.to_string(),
        is_ready,
        default_selected: !is_ready,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_inventory_lists_the_packaged_resources_and_selects_only_missing_items() {
        // payload 提取已内建进主程序，不再是需要校验的外部资源项。
        let items = build_resource_inventory(true, false, true);

        assert_eq!(items.len(), 3);
        assert_eq!(items[0].key, "scrcpy");
        assert!(items[0].is_ready);
        assert!(!items[0].default_selected);
        assert_eq!(items[1].key, "manager-KSU");
        assert!(items[1].default_selected);
        assert_eq!(items[2].key, "manager-OfficialKsu");
        assert!(!items[2].default_selected);
    }

    #[test]
    fn resource_install_selection_rejects_unknown_or_empty_resource_keys() {
        assert_eq!(
            validate_resource_selection(vec!["scrcpy".to_string(), "manager-KSU".to_string()])
                .expect("known resources should be accepted"),
            vec![ResourceKey::Scrcpy, ResourceKey::KsuManager]
        );
        assert!(validate_resource_selection(Vec::new())
            .expect_err("empty selection must be rejected")
            .contains("至少选择"));
        assert!(
            validate_resource_selection(vec!["https://example.invalid/file".to_string()])
                .expect_err("arbitrary URL must be rejected")
                .contains("不支持")
        );
    }
}
