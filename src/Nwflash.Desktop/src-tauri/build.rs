use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.png");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_PROTECTED");

    if env::var_os("CARGO_FEATURE_PROTECTED").is_some() {
        let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR"));
        let profile_dir = out_dir
            .ancestors()
            .nth(3)
            .expect("OUT_DIR must be nested under the Cargo profile directory");
        let map_path = profile_dir.join("nwflash-desktop.map");
        println!(
            "cargo:rustc-link-arg-bin=nwflash-desktop=/MAP:{}",
            map_path.display()
        );
        println!("cargo:rustc-link-arg-bin=nwflash-desktop=/MAPINFO:EXPORTS");
        println!("cargo:rustc-link-arg-bin=nwflash-desktop=/DEBUG:FULL");
    }

    tauri_build::build()
}
