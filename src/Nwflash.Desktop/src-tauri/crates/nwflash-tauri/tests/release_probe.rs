use std::sync::atomic::{AtomicUsize, Ordering};

use nwflash_protection::{IntegrityProbe, IntegritySignals};
use nwflash_tauri::{
    evaluate_protected_release_probe, ProtectedReleaseProbeAction, PROTECTED_RELEASE_PROBE_ARGUMENT,
};

struct CountingProbe {
    calls: AtomicUsize,
    signals: IntegritySignals,
}

impl CountingProbe {
    fn new(signals: IntegritySignals) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            signals,
        }
    }
}

impl IntegrityProbe for CountingProbe {
    fn signals(&self) -> IntegritySignals {
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.signals
    }
}

fn arguments(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn ordinary_arguments_leave_the_normal_tauri_path_untouched() {
    let probe = CountingProbe::new(IntegritySignals::unavailable());

    let action = evaluate_protected_release_probe(&arguments(&["--ordinary"]), &probe);

    assert_eq!(action, ProtectedReleaseProbeAction::NotRequested);
    assert_eq!(probe.calls.load(Ordering::Acquire), 0);
}

#[test]
fn exact_probe_argument_returns_machine_readable_success() {
    let probe = CountingProbe::new(IntegritySignals::available(true, true, true, true));

    let action =
        evaluate_protected_release_probe(&arguments(&[PROTECTED_RELEASE_PROBE_ARGUMENT]), &probe);
    let ProtectedReleaseProbeAction::Report(report) = action else {
        panic!("probe invocation must return a report");
    };

    assert_eq!(report.exit_code, 0);
    assert_eq!(probe.calls.load(Ordering::Acquire), 1);
    assert_eq!(
        report.to_json_line(),
        r#"{"schema":1,"mode":"nwflash-protected-release-probe","probe_available":true,"VMProtectIsProtected":true,"VMProtectIsValidImageCRC":true,"build_id":"debug-build","exit_code":0}"#
    );
}

#[test]
fn probe_exit_codes_distinguish_protection_crc_and_availability() {
    let cases = [
        (IntegritySignals::available(false, true, false, false), 41),
        (IntegritySignals::available(true, false, false, false), 42),
        (IntegritySignals::unavailable(), 43),
    ];

    for (signals, expected_exit) in cases {
        let probe = CountingProbe::new(signals);
        let ProtectedReleaseProbeAction::Report(report) = evaluate_protected_release_probe(
            &arguments(&[PROTECTED_RELEASE_PROBE_ARGUMENT]),
            &probe,
        ) else {
            panic!("probe invocation must return a report");
        };
        assert_eq!(report.exit_code, expected_exit);
        assert_eq!(probe.calls.load(Ordering::Acquire), 1);
    }
}

#[test]
fn malformed_probe_invocation_is_code_44_and_never_calls_the_sdk() {
    let probe = CountingProbe::new(IntegritySignals::available(true, true, false, false));

    let action = evaluate_protected_release_probe(
        &arguments(&[PROTECTED_RELEASE_PROBE_ARGUMENT, "unexpected"]),
        &probe,
    );
    let ProtectedReleaseProbeAction::Report(report) = action else {
        panic!("malformed probe invocation must return a report");
    };

    assert_eq!(report.exit_code, 44);
    assert_eq!(probe.calls.load(Ordering::Acquire), 0);
    assert!(report.to_json_line().contains(r#""exit_code":44"#));
}
