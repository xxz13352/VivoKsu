use nwflash_application::result_to_domain_error;
use nwflash_domain::{DomainError, OperationKind};
use nwflash_infrastructure::{
    PayloadDumperProvisioner, RemoteAssetDownloader, ScrcpyProvisioner, VivoRootResourceService,
};
use serde::Serialize;
use tauri::State;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceKey {
    Scrcpy,
    Payload,
    KsuManager,
    OfficialKsuManager,
}

impl ResourceKey {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "scrcpy" => Ok(Self::Scrcpy),
            "payload" => Ok(Self::Payload),
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
    let selected = validate_resource_selection(keys)?;
    let app_root = nwflash_windows::bundled_resource_root();
    let completed = selected.clone();

    state
        .operation_coordinator
        .run_async(
            OperationKind::Installing,
            "安装外置组件",
            move |context, cancellation| async move {
                let downloader = RemoteAssetDownloader::default();
                let scrcpy = ScrcpyProvisioner::new();
                let payload = PayloadDumperProvisioner::new(downloader.clone(), None, None);
                let managers = VivoRootResourceService::new(app_root, Some(downloader));
                let total = selected.len();

                for (index, resource) in selected.into_iter().enumerate() {
                    if cancellation.is_cancelled() {
                        return Err(DomainError::UserCancelled("用户取消组件安装。".to_string()));
                    }

                    let label = match resource {
                        ResourceKey::Scrcpy => {
                            context.report_stage("下载 scrcpy");
                            scrcpy.ensure_installed(&cancellation, None).await.map_err(
                                |error| {
                                    DomainError::ExternalTool(format!(
                                        "scrcpy 组件下载失败：{error}"
                                    ))
                                },
                            )?;
                            "scrcpy"
                        }
                        ResourceKey::Payload => {
                            context.report_stage("下载 payload_dumper");
                            payload
                                .ensure_installed(&cancellation, None)
                                .await
                                .map_err(|error| {
                                    DomainError::ExternalTool(format!(
                                        "payload_dumper 组件下载失败：{error}"
                                    ))
                                })?;
                            "payload"
                        }
                        ResourceKey::KsuManager => {
                            context.report_stage("下载 KSU 管理器");
                            let manager = managers
                                .resolve_manager("KSU")
                                .map_err(|error| DomainError::ExternalTool(error.to_string()))?;
                            managers
                                .ensure_manager_apk(&manager, &cancellation, None)
                                .await
                                .map_err(|error| {
                                    DomainError::ExternalTool(format!(
                                        "KSU 管理器下载失败：{error}"
                                    ))
                                })?;
                            "manager-KSU"
                        }
                        ResourceKey::OfficialKsuManager => {
                            context.report_stage("下载 KernelSU 管理器");
                            let manager = managers
                                .resolve_manager("OfficialKsu")
                                .map_err(|error| DomainError::ExternalTool(error.to_string()))?;
                            managers
                                .ensure_manager_apk(&manager, &cancellation, None)
                                .await
                                .map_err(|error| {
                                    DomainError::ExternalTool(format!(
                                        "KernelSU 管理器下载失败：{error}"
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
        ResourceKey::Payload => "payload",
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
    let downloader = RemoteAssetDownloader::default();
    let managers = VivoRootResourceService::new(app_root, Some(downloader.clone()));
    let scrcpy_ready = ScrcpyProvisioner::new().is_installed();
    let payload_ready = PayloadDumperProvisioner::new(downloader, None, None).is_available();

    build_resource_inventory(
        scrcpy_ready,
        payload_ready,
        managers.is_manager_apk_installed("KSU"),
        managers.is_manager_apk_installed("OfficialKsu"),
    )
}

fn build_resource_inventory(
    scrcpy_ready: bool,
    payload_ready: bool,
    ksu_ready: bool,
    official_ksu_ready: bool,
) -> Vec<ResourceInventoryItemDto> {
    [
        ("scrcpy", "scrcpy 投屏", scrcpy_ready),
        ("payload", "payload_dumper", payload_ready),
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
    fn resource_inventory_lists_the_four_wpf_resources_and_selects_only_missing_items() {
        let items = build_resource_inventory(true, false, false, true);

        assert_eq!(items.len(), 4);
        assert_eq!(items[0].key, "scrcpy");
        assert!(items[0].is_ready);
        assert!(!items[0].default_selected);
        assert_eq!(items[1].key, "payload");
        assert!(items[1].default_selected);
        assert_eq!(items[2].key, "manager-KSU");
        assert!(items[2].default_selected);
        assert_eq!(items[3].key, "manager-OfficialKsu");
        assert!(!items[3].default_selected);
    }

    #[test]
    fn resource_install_selection_rejects_unknown_or_empty_resource_keys() {
        assert_eq!(
            validate_resource_selection(vec!["scrcpy".to_string(), "payload".to_string()])
                .expect("known resources should be accepted"),
            vec![ResourceKey::Scrcpy, ResourceKey::Payload]
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
