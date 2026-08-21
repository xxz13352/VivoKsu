use nwflash_tauri::run_app;

fn main() {
    if let Err(error) = run_app(tauri::generate_context!()) {
        panic!("nwflash desktop failed: {error}");
    }
}
