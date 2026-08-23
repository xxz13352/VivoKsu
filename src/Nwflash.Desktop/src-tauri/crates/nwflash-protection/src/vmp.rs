#[cfg(feature = "vmp-sdk")]
use std::ffi::c_char;

/// Signals returned by an integrity probe at a synchronous safety boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegritySignals {
    availability: ProbeAvailability,
    image_protected: bool,
    image_crc_valid: bool,
    debugger_present: bool,
    virtual_machine_present: bool,
}

impl IntegritySignals {
    pub const fn available(
        image_protected: bool,
        image_crc_valid: bool,
        debugger_present: bool,
        virtual_machine_present: bool,
    ) -> Self {
        Self {
            availability: ProbeAvailability::Available,
            image_protected,
            image_crc_valid,
            debugger_present,
            virtual_machine_present,
        }
    }

    pub const fn unavailable() -> Self {
        Self {
            availability: ProbeAvailability::Unavailable,
            image_protected: false,
            image_crc_valid: false,
            debugger_present: false,
            virtual_machine_present: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeAvailability {
    Available,
    Unavailable,
}

/// Injectable source of normalized VMProtect integrity and telemetry signals.
pub trait IntegrityProbe {
    fn signals(&self) -> IntegritySignals;
}

/// Production VMProtect SDK probe. Without `vmp-sdk`, it is explicitly unavailable.
#[derive(Debug, Default, Clone, Copy)]
pub struct VmpIntegrityProbe;

impl IntegrityProbe for VmpIntegrityProbe {
    fn signals(&self) -> IntegritySignals {
        #[cfg(feature = "vmp-sdk")]
        {
            // C++ `bool` in the SDK ABI is one byte. The imported functions use `u8`
            // so this boundary does not depend on Rust's source-level `bool` ABI.
            unsafe {
                IntegritySignals::available(
                    VMProtectIsProtected() != 0,
                    VMProtectIsValidImageCRC() != 0,
                    VMProtectIsDebuggerPresent(0) != 0,
                    VMProtectIsVirtualMachinePresent() != 0,
                )
            }
        }

        #[cfg(not(feature = "vmp-sdk"))]
        {
            IntegritySignals::unavailable()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageIntegrityFailure {
    ImageNotProtected,
    InvalidImageCrc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageIntegrityStatus {
    Valid,
    Failure(ImageIntegrityFailure),
    ProbeUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityTelemetry {
    None,
    DebuggerPresent,
    VirtualMachinePresent,
    DebuggerAndVirtualMachinePresent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageIntegrityOutcome {
    pub status: ImageIntegrityStatus,
    pub telemetry: IntegrityTelemetry,
}

/// Classifies image protection and CRC as integrity policy, while retaining
/// debugger and virtual-machine detections as telemetry-only signals.
#[inline(never)]
#[export_name = "nwflash_protection_verify_image_integrity"]
pub fn verify_image_integrity(probe: &dyn IntegrityProbe) -> ImageIntegrityOutcome {
    let _marker = MarkerScope::enter(MarkerBoundary::ImageIntegrityDispatch);
    let signals = probe.signals();
    let telemetry = classify_telemetry(signals);
    let status = match signals.availability {
        ProbeAvailability::Unavailable => ImageIntegrityStatus::ProbeUnavailable,
        ProbeAvailability::Available if !signals.image_protected => {
            ImageIntegrityStatus::Failure(ImageIntegrityFailure::ImageNotProtected)
        }
        ProbeAvailability::Available if !signals.image_crc_valid => {
            ImageIntegrityStatus::Failure(ImageIntegrityFailure::InvalidImageCrc)
        }
        ProbeAvailability::Available => ImageIntegrityStatus::Valid,
    };

    ImageIntegrityOutcome { status, telemetry }
}

/// Compares normalized build identifiers at the dedicated mutation boundary.
#[inline(never)]
#[export_name = "nwflash_protection_build_identity_matches"]
pub fn build_identity_matches(expected: &str, actual: &str) -> bool {
    let _marker = MarkerScope::enter(MarkerBoundary::BuildIdentity);
    expected == actual
}

pub const fn marker_backend_available() -> bool {
    cfg!(feature = "vmp-sdk")
}

fn classify_telemetry(signals: IntegritySignals) -> IntegrityTelemetry {
    match (signals.debugger_present, signals.virtual_machine_present) {
        (false, false) => IntegrityTelemetry::None,
        (true, false) => IntegrityTelemetry::DebuggerPresent,
        (false, true) => IntegrityTelemetry::VirtualMachinePresent,
        (true, true) => IntegrityTelemetry::DebuggerAndVirtualMachinePresent,
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum MarkerBoundary {
    LoginLeaseAcceptance,
    HeartbeatLeaseClassification,
    OperationAdmission,
    ImageIntegrityDispatch,
    BuildIdentity,
}

pub(crate) struct MarkerScope {
    #[cfg(feature = "vmp-sdk")]
    active: bool,
}

impl MarkerScope {
    pub(crate) fn enter(boundary: MarkerBoundary) -> Self {
        #[cfg(feature = "vmp-sdk")]
        unsafe {
            match boundary {
                MarkerBoundary::LoginLeaseAcceptance => {
                    VMProtectBeginUltra(c"NWFlash.LoginLeaseAcceptance".as_ptr())
                }
                MarkerBoundary::HeartbeatLeaseClassification => {
                    VMProtectBeginVirtualization(c"NWFlash.HeartbeatLeaseClassification".as_ptr())
                }
                MarkerBoundary::OperationAdmission => {
                    VMProtectBeginUltra(c"NWFlash.OperationAdmission".as_ptr())
                }
                MarkerBoundary::ImageIntegrityDispatch => {
                    VMProtectBeginVirtualization(c"NWFlash.ImageIntegrityDispatch".as_ptr())
                }
                MarkerBoundary::BuildIdentity => {
                    VMProtectBeginMutation(c"NWFlash.BuildIdentity".as_ptr())
                }
            }
            Self { active: true }
        }

        #[cfg(not(feature = "vmp-sdk"))]
        {
            let _ = boundary;
            Self {}
        }
    }
}

impl Drop for MarkerScope {
    fn drop(&mut self) {
        #[cfg(feature = "vmp-sdk")]
        if self.active {
            unsafe { VMProtectEnd() }
        }
    }
}

#[cfg(feature = "vmp-sdk")]
unsafe extern "system" {
    fn VMProtectBeginVirtualization(name: *const c_char);
    fn VMProtectBeginMutation(name: *const c_char);
    fn VMProtectBeginUltra(name: *const c_char);
    fn VMProtectEnd();
    fn VMProtectIsProtected() -> u8;
    fn VMProtectIsDebuggerPresent(check_kernel_mode: u8) -> u8;
    fn VMProtectIsVirtualMachinePresent() -> u8;
    fn VMProtectIsValidImageCRC() -> u8;
}
