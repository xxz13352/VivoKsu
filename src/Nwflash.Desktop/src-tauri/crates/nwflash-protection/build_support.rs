use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub const SDK_ROOT_ENV: &str = "NWFLASH_VMP_SDK_ROOT";
pub const EXPECTED_SDK_DLL: &str = "VMProtectSDK64.dll";
pub const AMD64_MACHINE: u16 = 0x8664;
pub const REQUIRED_IMPORT_SYMBOLS: [&str; 8] = [
    "VMProtectBeginVirtualization",
    "VMProtectBeginMutation",
    "VMProtectBeginUltra",
    "VMProtectEnd",
    "VMProtectIsProtected",
    "VMProtectIsDebuggerPresent",
    "VMProtectIsVirtualMachinePresent",
    "VMProtectIsValidImageCRC",
];
pub const REQUIRED_HEADER_DECLARATIONS: [&str; 8] = [
    "VMP_IMPORT void VMP_API VMProtectBeginVirtualization(const char *);",
    "VMP_IMPORT void VMP_API VMProtectBeginMutation(const char *);",
    "VMP_IMPORT void VMP_API VMProtectBeginUltra(const char *);",
    "VMP_IMPORT void VMP_API VMProtectEnd(void);",
    "VMP_IMPORT bool VMP_API VMProtectIsProtected();",
    "VMP_IMPORT bool VMP_API VMProtectIsDebuggerPresent(bool);",
    "VMP_IMPORT bool VMP_API VMProtectIsVirtualMachinePresent(void);",
    "VMP_IMPORT bool VMP_API VMProtectIsValidImageCRC(void);",
];

#[derive(Debug)]
pub struct ValidatedSdkPaths {
    pub library_dir: PathBuf,
}

pub fn validate_target(os: &str, arch: &str, target_env: &str) -> Result<(), String> {
    if os == "windows" && arch == "x86_64" && target_env == "msvc" {
        Ok(())
    } else {
        Err(format!(
            "vmp-sdk supports only Windows x86_64 MSVC; got os={os}, arch={arch}, env={target_env}"
        ))
    }
}

pub fn validate_sdk_root(root: &Path) -> Result<ValidatedSdkPaths, String> {
    if !root.is_absolute() {
        return Err(format!(
            "{SDK_ROOT_ENV} must be a fully qualified path when vmp-sdk is enabled"
        ));
    }

    let header = root.join("Include").join("C").join("VMProtectSDK.h");
    let library = root.join("Lib").join("Windows").join("VMProtectSDK64.lib");
    let header_source = fs::read_to_string(&header).map_err(|error| {
        format!(
            "required VMProtect header {} is unavailable: {error}",
            header.display()
        )
    })?;
    validate_header_source(&header_source)
        .map_err(|error| format!("VMProtect header {} {error}", header.display()))?;

    let library_bytes = fs::read(&library).map_err(|error| {
        format!(
            "required VMProtect x64 import library {} is unavailable: {error}",
            library.display()
        )
    })?;
    validate_import_library_bytes(&library_bytes)
        .map_err(|error| format!("VMProtect import library {} {error}", library.display()))?;

    let library_dir = library
        .parent()
        .ok_or_else(|| "VMProtect import library must have a parent directory".to_string())?
        .to_path_buf();
    Ok(ValidatedSdkPaths { library_dir })
}

pub fn validate_header_source(header: &str) -> Result<(), String> {
    let normalized = header.split_whitespace().collect::<Vec<_>>().join(" ");
    for declaration in REQUIRED_HEADER_DECLARATIONS {
        if !normalized.contains(declaration) {
            return Err(format!("is missing required declaration: {declaration}"));
        }
    }
    Ok(())
}

pub fn validate_import_library_bytes(bytes: &[u8]) -> Result<(), String> {
    if !bytes.starts_with(b"!<arch>\n") {
        return Err("is not a COFF archive".to_string());
    }

    let mut offset = 8_usize;
    let mut object_count = 0_usize;
    let mut imports = BTreeMap::<String, String>::new();
    while offset < bytes.len() {
        let header_end = offset
            .checked_add(60)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "has a truncated archive header".to_string())?;
        let header = &bytes[offset..header_end];
        if &header[58..60] != b"`\n" {
            return Err("has an invalid archive member header".to_string());
        }

        let name = std::str::from_utf8(&header[..16])
            .map_err(|_| "has a non-ASCII archive member name".to_string())?
            .trim();
        let size = std::str::from_utf8(&header[48..58])
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .ok_or_else(|| "has an invalid member size".to_string())?;
        let data_start = header_end;
        let data_end = data_start
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "has a truncated member".to_string())?;

        if name != "/" && name != "//" {
            let member = &bytes[data_start..data_end];
            let machine = coff_machine(member)
                .ok_or_else(|| "contains an unrecognized COFF member".to_string())?;
            if machine != AMD64_MACHINE {
                return Err(format!(
                    "has wrong architecture 0x{machine:04X}; expected x64 COFF 0x{AMD64_MACHINE:04X}"
                ));
            }
            object_count += 1;

            if is_short_import(member) {
                let (symbol, dll) = short_import_identity(member)?;
                if !dll.eq_ignore_ascii_case(EXPECTED_SDK_DLL) {
                    return Err(format!(
                        "imports {symbol} from {dll}; expected {EXPECTED_SDK_DLL}"
                    ));
                }
                imports.insert(symbol.to_string(), dll.to_string());
            }
        }

        offset = if size & 1 == 1 {
            if data_end >= bytes.len() {
                return Err("is missing an odd-member archive padding byte".to_string());
            }
            if bytes[data_end] != b'\n' {
                return Err("has invalid odd-member archive padding; expected newline".to_string());
            }
            data_end + 1
        } else {
            data_end
        };
    }

    if object_count == 0 {
        return Err("contains no x64 COFF objects".to_string());
    }
    for symbol in REQUIRED_IMPORT_SYMBOLS {
        if !imports.contains_key(symbol) {
            return Err(format!(
                "is missing required import symbol {symbol} for {EXPECTED_SDK_DLL}"
            ));
        }
    }
    Ok(())
}

fn coff_machine(member: &[u8]) -> Option<u16> {
    if member.len() < 2 {
        return None;
    }
    let first = u16::from_le_bytes([member[0], member[1]]);
    if first != 0 {
        return Some(first);
    }
    if is_short_import(member) {
        return Some(u16::from_le_bytes([member[6], member[7]]));
    }
    None
}

fn is_short_import(member: &[u8]) -> bool {
    member.len() >= 8 && member[..4] == [0, 0, 0xff, 0xff]
}

fn short_import_identity(member: &[u8]) -> Result<(&str, &str), String> {
    if member.len() < 20 {
        return Err("contains a truncated short import header".to_string());
    }
    let payload_size =
        u32::from_le_bytes([member[12], member[13], member[14], member[15]]) as usize;
    let payload_end = 20_usize
        .checked_add(payload_size)
        .filter(|end| *end <= member.len())
        .ok_or_else(|| "contains a truncated short import payload".to_string())?;
    let payload = &member[20..payload_end];
    let symbol_end = payload
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| "contains a short import without a symbol terminator".to_string())?;
    let dll_start = symbol_end + 1;
    let dll_end = payload[dll_start..]
        .iter()
        .position(|byte| *byte == 0)
        .map(|offset| dll_start + offset)
        .ok_or_else(|| "contains a short import without a DLL terminator".to_string())?;
    let symbol = std::str::from_utf8(&payload[..symbol_end])
        .map_err(|_| "contains a non-UTF-8 import symbol".to_string())?;
    let dll = std::str::from_utf8(&payload[dll_start..dll_end])
        .map_err(|_| "contains a non-UTF-8 imported DLL name".to_string())?;
    if symbol.is_empty() || dll.is_empty() {
        return Err("contains an empty short import identity".to_string());
    }
    Ok((symbol, dll))
}
