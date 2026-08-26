use std::process::Command;

use nwflash_protection::{
    build_identity_matches, marker_backend_available, probe_release_image, verify_image_integrity,
    ImageIntegrityFailure, ImageIntegrityStatus, IntegrityProbe, IntegritySignals,
    IntegrityTelemetry, ReleaseImageProbe, VmpIntegrityProbe,
};

#[derive(Debug, Clone, Copy)]
struct DeterministicProbe(IntegritySignals);

impl IntegrityProbe for DeterministicProbe {
    fn signals(&self) -> IntegritySignals {
        self.0
    }
}

#[test]
fn valid_crc_from_an_injected_protected_probe_is_accepted() {
    let outcome = verify_image_integrity(&DeterministicProbe(IntegritySignals::available(
        true, true, false, false,
    )));

    assert_eq!(outcome.status, ImageIntegrityStatus::Valid);
    assert_eq!(outcome.telemetry, IntegrityTelemetry::None);
}

#[test]
fn invalid_crc_from_an_available_probe_is_an_integrity_failure() {
    let outcome = verify_image_integrity(&DeterministicProbe(IntegritySignals::available(
        true, false, false, false,
    )));

    assert_eq!(
        outcome.status,
        ImageIntegrityStatus::Failure(ImageIntegrityFailure::InvalidImageCrc)
    );
}

#[test]
fn unavailable_probe_is_reported_instead_of_claiming_integrity_success() {
    let outcome = verify_image_integrity(&DeterministicProbe(IntegritySignals::unavailable()));

    assert_eq!(outcome.status, ImageIntegrityStatus::ProbeUnavailable);
}

#[test]
fn available_but_unprotected_image_is_an_integrity_failure() {
    let outcome = verify_image_integrity(&DeterministicProbe(IntegritySignals::available(
        false, true, false, false,
    )));

    assert_eq!(
        outcome.status,
        ImageIntegrityStatus::Failure(ImageIntegrityFailure::ImageNotProtected)
    );
}

#[test]
fn debugger_and_vm_signals_are_classified_as_telemetry_without_vm_denial() {
    let debugger = verify_image_integrity(&DeterministicProbe(IntegritySignals::available(
        true, true, true, false,
    )));
    assert_eq!(debugger.status, ImageIntegrityStatus::Valid);
    assert_eq!(debugger.telemetry, IntegrityTelemetry::DebuggerPresent);

    let virtual_machine = verify_image_integrity(&DeterministicProbe(IntegritySignals::available(
        true, true, false, true,
    )));
    assert_eq!(virtual_machine.status, ImageIntegrityStatus::Valid);
    assert_eq!(
        virtual_machine.telemetry,
        IntegrityTelemetry::VirtualMachinePresent
    );

    let both = verify_image_integrity(&DeterministicProbe(IntegritySignals::available(
        true, true, true, true,
    )));
    assert_eq!(both.status, ImageIntegrityStatus::Valid);
    assert_eq!(
        both.telemetry,
        IntegrityTelemetry::DebuggerAndVirtualMachinePresent
    );
}

#[test]
fn release_image_probe_preserves_both_vmprotect_oracles() {
    let report = probe_release_image(&DeterministicProbe(IntegritySignals::available(
        false, false, true, true,
    )));

    assert_eq!(
        report,
        ReleaseImageProbe {
            available: true,
            vmprotect_is_protected: Some(false),
            vmprotect_is_valid_image_crc: Some(false),
        }
    );
}

#[test]
fn release_image_probe_reports_unavailable_without_claiming_false_oracles() {
    let report = probe_release_image(&DeterministicProbe(IntegritySignals::unavailable()));

    assert_eq!(
        report,
        ReleaseImageProbe {
            available: false,
            vmprotect_is_protected: None,
            vmprotect_is_valid_image_crc: None,
        }
    );
}

#[cfg(not(feature = "vmp-sdk"))]
#[test]
fn no_feature_production_probe_and_marker_backend_are_explicitly_unavailable() {
    let outcome = verify_image_integrity(&VmpIntegrityProbe);

    assert_eq!(outcome.status, ImageIntegrityStatus::ProbeUnavailable);
    assert!(!marker_backend_available());
    assert!(build_identity_matches("build-123", "build-123"));
    assert!(!build_identity_matches("build-123", "other-build"));
}

#[cfg(all(windows, target_arch = "x86_64", not(feature = "vmp-sdk")))]
#[test]
#[ignore = "spawns a nested Cargo build to exercise build.rs"]
fn missing_sdk_root_build_fails_closed_with_an_actionable_error() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let target_dir =
        std::env::temp_dir().join(format!("nwflash-vmp-missing-sdk-{}", std::process::id()));
    let output = Command::new(env!("CARGO"))
        .current_dir(manifest_dir)
        .args([
            "check",
            "--features",
            "vmp-sdk",
            "--target-dir",
            target_dir.to_str().expect("temporary path must be UTF-8"),
        ])
        .env_remove("NWFLASH_VMP_SDK_ROOT")
        .output()
        .expect("nested Cargo check must start");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "missing SDK root must fail closed"
    );
    assert!(
        stderr.contains("NWFLASH_VMP_SDK_ROOT must be set when vmp-sdk is enabled"),
        "unexpected build failure: {stderr}"
    );

    if target_dir.exists() {
        std::fs::remove_dir_all(&target_dir).expect("temporary target directory must be removable");
    }
}

#[cfg(all(windows, target_arch = "x86_64", not(feature = "vmp-sdk")))]
#[test]
#[ignore = "spawns a nested Cargo build to exercise build.rs"]
fn bogus_sdk_root_is_ignored_when_feature_is_disabled() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let target_dir = std::env::temp_dir().join(format!(
        "nwflash-vmp-disabled-bogus-sdk-{}",
        std::process::id()
    ));
    let output = Command::new(env!("CARGO"))
        .current_dir(manifest_dir)
        .args([
            "check",
            "--target-dir",
            target_dir.to_str().expect("temporary path must be UTF-8"),
        ])
        .env("NWFLASH_VMP_SDK_ROOT", "relative-and-nonexistent")
        .output()
        .expect("nested Cargo check must start");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "no-feature build failed: {stderr}");

    if target_dir.exists() {
        std::fs::remove_dir_all(&target_dir).expect("temporary target directory must be removable");
    }
}

#[cfg(all(windows, target_arch = "x86_64", not(feature = "vmp-sdk")))]
#[test]
#[ignore = "spawns a nested Cargo build to exercise build.rs"]
fn relative_sdk_root_feature_build_fails_before_filesystem_access() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let target_dir =
        std::env::temp_dir().join(format!("nwflash-vmp-relative-sdk-{}", std::process::id()));
    let output = Command::new(env!("CARGO"))
        .current_dir(manifest_dir)
        .args([
            "check",
            "--features",
            "vmp-sdk",
            "--target-dir",
            target_dir.to_str().expect("temporary path must be UTF-8"),
        ])
        .env("NWFLASH_VMP_SDK_ROOT", "relative-and-nonexistent")
        .output()
        .expect("nested Cargo check must start");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "relative SDK root must fail");
    assert!(
        stderr.contains("must be a fully qualified path"),
        "unexpected build failure: {stderr}"
    );

    if target_dir.exists() {
        std::fs::remove_dir_all(&target_dir).expect("temporary target directory must be removable");
    }
}
