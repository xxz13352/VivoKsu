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
pub trait IntegrityProbe: Send + Sync {
    fn signals(&self) -> IntegritySignals;
}

/// Raw VMProtect image oracles used by the desktop release smoke mode.
/// Unavailable probes use `None` instead of presenting a missing SDK as a
/// negative result from either VMProtect function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseImageProbe {
    pub available: bool,
    pub vmprotect_is_protected: Option<bool>,
    pub vmprotect_is_valid_image_crc: Option<bool>,
}

pub fn probe_release_image(probe: &dyn IntegrityProbe) -> ReleaseImageProbe {
    let signals = probe.signals();
    match signals.availability {
        ProbeAvailability::Available => ReleaseImageProbe {
            available: true,
            vmprotect_is_protected: Some(signals.image_protected),
            vmprotect_is_valid_image_crc: Some(signals.image_crc_valid),
        },
        ProbeAvailability::Unavailable => ReleaseImageProbe {
            available: false,
            vmprotect_is_protected: None,
            vmprotect_is_valid_image_crc: None,
        },
    }
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

/// 终结器叶子：进程退出的唯一权威同步终结点。“决定退出并携带退出码”
/// 收在 VMProtect 区域内，退出监督器与 Tauri 事件循环之后的存活都必须
/// 经由这里终结——圈外任何单点补丁都无法让已判定的退出失效。
/// 正常收尾传 0；完整性/篡改终局传 [`PROTECTED_EXIT_CODE`]。
#[inline(never)]
#[export_name = "nwflash_protection_terminate_process"]
pub fn terminate_protected_process(exit_code: i32) -> ! {
    begin_terminal_exit();
    let code = exit_code;
    end_marker();
    // 退出系统调用本身在 marker 区外执行:VMProtect 结束后进程立即消亡,
    // 区内只需要覆盖“决定退出并携带退出码”。
    std::process::exit(code)
}

/// 与退出监督器协商的受保护退出码。保留 70 与既有
/// `PROTECTED_EXIT_CODE` 语义一致(TAMPER/完整性终局退出)。
pub const PROTECTED_EXIT_CODE: i32 = 70;

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
    begin_image_integrity_dispatch();
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

    let outcome = ImageIntegrityOutcome { status, telemetry };
    end_marker();
    outcome
}

/// Compares normalized build identifiers at the dedicated mutation boundary.
#[inline(never)]
#[export_name = "nwflash_protection_build_identity_matches"]
pub fn build_identity_matches(expected: &str, actual: &str) -> bool {
    begin_build_identity();
    let matches = expected == actual;
    end_marker();
    matches
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

#[inline(always)]
pub(crate) fn begin_login_lease_acceptance() {
    #[cfg(feature = "vmp-sdk")]
    unsafe {
        VMProtectBeginUltra(c"NWFlash.LoginLeaseAcceptance".as_ptr())
    }
}

#[inline(always)]
pub(crate) fn begin_heartbeat_lease_classification() {
    #[cfg(feature = "vmp-sdk")]
    unsafe {
        VMProtectBeginVirtualization(c"NWFlash.HeartbeatLeaseClassification".as_ptr())
    }
}

#[inline(always)]
pub(crate) fn begin_operation_admission() {
    #[cfg(feature = "vmp-sdk")]
    unsafe {
        VMProtectBeginUltra(c"NWFlash.OperationAdmission".as_ptr())
    }
}

#[inline(always)]
fn begin_image_integrity_dispatch() {
    #[cfg(feature = "vmp-sdk")]
    unsafe {
        VMProtectBeginVirtualization(c"NWFlash.ImageIntegrityDispatch".as_ptr())
    }
}

#[inline(always)]
fn begin_build_identity() {
    #[cfg(feature = "vmp-sdk")]
    unsafe {
        VMProtectBeginMutation(c"NWFlash.BuildIdentity".as_ptr())
    }
}

#[inline(always)]
pub(crate) fn begin_trace_credential_sentinel() {
    #[cfg(feature = "vmp-sdk")]
    unsafe {
        VMProtectBeginUltra(c"NWFlash.TraceCredentialSentinel".as_ptr())
    }
}

#[inline(always)]
pub(crate) fn begin_terminal_exit() {
    #[cfg(feature = "vmp-sdk")]
    unsafe {
        VMProtectBeginUltra(c"NWFlash.TerminalExit".as_ptr())
    }
}

#[inline(always)]
pub(crate) fn end_marker() {
    #[cfg(feature = "vmp-sdk")]
    unsafe {
        VMProtectEnd()
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
