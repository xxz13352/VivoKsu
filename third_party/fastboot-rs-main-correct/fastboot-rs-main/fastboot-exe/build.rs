// 构建脚本 - 嵌入 Windows 版本信息

fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set("ProductName", "Fastboot-RS")
            .set(
                "FileDescription",
                "Android Fastboot Tool - High Performance",
            )
            .set(
                "LegalCopyright",
                "Copyright (C) 2024-2026 GriefRedd & AndyWu",
            )
            .set("CompanyName", "GriefRedd & AndyWu")
            .set("OriginalFilename", "fastboot.exe")
            .set("ProductVersion", env!("CARGO_PKG_VERSION"))
            .set("FileVersion", env!("CARGO_PKG_VERSION"));

        if let Err(e) = res.compile() {
            eprintln!("Warning: Failed to compile Windows resources: {}", e);
        }
    }
}
