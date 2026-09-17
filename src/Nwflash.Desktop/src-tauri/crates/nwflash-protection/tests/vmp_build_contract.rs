#[path = "../build_support.rs"]
mod build_support;

use std::path::Path;

use build_support::{
    validate_header_source, validate_import_library_bytes, validate_sdk_root, validate_target,
    ValidatedSdkPaths, EXPECTED_SDK_DLL, REQUIRED_IMPORT_SYMBOLS,
};

const AMD64: u16 = 0x8664;
const I386: u16 = 0x014c;

#[test]
fn target_and_validated_path_contract_remain_windows_x64_msvc_only() {
    validate_target("windows", "x86_64", "msvc").expect("supported target must pass");
    assert!(validate_target("windows", "x86", "msvc").is_err());

    let paths = ValidatedSdkPaths {
        library_dir: std::path::PathBuf::from("validated-lib-dir"),
    };
    assert_eq!(paths.library_dir, Path::new("validated-lib-dir"));
}

#[test]
fn sdk_root_must_be_fully_qualified_before_any_filesystem_probe() {
    let error = validate_sdk_root(Path::new("relative-sdk-root"))
        .expect_err("relative SDK roots must fail closed");

    assert!(
        error.contains("fully qualified"),
        "unexpected error: {error}"
    );
}

#[test]
fn header_identity_requires_every_consumed_sdk_declaration() {
    let header = valid_header().replace(
        "VMP_IMPORT bool VMP_API VMProtectIsValidImageCRC(void);",
        "VMP_IMPORT bool VMP_API WrongCrcFunction(void);",
    );

    let error = validate_header_source(&header).expect_err("wrong header identity must fail");
    assert!(
        error.contains("VMProtectIsValidImageCRC"),
        "unexpected error: {error}"
    );
}

#[test]
fn import_library_requires_every_symbol_and_the_exact_sdk_dll_name() {
    let mut members = required_imports(AMD64, EXPECTED_SDK_DLL);
    members.retain(|(symbol, _, _)| *symbol != "VMProtectEnd");
    let missing_symbol = archive(&members);
    let error = validate_import_library_bytes(&missing_symbol)
        .expect_err("missing required import symbol must fail");
    assert!(error.contains("VMProtectEnd"), "unexpected error: {error}");

    let wrong_dll = archive(&required_imports(AMD64, "LookalikeSDK64.dll"));
    let error = validate_import_library_bytes(&wrong_dll)
        .expect_err("wrong imported DLL identity must fail");
    assert!(
        error.contains(EXPECTED_SDK_DLL),
        "unexpected error: {error}"
    );
}

#[test]
fn import_library_rejects_wrong_architecture() {
    let bytes = archive(&required_imports(I386, EXPECTED_SDK_DLL));

    let error = validate_import_library_bytes(&bytes).expect_err("x86 import library must fail");
    assert!(error.contains("0x014C"), "unexpected error: {error}");
}

#[test]
fn odd_archive_member_requires_a_present_newline_padding_byte() {
    let mut bytes = archive(&[("OddName", EXPECTED_SDK_DLL, AMD64)]);
    assert_eq!(bytes.last(), Some(&b'\n'));
    bytes.pop();

    let error = validate_import_library_bytes(&bytes)
        .expect_err("missing odd-member padding must be rejected");
    assert!(error.contains("padding"), "unexpected error: {error}");
}

#[test]
fn odd_archive_member_rejects_non_newline_padding() {
    let mut bytes = archive(&[("OddName", EXPECTED_SDK_DLL, AMD64)]);
    *bytes.last_mut().expect("archive must have padding") = b' ';

    let error = validate_import_library_bytes(&bytes)
        .expect_err("invalid odd-member padding must be rejected");
    assert!(error.contains("padding"), "unexpected error: {error}");
}

#[test]
fn archive_rejects_a_truncated_member_payload() {
    let mut bytes = archive(&required_imports(AMD64, EXPECTED_SDK_DLL));
    bytes.truncate(bytes.len() - 4);

    let error = validate_import_library_bytes(&bytes)
        .expect_err("truncated archive member payload must be rejected");
    assert!(error.contains("truncated"), "unexpected error: {error}");
}

#[test]
fn valid_amd64_import_identity_is_accepted() {
    let bytes = archive(&required_imports(AMD64, EXPECTED_SDK_DLL));

    validate_import_library_bytes(&bytes).expect("complete x64 SDK identity must pass");
}

fn valid_header() -> String {
    [
        "VMP_IMPORT void VMP_API VMProtectBeginVirtualization(const char *);",
        "VMP_IMPORT void VMP_API VMProtectBeginMutation(const char *);",
        "VMP_IMPORT void VMP_API VMProtectBeginUltra(const char *);",
        "VMP_IMPORT void VMP_API VMProtectEnd(void);",
        "VMP_IMPORT bool VMP_API VMProtectIsProtected();",
        "VMP_IMPORT bool VMP_API VMProtectIsDebuggerPresent(bool);",
        "VMP_IMPORT bool VMP_API VMProtectIsVirtualMachinePresent(void);",
        "VMP_IMPORT bool VMP_API VMProtectIsValidImageCRC(void);",
    ]
    .join("\n")
}

fn required_imports(machine: u16, dll: &'static str) -> Vec<(&'static str, &'static str, u16)> {
    REQUIRED_IMPORT_SYMBOLS
        .iter()
        .map(|symbol| (*symbol, dll, machine))
        .collect()
}

fn archive(imports: &[(&str, &str, u16)]) -> Vec<u8> {
    let mut archive = b"!<arch>\n".to_vec();
    for (index, (symbol, dll, machine)) in imports.iter().enumerate() {
        let member = short_import_object(symbol, dll, *machine);
        append_archive_member(&mut archive, &format!("m{index}/"), &member);
    }
    archive
}

fn short_import_object(symbol: &str, dll: &str, machine: u16) -> Vec<u8> {
    let payload_size = symbol.len() + 1 + dll.len() + 1;
    let mut member = Vec::with_capacity(20 + payload_size);
    member.extend_from_slice(&0_u16.to_le_bytes());
    member.extend_from_slice(&0xffff_u16.to_le_bytes());
    member.extend_from_slice(&0_u16.to_le_bytes());
    member.extend_from_slice(&machine.to_le_bytes());
    member.extend_from_slice(&0_u32.to_le_bytes());
    member.extend_from_slice(&(payload_size as u32).to_le_bytes());
    member.extend_from_slice(&0_u16.to_le_bytes());
    member.extend_from_slice(&4_u16.to_le_bytes());
    member.extend_from_slice(symbol.as_bytes());
    member.push(0);
    member.extend_from_slice(dll.as_bytes());
    member.push(0);
    member
}

fn append_archive_member(archive: &mut Vec<u8>, name: &str, member: &[u8]) {
    let header = format!(
        "{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
        "0",
        "0",
        "0",
        "0",
        member.len()
    );
    assert_eq!(header.len(), 60);
    archive.extend_from_slice(header.as_bytes());
    archive.extend_from_slice(member);
    if member.len() % 2 == 1 {
        archive.push(b'\n');
    }
}
