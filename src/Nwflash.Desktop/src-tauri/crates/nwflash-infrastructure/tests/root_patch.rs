use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_domain::FlashImageInfo;
use nwflash_infrastructure::{validate_patched_root_image, RootPatchArtifactService};

#[test]
fn exports_non_empty_patched_images_to_the_fixed_desktop_folder() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-root-artifacts-{nonce}"));
    let source = root.join("source");
    let desktop = root.join("desktop");
    fs::create_dir_all(&source).expect("source directory should be created");
    let init_boot = source.join("init_boot_vivoksu_patched.img");
    let vendor_boot = source.join("vendor_boot_vivo_patched.img");
    fs::write(&init_boot, b"init-patched").expect("init fixture should be written");
    fs::write(&vendor_boot, b"vendor-patched").expect("vendor fixture should be written");

    let exported = RootPatchArtifactService::new()
        .export_to_directory(
            &[
                FlashImageInfo {
                    path: init_boot.to_string_lossy().into_owned(),
                    size_bytes: 12,
                },
                FlashImageInfo {
                    path: vendor_boot.to_string_lossy().into_owned(),
                    size_bytes: 14,
                },
            ],
            &desktop,
        )
        .expect("valid patched images should be copied to the ROOT artifact folder");

    let output = desktop.join("VivoKsu_修补镜像");
    assert_eq!(exported.len(), 2);
    assert_eq!(
        fs::read(output.join("init_boot_vivoksu_patched.img")).unwrap(),
        b"init-patched"
    );
    assert_eq!(
        fs::read(output.join("vendor_boot_vivo_patched.img")).unwrap(),
        b"vendor-patched"
    );
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn patched_root_image_must_be_non_empty_and_within_the_allowed_growth_limit() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-root-validation-{nonce}"));
    fs::create_dir_all(&root).expect("fixture root should be created");
    let source = root.join("init_boot.img");
    let patched = root.join("patched_init_boot.img");
    fs::write(&source, b"source-image").expect("source fixture should be written");
    fs::write(&patched, b"patched-image").expect("patched fixture should be written");
    let source_info = FlashImageInfo {
        path: source.to_string_lossy().into_owned(),
        size_bytes: 1,
    };

    let validated = validate_patched_root_image(&source_info, &patched)
        .expect("a non-empty patched image within the growth limit should be accepted");
    assert_eq!(validated.size_bytes, 13);

    let source_length = fs::metadata(&source)
        .expect("source metadata should be readable")
        .len();
    fs::File::create(&patched)
        .and_then(|file| file.set_len(source_length + 16 * 1024 * 1024 + 1))
        .expect("oversized fixture should be created");
    assert!(validate_patched_root_image(&source_info, &patched).is_err());

    fs::File::create(&patched).expect("empty fixture should be created");
    assert!(validate_patched_root_image(&source_info, &patched).is_err());
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}
