use std::{env, fs, path::Path};

const SDK_ROOT_ENV: &str = "NWFLASH_VMP_SDK_ROOT";
const AMD64_MACHINE: u16 = 0x8664;

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_VMP_SDK");

    if env::var_os("CARGO_FEATURE_VMP_SDK").is_none() {
        return;
    }

    println!("cargo:rerun-if-env-changed={SDK_ROOT_ENV}");
    validate_target();

    let root = env::var_os(SDK_ROOT_ENV)
        .unwrap_or_else(|| panic!("{SDK_ROOT_ENV} must be set when vmp-sdk is enabled"));
    let root = Path::new(&root);
    let header = root.join("Include").join("C").join("VMProtectSDK.h");
    let library = root.join("Lib").join("Windows").join("VMProtectSDK64.lib");

    validate_header(&header);
    validate_amd64_import_library(&library);

    let library_dir = library
        .parent()
        .expect("VMProtect import library must have a parent directory");
    println!("cargo:rustc-link-search=native={}", library_dir.display());
    println!("cargo:rustc-link-lib=dylib=VMProtectSDK64");
}

fn validate_target() {
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    if os != "windows" || arch != "x86_64" || target_env != "msvc" {
        panic!(
            "vmp-sdk supports only Windows x86_64 MSVC; got os={os}, arch={arch}, env={target_env}"
        );
    }
}

fn validate_header(path: &Path) {
    let header = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "required VMProtect header {} is unavailable: {error}",
            path.display()
        )
    });
    let normalized = header.split_whitespace().collect::<Vec<_>>().join(" ");
    let required = [
        "VMP_IMPORT void VMP_API VMProtectBeginVirtualization(const char *);",
        "VMP_IMPORT void VMP_API VMProtectBeginMutation(const char *);",
        "VMP_IMPORT void VMP_API VMProtectBeginUltra(const char *);",
        "VMP_IMPORT void VMP_API VMProtectEnd(void);",
        "VMP_IMPORT bool VMP_API VMProtectIsProtected();",
        "VMP_IMPORT bool VMP_API VMProtectIsDebuggerPresent(bool);",
        "VMP_IMPORT bool VMP_API VMProtectIsVirtualMachinePresent(void);",
        "VMP_IMPORT bool VMP_API VMProtectIsValidImageCRC(void);",
    ];

    for declaration in required {
        if !normalized.contains(declaration) {
            panic!(
                "VMProtect header {} is missing required declaration: {declaration}",
                path.display()
            );
        }
    }
}

fn validate_amd64_import_library(path: &Path) {
    let bytes = fs::read(path).unwrap_or_else(|error| {
        panic!(
            "required VMProtect x64 import library {} is unavailable: {error}",
            path.display()
        )
    });
    if !bytes.starts_with(b"!<arch>\n") {
        panic!(
            "VMProtect import library {} is not a COFF archive",
            path.display()
        );
    }

    let mut offset = 8_usize;
    let mut object_count = 0_usize;
    while offset < bytes.len() {
        let header_end = offset
            .checked_add(60)
            .filter(|end| *end <= bytes.len())
            .unwrap_or_else(|| {
                panic!(
                    "VMProtect import library {} has a truncated archive header",
                    path.display()
                )
            });
        let header = &bytes[offset..header_end];
        if &header[58..60] != b"`\n" {
            panic!(
                "VMProtect import library {} has an invalid archive member header",
                path.display()
            );
        }

        let name = std::str::from_utf8(&header[..16]).unwrap_or("").trim();
        let size = std::str::from_utf8(&header[48..58])
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or_else(|| {
                panic!(
                    "VMProtect import library {} has an invalid member size",
                    path.display()
                )
            });
        let data_start = header_end;
        let data_end = data_start
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .unwrap_or_else(|| {
                panic!(
                    "VMProtect import library {} has a truncated member",
                    path.display()
                )
            });

        if name != "/" && name != "//" {
            let member = &bytes[data_start..data_end];
            let machine = coff_machine(member).unwrap_or_else(|| {
                panic!(
                    "VMProtect import library {} contains an unrecognized COFF member",
                    path.display()
                )
            });
            if machine != AMD64_MACHINE {
                panic!(
                    "VMProtect import library {} has wrong architecture 0x{machine:04X}; expected x64 COFF 0x{AMD64_MACHINE:04X}",
                    path.display()
                );
            }
            object_count += 1;
        }

        offset = data_end + (size & 1);
    }

    if object_count == 0 {
        panic!(
            "VMProtect import library {} contains no x64 COFF objects",
            path.display()
        );
    }
}

fn coff_machine(member: &[u8]) -> Option<u16> {
    if member.len() < 2 {
        return None;
    }
    let first = u16::from_le_bytes([member[0], member[1]]);
    if first != 0 {
        return Some(first);
    }

    if member.len() >= 8 && member[2..4] == [0xff, 0xff] {
        return Some(u16::from_le_bytes([member[6], member[7]]));
    }
    None
}
