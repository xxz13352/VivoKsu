mod build_support;

use std::{env, path::Path};

use build_support::{validate_sdk_root, validate_target, SDK_ROOT_ENV};

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_VMP_SDK");
    if env::var_os("CARGO_FEATURE_VMP_SDK").is_none() {
        return;
    }

    println!("cargo:rerun-if-env-changed={SDK_ROOT_ENV}");
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    validate_target(&os, &arch, &target_env).unwrap_or_else(|error| panic!("{error}"));

    let root = env::var_os(SDK_ROOT_ENV)
        .unwrap_or_else(|| panic!("{SDK_ROOT_ENV} must be set when vmp-sdk is enabled"));
    let paths = validate_sdk_root(Path::new(&root)).unwrap_or_else(|error| panic!("{error}"));

    println!(
        "cargo:rustc-link-search=native={}",
        paths.library_dir.display()
    );
    println!("cargo:rustc-link-lib=dylib=VMProtectSDK64");
}
