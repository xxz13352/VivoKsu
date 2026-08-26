use std::process::ExitCode;

use nwflash_protection::VmpIntegrityProbe;
use nwflash_tauri::{
    effective_capabilities_json, evaluate_protected_release_probe, run_app,
    ProtectedReleaseProbeAction, EFFECTIVE_CAPABILITIES_PROBE_ARGUMENT,
};

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if let ProtectedReleaseProbeAction::Report(report) =
        evaluate_protected_release_probe(&arguments, &VmpIntegrityProbe)
    {
        println!("{}", report.to_json_line());
        return ExitCode::from(report.exit_code);
    }

    let context = tauri::generate_context!();
    if arguments == [EFFECTIVE_CAPABILITIES_PROBE_ARGUMENT] {
        match effective_capabilities_json(context.config()) {
            Ok(report) => {
                println!("{report}");
                return ExitCode::SUCCESS;
            }
            Err(error) => {
                eprintln!("effective capability probe failed: {error}");
                return ExitCode::from(45);
            }
        }
    }

    if let Err(error) = run_app(context) {
        panic!("nwflash desktop failed: {error}");
    }

    ExitCode::SUCCESS
}
