use nwflash_protection::{probe_release_image, IntegrityProbe};

pub const PROTECTED_RELEASE_PROBE_ARGUMENT: &str = "--nwflash-protected-release-probe";
pub const EFFECTIVE_CAPABILITIES_PROBE_ARGUMENT: &str =
    "--nwflash-effective-capabilities-probe";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtectedReleaseProbeAction {
    NotRequested,
    Report(ProtectedReleaseProbeReport),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedReleaseProbeReport {
    pub probe_available: bool,
    pub vmprotect_is_protected: Option<bool>,
    pub vmprotect_is_valid_image_crc: Option<bool>,
    pub exit_code: u8,
}

impl ProtectedReleaseProbeReport {
    fn malformed() -> Self {
        Self {
            probe_available: false,
            vmprotect_is_protected: None,
            vmprotect_is_valid_image_crc: None,
            exit_code: 44,
        }
    }

    pub fn to_json_line(&self) -> String {
        fn value(value: Option<bool>) -> &'static str {
            match value {
                Some(true) => "true",
                Some(false) => "false",
                None => "null",
            }
        }

        format!(
            concat!(
                r#"{{"schema":1,"mode":"nwflash-protected-release-probe","probe_available":{},"#,
                r#""VMProtectIsProtected":{},"VMProtectIsValidImageCRC":{},"exit_code":{}}}"#
            ),
            self.probe_available,
            value(self.vmprotect_is_protected),
            value(self.vmprotect_is_valid_image_crc),
            self.exit_code,
        )
    }
}

pub fn evaluate_protected_release_probe(
    arguments: &[String],
    probe: &dyn IntegrityProbe,
) -> ProtectedReleaseProbeAction {
    let probe_requested = arguments
        .iter()
        .any(|argument| argument.starts_with(PROTECTED_RELEASE_PROBE_ARGUMENT));
    if !probe_requested {
        return ProtectedReleaseProbeAction::NotRequested;
    }

    if arguments != [PROTECTED_RELEASE_PROBE_ARGUMENT] {
        return ProtectedReleaseProbeAction::Report(ProtectedReleaseProbeReport::malformed());
    }

    let observed = probe_release_image(probe);
    let exit_code = if !observed.available {
        43
    } else if observed.vmprotect_is_protected != Some(true) {
        41
    } else if observed.vmprotect_is_valid_image_crc != Some(true) {
        42
    } else {
        0
    };

    ProtectedReleaseProbeAction::Report(ProtectedReleaseProbeReport {
        probe_available: observed.available,
        vmprotect_is_protected: observed.vmprotect_is_protected,
        vmprotect_is_valid_image_crc: observed.vmprotect_is_valid_image_crc,
        exit_code,
    })
}

pub fn effective_capabilities_json(config: &tauri::Config) -> Result<String, serde_json::Error> {
    serde_json::to_string(&serde_json::json!({
        "schema": 1,
        "mode": "nwflash-effective-capabilities-probe",
        "capabilities": &config.app.security.capabilities,
    }))
}
