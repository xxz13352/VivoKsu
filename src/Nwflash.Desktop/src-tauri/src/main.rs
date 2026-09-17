use nwflash_protection::VmpIntegrityProbe;
use nwflash_tauri::{
    effective_capabilities_json, evaluate_protected_release_probe, run_app,
    ProtectedReleaseProbeAction, EFFECTIVE_CAPABILITIES_PROBE_ARGUMENT,
};

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if let ProtectedReleaseProbeAction::Report(report) =
        evaluate_protected_release_probe(&arguments, &VmpIntegrityProbe)
    {
        println!("{}", report.to_json_line());
        std::process::exit(report.exit_code as i32);
    }

    let context = tauri::generate_context!();
    if arguments == [EFFECTIVE_CAPABILITIES_PROBE_ARGUMENT] {
        match effective_capabilities_json(context.config()) {
            Ok(report) => {
                println!("{report}");
                return;
            }
            Err(error) => {
                eprintln!("effective capability probe failed: {error}");
                std::process::exit(45);
            }
        }
    }

    if let Err(error) = run_app(context) {
        panic!("nwflash desktop failed: {error}");
    }

    // 事件循环正常返回后由保护圈内同步终结(正常退出码 0),防止已接受
    // 的完整性退出在事件循环之外被继续拦截驻留。
    nwflash_protection::terminate_protected_process(0);
}
