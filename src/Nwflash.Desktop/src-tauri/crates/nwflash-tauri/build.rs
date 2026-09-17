use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=tests/windows-test.rc");
    println!("cargo:rerun-if-changed=tests/windows-test.manifest");

    let target = env::var("TARGET").expect("Cargo must provide TARGET");
    if target.contains("-windows-") {
        let package_manifest =
            fs::read_to_string("Cargo.toml").expect("nwflash-tauri Cargo.toml must be readable");
        if !package_manifest.contains("autobins = false") || package_manifest.contains("[[bin]]") {
            panic!(
                "the test-only Windows manifest fallback requires nwflash-tauri to have no shipping binary"
            );
        }

        // Compile the object without asking embed-resource to emit its
        // test-target directive. Cargo's `rustc-link-arg-tests` matches
        // integration-test targets (TargetKind::Test), while a lib unit
        // harness is the library target compiled in test mode; emitting both
        // directives also duplicates the resource for integration tests and
        // makes link.exe reject it.
        embed_resource::compile_for(
            "tests/windows-test.rc",
            std::iter::empty::<&str>(),
            embed_resource::NONE,
        )
        .manifest_required()
        .expect("Common Controls v6 test manifest must compile");

        // Cargo does not attach rustc-link-arg-tests to a lib unit-test
        // harness on all supported versions. This package has no shipping
        // binary of its own: its only executable targets are test harnesses,
        // so the package-scoped fallback remains test-only while also covering
        // `cargo test -p nwflash-tauri --lib`.
        let resource = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR"))
            .join(if target.contains("-msvc") {
                "windows-test.lib"
            } else {
                "libwindows-test.a"
            });
        println!("cargo:rustc-link-arg={}", resource.display());
    }
}
