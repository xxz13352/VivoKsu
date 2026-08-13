use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "fastboot")]
#[command(author = "Fastboot Rust Port")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Android Fastboot/ADB tool rewritten in Rust", long_about = None)]
pub struct Cli {
    #[arg(short = 's', long, global = true)]
    pub serial: Option<String>,

    #[arg(long, global = true)]
    pub slot: Option<String>,

    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    Devices,

    Getvar {
        variable: String,
    },

    Flash {
        partition: String,
        filename: PathBuf,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra_args: Vec<String>,
    },

    Erase {
        partition: String,
    },

    #[command(visible_alias = "r")]
    Reboot {
        #[arg(default_value = "")]
        target: String,
    },

    // 连字符形式的 reboot 便捷命令：标准 fastboot 与多数 GUI/脚本都直接调
    // `fastboot reboot-bootloader` 等单命令，缺失会报 unrecognized subcommand。
    #[command(name = "reboot-bootloader")]
    RebootBootloader,

    #[command(name = "reboot-fastboot")]
    RebootFastboot,

    #[command(name = "reboot-recovery")]
    RebootRecovery,

    #[command(name = "reboot-edl")]
    RebootEdl,

    Flashall {
        #[arg(short, long)]
        wipe: bool,
    },

    Update {
        filename: PathBuf,
    },

    Diagnose,

    Upload {
        partition: String,
        filename: PathBuf,
    },

    #[command(visible_alias = "sh")]
    Shell {
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
    },

    Push {
        local: PathBuf,
        remote: String,
    },

    Pull {
        remote: String,
        local: PathBuf,
    },

    #[command(visible_alias = "i")]
    Install {
        apk: PathBuf,
        #[arg(short, long)]
        replace: bool,
    },

    Uninstall {
        package: String,
    },

    #[command(visible_alias = "pm")]
    Packages {
        #[arg(short = '3', long)]
        third_party: bool,
        #[arg(short = 's', long)]
        system: bool,
    },

    Logcat {
        #[arg(trailing_var_arg = true)]
        filter: Vec<String>,
    },

    Screencap {
        #[arg(default_value = "screenshot.png")]
        output: PathBuf,
    },

    Screenrecord {
        #[arg(default_value = "recording.mp4")]
        output: PathBuf,
        #[arg(short, long, default_value = "180")]
        time: u32,
    },

    #[command(name = "set_active", visible_alias = "set-active")]
    SetActive {
        slot: String,
    },

    Oem {
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },

    Flashing {
        operation: String,
    },

    Format {
        partition: String,
        #[arg(short = 't', long)]
        fs_type: Option<String>,
        #[arg(short = 's', long)]
        size: Option<String>,
    },

    Boot {
        kernel: PathBuf,
        ramdisk: Option<PathBuf>,
    },

    Fetch {
        partition: String,
        output: PathBuf,
    },

    Root,

    Unroot,

    #[command(name = "create-logical-partition")]
    CreateLogicalPartition {
        name: String,
        size: u64,
    },

    #[command(name = "delete-logical-partition")]
    DeleteLogicalPartition {
        name: String,
    },

    #[command(name = "resize-logical-partition")]
    ResizeLogicalPartition {
        name: String,
        size: u64,
    },

    #[command(name = "snapshot-update")]
    SnapshotUpdate {
        operation: String,
    },

    Gsi {
        operation: String,
    },

    #[command(name = "wipe-super")]
    WipeSuper {
        super_empty: Option<PathBuf>,
    },

    Stage {
        input: PathBuf,
    },

    #[command(name = "get_staged")]
    GetStaged {
        output: PathBuf,
    },

    #[command(name = "截图")]
    Jietu,

    #[command(name = "音量加")]
    VolUp,

    #[command(name = "音量减")]
    VolDown,

    #[command(name = "锁屏")]
    LockScreen,

    Custom {
        cmd: String,
    },
}
