fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "android")]
    {
        ksud::cli::run()
    }
    #[cfg(not(target_os = "android"))]
    {
        #[cfg(windows)]
        if std::env::args_os().len() == 1 {
            return ksud::cli_non_android::run_gui();
        }
        ksud::cli_non_android::run()
    }
}
