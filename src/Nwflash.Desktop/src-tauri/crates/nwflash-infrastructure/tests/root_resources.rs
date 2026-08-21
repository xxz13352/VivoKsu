use std::{
    collections::HashMap,
    fs::{self, File},
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_infrastructure::{
    VivoRootLibrarySpec, VivoRootManagerResource, VivoRootResourceService,
};
use zip::write::{SimpleFileOptions, ZipWriter};

#[test]
fn libksud_extraction_atomically_replaces_an_existing_stale_output() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-root-duplicate-libksud-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let apk_path = root.join("fixture.apk");
    let mut archive =
        ZipWriter::new(File::create(&apk_path).expect("fixture APK should be created"));
    let options = SimpleFileOptions::default();
    archive
        .start_file("AndroidManifest.xml", options)
        .expect("manifest entry should be created");
    std::io::Write::write_all(&mut archive, b"manifest").expect("manifest should be written");
    archive
        .start_file("lib/arm64-v8a/libksud.so", options)
        .expect("library entry should be created");
    std::io::Write::write_all(&mut archive, b"verified-library")
        .expect("library should be written");
    archive.finish().expect("fixture APK should be finalized");

    let manager = VivoRootManagerResource {
        key: "fixture".to_string(),
        apk_path: apk_path.to_string_lossy().into_owned(),
        package_name: "fixture.package".to_string(),
        activity_name: "fixture.activity".to_string(),
        libraries: HashMap::from([(String::from("arm64-v8a"), VivoRootLibrarySpec)]),
    };
    let destination = root.join("libksud.so");
    fs::write(&destination, b"stale-library").expect("stale output should be written");
    let extracted = VivoRootResourceService::new(root.clone(), None)
        .extract_verified_libksud(&manager, "arm64-v8a", &destination)
        .expect("a verified library should replace a stale output atomically");

    assert_eq!(extracted, destination);
    assert_eq!(
        fs::read(&destination).expect("published output should be readable"),
        b"verified-library"
    );
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}
