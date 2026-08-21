use nwflash_application::{RootManager, RootPatchPreflightRequest, RootService};
use nwflash_domain::FlashImageInfo;

fn image(path: &str) -> FlashImageInfo {
    FlashImageInfo {
        path: path.to_string(),
        size_bytes: 1024,
    }
}

#[test]
fn vivo_ksu_requires_only_init_boot_and_maps_the_connected_kernel() {
    let readiness = RootService::new()
        .evaluate_preflight(RootPatchPreflightRequest {
            manager: RootManager::VivoKsu,
            init_boot: Some(image("C:\\images\\init_boot.img")),
            vendor_boot: None,
            use_automatic_kmi: true,
            connected_kernel_release: Some("6.1.75-android14".to_string()),
            selected_kmi: None,
        })
        .expect("the connected kernel should map to a supported KMI");

    assert!(readiness.can_patch);
    assert!(readiness.can_run_automatic);
    assert_eq!(readiness.effective_kmi, "android14-6.1");
    assert_eq!(readiness.manager_label, "Vivo KSU");
}

#[test]
fn official_kernelsu_allows_a_single_patch_but_requires_both_images_for_automatic_root() {
    let readiness = RootService::new()
        .evaluate_preflight(RootPatchPreflightRequest {
            manager: RootManager::OfficialKernelSu,
            init_boot: Some(image("C:\\images\\init_boot.bin")),
            vendor_boot: None,
            use_automatic_kmi: false,
            connected_kernel_release: None,
            selected_kmi: Some("android15-6.6".to_string()),
        })
        .expect("a selected supported KMI should be accepted");

    assert!(readiness.can_patch);
    assert!(!readiness.can_run_automatic);
    assert!(readiness.summary.contains("全自动需要两份镜像"));
    assert_eq!(readiness.manager_label, "官方 KernelSU");
}
