use std::fs;
use std::path::PathBuf;

#[test]
fn workspace_product_name_is_marked_as_nwf_display_name() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cargo_toml = fs::read_to_string(manifest_dir.join("Cargo.toml"))
        .expect("Cargo.toml should exist in workspace root");
    let tauri_conf = fs::read_to_string(manifest_dir.join("tauri.conf.json"))
        .expect("tauri.conf.json should exist in workspace root");

    assert!(cargo_toml.contains("name = \"nwflash-desktop\""));
    assert!(tauri_conf.contains("\"奶蛙Flash\""));
    assert!(!cargo_toml.contains(".NET"));
    assert!(!tauri_conf.contains("dotnet"));
}

#[test]
fn all_required_member_crates_are_declared_in_workspace() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cargo_toml = fs::read_to_string(manifest_dir.join("Cargo.toml"))
        .expect("Cargo.toml should exist in workspace root");

    let expected_members = [
        "crates/nwflash-domain",
        "crates/nwflash-application",
        "crates/nwflash-infrastructure",
        "crates/nwflash-windows",
        "crates/nwflash-tauri",
    ];

    for member in expected_members {
        let manifest_path = manifest_dir.join(member).join("Cargo.toml");
        assert!(manifest_path.exists(), "missing crate manifest: {member}");
        assert!(
            cargo_toml.contains(member),
            "workspace manifest should include {member}"
        );
    }
}

#[test]
fn windows_build_generates_tauri_resource_metadata() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cargo_toml = fs::read_to_string(manifest_dir.join("Cargo.toml"))
        .expect("Cargo.toml should exist in workspace root");
    let build_rs = fs::read_to_string(manifest_dir.join("build.rs"))
        .expect("build.rs should exist in workspace root");

    assert!(
        cargo_toml.contains("tauri-build"),
        "the workspace root must declare the Tauri build dependency"
    );
    assert!(
        build_rs.contains("tauri_build::build()"),
        "build.rs must generate Tauri resource metadata, including the Windows application manifest"
    );
}

#[test]
fn windows_tauri_ipc_sources_are_allowed_by_the_content_security_policy() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let tauri_conf = fs::read_to_string(manifest_dir.join("tauri.conf.json"))
        .expect("tauri.conf.json should exist in workspace root");

    assert!(
        tauri_conf.contains("connect-src 'self' ipc: http://ipc.localhost"),
        "the WebView2 CSP must allow Tauri's Windows IPC fetch endpoints"
    );
}
