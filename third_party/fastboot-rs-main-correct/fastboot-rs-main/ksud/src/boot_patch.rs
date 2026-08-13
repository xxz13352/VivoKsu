use std::fs::File;
use std::io::{BufReader, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use regex_lite::Regex;
use which::which;

#[cfg(target_os = "android")]
mod android {
    use super::{PermissionsExt, Result, do_cpio_cmd};
    pub(super) use crate::defs::{BACKUP_FILENAME, KSU_BACKUP_DIR, KSU_BACKUP_FILE_PREFIX};
    use crate::utils;
    use anyhow::{Context, anyhow, bail, ensure};
    use regex_lite::Regex;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    pub(super) fn ensure_gki_kernel() -> Result<()> {
        let version = get_kernel_version()?;
        let is_gki = version.0 == 5 && version.1 >= 10 || version.2 > 5;
        ensure!(is_gki, "only support GKI kernel");
        Ok(())
    }

    pub fn get_kernel_version() -> Result<(i32, i32, i32)> {
        let uname = rustix::system::uname();
        let version = uname.release().to_string_lossy();
        let re = Regex::new(r"(\d+)\.(\d+)\.(\d+)")?;
        if let Some(captures) = re.captures(&version) {
            let major = captures
                .get(1)
                .and_then(|m| m.as_str().parse::<i32>().ok())
                .unwrap_or(0);
            let minor = captures
                .get(2)
                .and_then(|m| m.as_str().parse::<i32>().ok())
                .unwrap_or(0);
            let patch = captures
                .get(3)
                .and_then(|m| m.as_str().parse::<i32>().ok())
                .unwrap_or(0);
            Ok((major, minor, patch))
        } else {
            Err(anyhow!("Invalid kernel version string"))
        }
    }

    fn parse_kmi(version: &str) -> Result<String> {
        let re = Regex::new(r"(.* )?(\d+\.\d+)(\S+)?(android\d+)(.*)")?;
        let cap = re
            .captures(version)
            .ok_or_else(|| anyhow::anyhow!("Failed to get KMI from boot/modules"))?;
        let android_version = cap.get(4).map_or("", |m| m.as_str());
        let kernel_version = cap.get(2).map_or("", |m| m.as_str());
        Ok(format!("{android_version}-{kernel_version}"))
    }

    fn parse_kmi_from_uname() -> Result<String> {
        let uname = rustix::system::uname();
        let version = uname.release().to_string_lossy();
        parse_kmi(&version)
    }

    fn parse_kmi_from_modules() -> Result<String> {
        use std::io::BufRead;
        let modfile = std::fs::read_dir("/vendor/lib/modules")?
            .filter_map(Result::ok)
            .find(|entry| entry.path().extension().is_some_and(|ext| ext == "ko"))
            .map(|entry| entry.path())
            .ok_or_else(|| anyhow!("No kernel module found"))?;
        let output = Command::new("modinfo").arg(modfile).output()?;
        for line in output.stdout.lines().map_while(Result::ok) {
            if line.starts_with("vermagic") {
                return parse_kmi(&line);
            }
        }
        bail!("Parse KMI from modules failed")
    }

    pub fn get_current_kmi() -> Result<String> {
        parse_kmi_from_uname().or_else(|_| parse_kmi_from_modules())
    }

    fn calculate_sha1(file_path: impl AsRef<Path>) -> Result<String> {
        use sha1::Digest;
        use std::io::Read;
        let mut file = std::fs::File::open(file_path.as_ref())?;
        let mut hasher = sha1::Sha1::new();
        let mut buffer = [0; 1024];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    pub(super) fn do_backup(
        magiskboot: &Path,
        workdir: &Path,
        cpio_path: &Path,
        image: &Path,
        verbose: bool,
    ) -> Result<()> {
        let sha1 = calculate_sha1(image)?;
        let filename = format!("{KSU_BACKUP_FILE_PREFIX}{sha1}");
        if verbose {
            println!("- Backup stock boot image");
        }
        let target = format!("{KSU_BACKUP_DIR}{filename}");
        std::fs::copy(image, &target).with_context(|| format!("backup to {target}"))?;
        std::fs::write(workdir.join(BACKUP_FILENAME), sha1.as_bytes()).context("write sha1")?;
        do_cpio_cmd(
            magiskboot,
            workdir,
            cpio_path,
            &format!("add 0755 {BACKUP_FILENAME} {BACKUP_FILENAME}"),
        )?;
        if verbose {
            println!("- Stock image has been backup to\n- {target}");
        }
        Ok(())
    }

    pub(super) fn flash_boot(bootdevice: &Option<String>, new_boot: &PathBuf) -> Result<()> {
        let Some(bootdevice) = bootdevice else {
            bail!("boot device not found")
        };
        let status = Command::new("blockdev")
            .arg("--setrw")
            .arg(bootdevice)
            .status()?;
        ensure!(status.success(), "set boot device rw failed");
        dd(new_boot, bootdevice).context("flash boot failed")?;
        Ok(())
    }

    pub fn choose_boot_partition(
        kmi: &str,
        is_replace_kernel: bool,
        partition: &Option<String>,
    ) -> String {
        let slot_suffix = get_slot_suffix(false);
        let skip_init_boot = kmi.starts_with("android12-");
        let init_boot_exist =
            Path::new(&format!("/dev/block/by-name/init_boot{slot_suffix}")).exists();
        if let Some(part) = partition {
            return match part.as_str() {
                "boot" | "init_boot" | "vendor_boot" => part.clone(),
                _ => "boot".to_string(),
            };
        }
        if !is_replace_kernel && init_boot_exist && !skip_init_boot {
            return "init_boot".to_string();
        }
        "boot".to_string()
    }

    pub fn get_slot_suffix(ota: bool) -> String {
        let mut slot_suffix = utils::getprop("ro.boot.slot_suffix").unwrap_or_default();
        if !slot_suffix.is_empty() && ota {
            slot_suffix = if slot_suffix == "_a" {
                "_b".to_string()
            } else {
                "_a".to_string()
            };
        }
        slot_suffix
    }

    pub(super) fn post_ota() -> Result<()> {
        use crate::assets::BOOTCTL_PATH;
        use crate::defs::ADB_DIR;
        let status = Command::new(BOOTCTL_PATH).arg("hal-info").status()?;
        if !status.success() {
            return Ok(());
        }
        let current_slot = Command::new(BOOTCTL_PATH)
            .arg("get-current-slot")
            .output()?
            .stdout;
        let current_slot = String::from_utf8(current_slot)?;
        let target_slot = i32::from(current_slot.trim() == "0");
        Command::new(BOOTCTL_PATH)
            .arg(format!("set-active-boot-slot {target_slot}"))
            .status()?;
        let post_fs_data = Path::new(ADB_DIR).join("post-fs-data.d");
        utils::ensure_dir_exists(&post_fs_data)?;
        let post_ota_sh = post_fs_data.join("post_ota.sh");
        let sh_content = format!(
            "\n{BOOTCTL_PATH} mark-boot-successful\nrm -f {BOOTCTL_PATH}\nrm -f /data/adb/post-fs-data.d/post_ota.sh\n"
        );
        std::fs::write(&post_ota_sh, sh_content)?;
        #[cfg(unix)]
        std::fs::set_permissions(post_ota_sh, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    pub(super) fn dd<P: AsRef<Path>, Q: AsRef<Path>>(ifile: P, ofile: Q) -> Result<()> {
        let status = Command::new("dd")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .arg(format!("if={}", ifile.as_ref().display()))
            .arg(format!("of={}", ofile.as_ref().display()))
            .status()?;
        ensure!(status.success(), "dd failed");
        Ok(())
    }
}

#[cfg(target_os = "android")]
pub use android::*;

fn scan_file_for_kmi(file_path: &Path) -> Result<String> {
    let file = File::open(file_path).context("Failed to open kernel file")?;
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    reader
        .read_to_end(&mut buffer)
        .context("Failed to read kernel file")?;

    let target = b"Linux version";
    let re =
        Regex::new(r"(?:.* )?(\d+\.\d+)(?:\S+)?(android\d+)").context("Failed to compile regex")?;

    let mut search_start = 0;
    while let Some(pos) = buffer[search_start..]
        .windows(target.len())
        .position(|window| window == target)
    {
        let absolute_pos = search_start + pos;
        let mut start = absolute_pos;
        while start > 0
            && buffer[start - 1] != 0
            && buffer[start - 1] != b'\n'
            && buffer[start - 1] != b'\r'
        {
            start -= 1;
        }
        let mut end = absolute_pos + target.len();
        while end < buffer.len() && buffer[end] != 0 && buffer[end] != b'\n' && buffer[end] != b'\r'
        {
            end += 1;
        }

        if let Ok(line) = std::str::from_utf8(&buffer[start..end]) {
            if let Some(caps) = re.captures(line) {
                if let (Some(kernel_version), Some(android_version)) = (caps.get(1), caps.get(2)) {
                    return Ok(format!(
                        "{}-{}",
                        android_version.as_str(),
                        kernel_version.as_str()
                    ));
                }
            }
        }
        search_start = end;
    }
    bail!("未找到 KMI 字符串")
}

fn parse_kmi_from_kernel(
    magiskboot: &Path,
    kernel: &PathBuf,
    workdir: &Path,
    verbose: bool,
) -> Result<String> {
    if verbose {
        println!("正在扫描原生 kernel 文件...");
    }
    if let Ok(kmi) = scan_file_for_kmi(kernel) {
        if verbose {
            println!("在原生内核中成功定位 KMI");
        }
        return Ok(kmi);
    }

    if verbose {
        println!("判定为压缩内核，正在尝试强行解压...");
    }
    let kernel_dec = workdir.join("kernel_dec");
    if let Some(filename) = kernel.file_name() {
        let mut cmd = Command::new(magiskboot);
        cmd.current_dir(workdir)
            .arg("decompress")
            .arg(filename)
            .arg("kernel_dec");
        if !verbose {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
        let _ = cmd.status();
    }

    if kernel_dec.exists() {
        if verbose {
            println!("正在扫描解压后的内核文件...");
        }
        if let Ok(kmi) = scan_file_for_kmi(&kernel_dec) {
            if verbose {
                println!("在解压后的内核中成功定位 KMI");
            }
            return Ok(kmi);
        }
    }
    bail!("未能在内核底层找到标准的 KMI 基因。")
}

fn parse_kmi_from_boot(
    magiskboot: &Path,
    image: &PathBuf,
    workdir: &Path,
    verbose: bool,
) -> Result<String> {
    let image_path = workdir.join("image");
    std::fs::copy(image, &image_path).context("Failed to copy image")?;

    let mut cmd = Command::new(magiskboot);
    cmd.current_dir(workdir)
        .arg("unpack")
        .arg(image_path.file_name().unwrap());
    if !verbose {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
    }
    cmd.status()?;

    let unpacked_kernel = workdir.join("kernel");
    if unpacked_kernel.exists() {
        parse_kmi_from_kernel(magiskboot, &unpacked_kernel, workdir, verbose)
    } else {
        parse_kmi_from_kernel(magiskboot, &image_path, workdir, verbose)
    }
}

fn do_cpio_cmd(magiskboot: &Path, workdir: &Path, cpio_path: &Path, cmd: &str) -> Result<()> {
    Command::new(magiskboot)
        .current_dir(workdir)
        .arg("cpio")
        .arg(cpio_path.file_name().unwrap())
        .arg(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(())
}

fn do_cpio_batch(
    magiskboot: &Path,
    workdir: &Path,
    cpio_path: &Path,
    cmds: &[String],
    verbose: bool,
) -> Result<()> {
    if cmds.is_empty() {
        return Ok(());
    }
    let mut command = Command::new(magiskboot);
    command
        .current_dir(workdir)
        .arg("cpio")
        .arg(cpio_path.file_name().unwrap());

    for cmd in cmds {
        command.arg(cmd);
    }

    if !verbose {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    command.status()?;
    Ok(())
}

fn is_magisk_patched(magiskboot: &Path, workdir: &Path, cpio_path: &Path) -> Result<bool> {
    let status = Command::new(magiskboot)
        .current_dir(workdir)
        .arg("cpio")
        .arg(cpio_path.file_name().unwrap())
        .arg("test")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.code() == Some(1))
}

fn is_kernelsu_patched(magiskboot: &Path, workdir: &Path, cpio_path: &Path) -> Result<bool> {
    let status = Command::new(magiskboot)
        .current_dir(workdir)
        .arg("cpio")
        .arg(cpio_path.file_name().unwrap())
        .arg("exists kernelsu.ko")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

fn find_magiskboot(magiskboot_path: Option<PathBuf>, workdir: &Path) -> Result<PathBuf> {
    let magiskboot = {
        if which("magiskboot.exe").is_ok() || which("magiskboot").is_ok() {
            #[cfg(target_os = "android")]
            let _ = crate::assets::ensure_binaries(true);
            "magiskboot.exe".into()
        } else {
            let magiskboot = if let Some(magiskboot_path) = magiskboot_path {
                std::fs::canonicalize(magiskboot_path)?
            } else {
                let magiskboot_path = workdir.join("magiskboot.exe");
                crate::assets::copy_assets_to_file("magiskboot.exe", &magiskboot_path).context(
                    "提取内嵌 magiskboot.exe 失败，请确保你已经把它放进 bin/aarch64 文件夹里了！",
                )?;
                magiskboot_path
            };
            ensure!(magiskboot.exists(), "{} is not exist", magiskboot.display());
            #[cfg(unix)]
            let _ = std::fs::set_permissions(&magiskboot, std::fs::Permissions::from_mode(0o755));
            magiskboot
        }
    };
    Ok(magiskboot)
}

fn find_boot_image(
    image: &Option<PathBuf>,
    kmi: &str,
    ota: bool,
    is_replace_kernel: bool,
    workdir: &Path,
    partition: &Option<String>,
    verbose: bool,
) -> Result<(PathBuf, Option<String>)> {
    #[cfg(not(target_os = "android"))]
    let _ = (kmi, ota, is_replace_kernel, workdir, partition);

    let bootimage;
    #[cfg(target_os = "android")]
    let mut bootdevice = None;
    #[cfg(not(target_os = "android"))]
    let bootdevice = None;
    if let Some(ref image) = *image {
        ensure!(image.exists(), "boot image not found");
        bootimage = std::fs::canonicalize(image)?;
    } else {
        #[cfg(not(target_os = "android"))]
        {
            if verbose {
                println!(
                    "- Current OS is not android, refusing auto bootimage/bootdevice detection"
                );
            }
            bail!("Please specify a boot image");
        }
        #[cfg(target_os = "android")]
        {
            let slot_suffix = get_slot_suffix(ota);
            let boot_partition_name = choose_boot_partition(kmi, is_replace_kernel, partition);
            let boot_partition = format!("/dev/block/by-name/{boot_partition_name}{slot_suffix}");
            if verbose {
                println!("- Bootdevice: {boot_partition}");
            }
            let tmp_boot_path = workdir.join("boot.img");
            dd(&boot_partition, &tmp_boot_path)?;
            ensure!(tmp_boot_path.exists(), "boot image not found");
            bootimage = tmp_boot_path;
            bootdevice = Some(boot_partition);
        }
    }
    Ok((bootimage, bootdevice))
}

#[allow(clippy::struct_excessive_bools)]
#[derive(clap::Args, Debug)]
pub struct BootPatchArgs {
    #[arg(short, long)]
    pub boot: Option<PathBuf>,
    #[arg(short, long)]
    pub kernel: Option<PathBuf>,
    #[arg(short, long)]
    pub module: Option<PathBuf>,
    #[arg(short, long, requires("module"))]
    pub init: Option<PathBuf>,
    #[cfg(target_os = "android")]
    #[arg(short = 'u', long, default_value = "false")]
    pub ota: bool,
    #[cfg(target_os = "android")]
    #[arg(short, long, default_value = "false")]
    pub flash: bool,
    #[cfg(target_os = "android")]
    #[arg(short, long, default_value = None)]
    pub out: Option<PathBuf>,
    #[cfg(not(target_os = "android"))]
    #[arg(short, long, default_value = None)]
    pub out: Option<PathBuf>,
    #[arg(long, default_value = None)]
    pub magiskboot: Option<PathBuf>,
    #[arg(long, default_value = None)]
    pub kmi: Option<String>,
    #[arg(long, value_enum, default_value_t = KernelSuSource::Embedded, conflicts_with = "module")]
    pub kernelsu_source: KernelSuSource,
    #[cfg(target_os = "android")]
    #[arg(long, default_value = None)]
    pub partition: Option<String>,
    #[cfg(target_os = "android")]
    #[arg(long, default_value = None)]
    pub out_name: Option<String>,
    #[cfg(not(target_os = "android"))]
    #[arg(long, default_value = None)]
    pub out_name: Option<String>,
    #[arg(long, default_value = None)]
    pub cmdline: Option<String>,
    #[arg(long, default_value = "false")]
    allow_shell: bool,
    #[arg(long, default_value = "false")]
    enable_adbd: bool,
    #[arg(long, required = false)]
    adb_debug_prop: Option<String>,
    #[arg(long, default_value = "false")]
    no_install: bool,
    #[arg(short = 'v', long, default_value = "false")]
    pub verbose: bool,
}

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum KernelSuSource {
    #[default]
    Embedded,
    Official,
    #[value(name = "sukisu-ultra")]
    SukiSuUltra,
    #[value(name = "kernelsu-next")]
    KernelSuNext,
    #[value(name = "wild-ksu")]
    WildKsu,
}

impl KernelSuSource {
    const fn distribution(self) -> Option<crate::assets::KernelSuDistribution> {
        use crate::assets::KernelSuDistribution;

        match self {
            Self::Embedded => None,
            Self::Official => Some(KernelSuDistribution::Official),
            Self::SukiSuUltra => Some(KernelSuDistribution::SukiSuUltra),
            Self::KernelSuNext => Some(KernelSuDistribution::KernelSuNext),
            Self::WildKsu => Some(KernelSuDistribution::WildKsu),
        }
    }
}

impl BootPatchArgs {
    #[must_use]
    pub fn for_remote_patch(
        boot: PathBuf,
        out_dir: PathBuf,
        out_file_name: String,
        kmi: Option<String>,
        kernelsu_source: KernelSuSource,
    ) -> Self {
        Self {
            boot: Some(boot),
            kernel: None,
            module: None,
            init: None,
            #[cfg(target_os = "android")]
            ota: false,
            #[cfg(target_os = "android")]
            flash: false,
            out: Some(out_dir),
            magiskboot: None,
            kmi,
            kernelsu_source,
            #[cfg(target_os = "android")]
            partition: None,
            out_name: Some(out_file_name),
            cmdline: None,
            allow_shell: false,
            enable_adbd: false,
            adb_debug_prop: None,
            no_install: false,
            verbose: false,
        }
    }

    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn for_embedded_flash(
        boot: PathBuf,
        module: PathBuf,
        out_dir: PathBuf,
        out_file_name: String,
        kmi: Option<String>,
        verbose: bool,
    ) -> Self {
        Self {
            boot: Some(boot),
            kernel: None,
            module: Some(module),
            init: None,
            #[cfg(target_os = "android")]
            ota: false,
            #[cfg(target_os = "android")]
            flash: false,
            out: Some(out_dir),
            magiskboot: None,
            kmi,
            kernelsu_source: KernelSuSource::Embedded,
            #[cfg(target_os = "android")]
            partition: None,
            out_name: Some(out_file_name),
            cmdline: None,
            allow_shell: false,
            enable_adbd: false,
            adb_debug_prop: None,
            no_install: false,
            verbose,
        }
    }
}

pub fn patch(args: BootPatchArgs) -> Result<()> {
    let inner = move || {
        let BootPatchArgs {
            boot: image,
            init,
            kernel,
            module: kmod,
            out,
            magiskboot: magiskboot_path,
            kmi,
            kernelsu_source,
            out_name,
            cmdline,
            allow_shell,
            enable_adbd,
            adb_debug_prop,
            no_install,
            verbose,
            #[cfg(target_os = "android")]
            ota,
            #[cfg(target_os = "android")]
            flash,
            #[cfg(target_os = "android")]
            partition,
        } = args;

        if verbose {
            println!(include_str!("banner"));
        }
        let patch_file = image.is_some();

        #[cfg(target_os = "android")]
        if !patch_file {
            crate::android::ensure_gki_kernel()?;
        }

        let is_replace_kernel = kernel.is_some();
        if is_replace_kernel {
            ensure!(
                init.is_none() && kmod.is_none(),
                "init and module must not be specified."
            );
        }

        let tmpdir = tempfile::Builder::new()
            .prefix("KernelSU")
            .tempdir()
            .context("create temp dir failed")?;
        let workdir = tmpdir.path();
        let magiskboot = find_magiskboot(magiskboot_path, workdir)?;

        let kmi = kmi.map_or_else(
            || -> Result<_> {
                if kmod.is_some() {
                    return Ok(String::new());
                }
                #[cfg(target_os = "android")]
                match crate::android::get_current_kmi() {
                    Ok(value) => return Ok(value),
                    Err(e) => {
                        if verbose {
                            println!("- {e}");
                        }
                    }
                }
                Ok(if let Some(image_path) = &image {
                    if verbose {
                        println!(
                            "- Trying to auto detect KMI version for {}",
                            image_path.display()
                        );
                    }
                    parse_kmi_from_boot(&magiskboot, image_path, tmpdir.path(), verbose)?
                } else if let Some(kernel_path) = &kernel {
                    if verbose {
                        println!(
                            "- Trying to auto detect KMI version for {}",
                            kernel_path.display()
                        );
                    }
                    parse_kmi_from_kernel(&magiskboot, kernel_path, tmpdir.path(), verbose)?
                } else {
                    String::new()
                })
            },
            Ok,
        )?;

        #[cfg(target_os = "android")]
        let (bootimage, bootdevice) = find_boot_image(
            &image,
            &kmi,
            ota,
            is_replace_kernel,
            workdir,
            &partition,
            verbose,
        )?;
        #[cfg(not(target_os = "android"))]
        let (bootimage, _) = find_boot_image(
            &image,
            &kmi,
            false,
            is_replace_kernel,
            workdir,
            &None,
            verbose,
        )?;

        let bootimage = bootimage.as_path();
        let local_boot_name = "boot_orig.img";
        let local_boot_img = workdir.join(local_boot_name);
        std::fs::copy(bootimage, &local_boot_img).context("copy bootimage failed")?;

        #[cfg(target_os = "android")]
        let _ = crate::assets::ensure_binaries(false);

        if let Some(kernel) = kernel {
            std::fs::copy(kernel, workdir.join("kernel")).context("copy kernel from failed")?;
        }

        if verbose {
            println!("- Preparing assets");
        }
        let kmod_file = workdir.join("kernelsu.ko");
        if let Some(kmod) = kmod {
            std::fs::copy(kmod, kmod_file).context("copy kernel module failed")?;
        } else if !no_install {
            if verbose {
                println!("- KMI: {kmi}");
            }
            let name = format!("{kmi}_kernelsu.ko");
            match kernelsu_source.distribution() {
                None => crate::assets::copy_assets_to_file(&name, kmod_file),
                Some(distribution) => {
                    let tag = crate::assets::download_distribution_ko_to_file(
                        distribution,
                        &kmi,
                        kmod_file,
                    )?;
                    if verbose {
                        println!("- Downloaded {} {tag}", distribution.name());
                    }
                    Ok(())
                }
            }
            .with_context(|| format!("Failed to copy {name}"))?;
        }

        let init_file = workdir.join("init");
        if let Some(init) = init {
            std::fs::copy(init, init_file).context("copy init failed")?;
        } else if !no_install {
            crate::assets::copy_assets_to_file("ksuinit", init_file)
                .context("copy ksuinit failed")?;
        }

        if verbose {
            println!("- Unpacking boot image");
        }
        let mut cmd = Command::new(&magiskboot);
        cmd.current_dir(workdir).arg("unpack").arg(local_boot_name);
        if !verbose {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
        cmd.status()?;

        if let Some(ref cmdline_value) = cmdline {
            let header_path = workdir.join("header");
            std::fs::write(&header_path, format!("cmdline={cmdline_value}\n"))
                .context("write header file failed")?;
        }

        let mut ramdisk = workdir.join("ramdisk.cpio");
        if !ramdisk.exists() {
            ramdisk = workdir.join("vendor_ramdisk").join("init_boot.cpio");
        }
        if !ramdisk.exists() {
            ramdisk = workdir.join("vendor_ramdisk").join("ramdisk.cpio");
        }
        if !ramdisk.exists() {
            if verbose {
                println!("- No ramdisk, create by default");
            }
            ramdisk = "ramdisk.cpio".into();
        }
        let ramdisk = ramdisk.as_path();

        let mut cpio_cmds = Vec::new();

        if !no_install {
            ensure!(
                !is_magisk_patched(&magiskboot, workdir, ramdisk)?,
                "Cannot work with Magisk patched image"
            );

            if verbose {
                println!("- Adding KernelSU LKM");
            }
            let is_kernelsu_patched = is_kernelsu_patched(&magiskboot, workdir, ramdisk)?;

            if !is_kernelsu_patched {
                if do_cpio_cmd(&magiskboot, workdir, ramdisk, "exists init").is_ok() {
                    cpio_cmds.push("mv init init.real".to_string());
                }
            }
            cpio_cmds.push("add 0755 init init".to_string());
            cpio_cmds.push("add 0755 kernelsu.ko kernelsu.ko".to_string());

            #[cfg(target_os = "android")]
            if !is_kernelsu_patched
                && flash
                && let Err(e) =
                    crate::android::do_backup(&magiskboot, workdir, ramdisk, bootimage, verbose)
            {
                if verbose {
                    println!("- Backup stock image failed: {e}");
                }
            }
        }

        if allow_shell {
            File::create(workdir.join("ksu_allow_shell"))?;
            cpio_cmds.push("add 0644 ksu_allow_shell ksu_allow_shell".to_string());
        } else if do_cpio_cmd(&magiskboot, workdir, ramdisk, "exists ksu_allow_shell").is_ok() {
            cpio_cmds.push("rm ksu_allow_shell".to_string());
        }

        if enable_adbd || adb_debug_prop.is_some() {
            File::create(workdir.join("force_debuggable"))?;
            cpio_cmds.push("add 0644 force_debuggable force_debuggable".to_string());

            let prop_path = workdir.join("adb_debug.prop");
            let mut prop_file = File::create(prop_path)?;
            if enable_adbd {
                write!(
                    prop_file,
                    "ro.debuggable=1\nro.force.debuggable=1\nro.adb.secure=0\n"
                )?;
            }
            if let Some(props) = adb_debug_prop {
                prop_file.write_all(props.as_bytes())?;
            }
            cpio_cmds.push("add 0644 adb_debug.prop adb_debug.prop".to_string());
        } else {
            if do_cpio_cmd(&magiskboot, workdir, ramdisk, "exists force_debuggable").is_ok() {
                cpio_cmds.push("rm force_debuggable".to_string());
            }
            if do_cpio_cmd(&magiskboot, workdir, ramdisk, "exists adb_debug.prop").is_ok() {
                cpio_cmds.push("rm adb_debug.prop".to_string());
            }
        }

        if !cpio_cmds.is_empty() {
            do_cpio_batch(&magiskboot, workdir, ramdisk, &cpio_cmds, verbose)?;
        }

        if verbose {
            println!("- Repacking boot image");
        }
        let mut cmd = Command::new(&magiskboot);
        cmd.current_dir(workdir).arg("repack").arg(local_boot_name);
        if !verbose {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
        cmd.status()?;

        let new_boot = workdir.join("new-boot.img");

        #[cfg(target_os = "android")]
        if flash {
            if verbose {
                println!("- Flashing new boot image");
            }
            crate::android::flash_boot(&bootdevice, &new_boot)?;
            if ota {
                crate::android::post_ota()?;
            }
        }

        #[cfg(target_os = "android")]
        let should_write_output = patch_file || !flash || out_name.is_some() || out.is_some();
        #[cfg(not(target_os = "android"))]
        let should_write_output = patch_file;

        if should_write_output {
            let output_dir = out.unwrap_or(std::env::current_dir()?);
            let name = out_name.unwrap_or_else(|| {
                let now = chrono::Utc::now();
                format!("kernelsu_patched_{}.img", now.format("%Y%m%d_%H%M%S"))
            });
            let output_image = output_dir.join(name);
            if std::fs::rename(&new_boot, &output_image).is_err() {
                std::fs::copy(&new_boot, &output_image).context("copy out new boot failed")?;
            }
            if verbose {
                println!("- Output file is written to");
                println!("- {}", output_image.display().to_string().trim_matches('"'));
            }
        }
        if verbose {
            println!("- Done!");
        }
        Ok(())
    };

    let result = inner();
    if let Err(ref e) = result {
        eprintln!("- Patch Error: {e}");
    }
    result
}

#[derive(clap::Args, Debug)]
pub struct BootRestoreArgs {
    #[arg(short, long)]
    pub boot: Option<PathBuf>,
    #[cfg(target_os = "android")]
    #[arg(short, long, default_value = "false")]
    pub flash: bool,
    #[arg(long, default_value = None)]
    pub magiskboot: Option<PathBuf>,
    #[cfg(target_os = "android")]
    #[arg(short, long, default_value = None)]
    pub out: Option<PathBuf>,
    #[cfg(not(target_os = "android"))]
    #[arg(short, long, default_value = None)]
    pub out: Option<PathBuf>,
    #[cfg(target_os = "android")]
    #[arg(long, default_value = None)]
    pub out_name: Option<String>,
    #[cfg(not(target_os = "android"))]
    #[arg(long, default_value = None)]
    pub out_name: Option<String>,
    #[arg(short = 'v', long, default_value = "false")]
    pub verbose: bool,
}

pub fn restore(args: BootRestoreArgs) -> Result<()> {
    let BootRestoreArgs {
        boot: image,
        magiskboot: magiskboot_path,
        out_name,
        out,
        verbose,
        #[cfg(target_os = "android")]
        flash,
    } = args;

    let tmpdir = tempfile::Builder::new()
        .prefix("KernelSU")
        .tempdir()
        .context("create temp dir failed")?;
    let workdir = tmpdir.path();
    let magiskboot = find_magiskboot(magiskboot_path, workdir)?;

    #[cfg(target_os = "android")]
    let kmi = crate::android::get_current_kmi().unwrap_or_default();
    #[cfg(target_os = "android")]
    let (bootimage, _) = find_boot_image(&image, &kmi, false, false, workdir, &None, verbose)?;
    #[cfg(not(target_os = "android"))]
    let (bootimage, _) = find_boot_image(&image, "", false, false, workdir, &None, verbose)?;

    let local_boot_name = "boot_orig.img";
    let local_boot_img = workdir.join(local_boot_name);
    std::fs::copy(&bootimage, &local_boot_img).context("copy bootimage failed")?;

    if verbose {
        println!("- Unpacking boot image");
    }
    let mut cmd = Command::new(&magiskboot);
    cmd.current_dir(workdir).arg("unpack").arg(local_boot_name);
    if !verbose {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
    }
    cmd.status()?;

    let mut ramdisk = workdir.join("ramdisk.cpio");
    if !ramdisk.exists() {
        ramdisk = workdir.join("vendor_ramdisk").join("init_boot.cpio");
    }
    if !ramdisk.exists() {
        ramdisk = workdir.join("vendor_ramdisk").join("ramdisk.cpio");
    }
    if !ramdisk.exists() {
        bail!("No compatible ramdisk found.")
    }
    let ramdisk = ramdisk.as_path();

    ensure!(
        is_kernelsu_patched(&magiskboot, workdir, ramdisk)?,
        "boot image is not patched by KernelSU"
    );

    let remove_ksu = || -> Result<_> {
        let mut cpio_cmds = vec!["rm kernelsu.ko".to_string()];
        if do_cpio_cmd(&magiskboot, workdir, ramdisk, "exists init.real").is_ok() {
            cpio_cmds.push("mv init.real init".to_string());
        }

        if !cpio_cmds.is_empty() {
            do_cpio_batch(&magiskboot, workdir, ramdisk, &cpio_cmds, verbose)?;
        }

        if verbose {
            println!("- Repacking boot image");
        }
        let mut cmd = Command::new(&magiskboot);
        cmd.current_dir(workdir).arg("repack").arg(local_boot_name);
        if !verbose {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
        cmd.status()?;
        Ok(workdir.join("new-boot.img"))
    };

    let new_boot = remove_ksu()?;

    #[cfg(target_os = "android")]
    let should_write_output = image.is_some() || !flash || out_name.is_some() || out.is_some();
    #[cfg(not(target_os = "android"))]
    let should_write_output = image.is_some();

    if should_write_output {
        let output_dir = out.unwrap_or(std::env::current_dir()?);
        let name = out_name.unwrap_or_else(|| {
            let now = chrono::Utc::now();
            format!("kernelsu_restore_{}.img", now.format("%Y%m%d_%H%M%S"))
        });
        let output_image = output_dir.join(name);
        if std::fs::rename(&new_boot, &output_image).is_err() {
            std::fs::copy(&new_boot, &output_image).context("copy out new boot failed")?;
        }
        if verbose {
            println!(
                "- Output file is written to\n- {}",
                output_image.display().to_string().trim_matches('"')
            );
        }
    }
    if verbose {
        println!("- Done!");
    }
    Ok(())
}

#[derive(clap::Args, Debug)]
pub struct GetKmiArgs {
    #[arg(short, long)]
    pub boot: PathBuf,
    #[arg(long, default_value = None)]
    pub magiskboot: Option<PathBuf>,
    #[arg(short = 'v', long, default_value = "false")]
    pub verbose: bool,
}

pub fn get_kmi(args: GetKmiArgs) -> Result<()> {
    let GetKmiArgs {
        boot: image,
        magiskboot: magiskboot_path,
        verbose,
    } = args;
    ensure!(image.exists(), "找不到指定的 boot 镜像");

    let tmpdir = tempfile::Builder::new()
        .prefix("KernelSU_KMI")
        .tempdir()
        .context("创建临时目录失败")?;
    let workdir = tmpdir.path();
    let magiskboot = find_magiskboot(magiskboot_path, workdir)?;

    if verbose {
        println!("正在解包 KMI 版本，请稍候...");
    }

    let kmi = parse_kmi_from_boot(&magiskboot, &image, workdir, verbose)?;
    println!("{kmi}");
    Ok(())
}
