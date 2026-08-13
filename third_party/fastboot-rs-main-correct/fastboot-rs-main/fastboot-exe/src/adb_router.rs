use crate::adb_winusb_transport::AdbWinUsbDevice;
use std::env;
use std::io::{self, Write};
use std::path::Path;
use std::process;

/// 判断本程序是否以 "adb" 身份被调用：只看可执行文件名(去扩展名)是否等于 adb。
/// 不能用「整条路径包含 adb」来判断——部署目录名为 AdbToolbox 等含 "adb" 字样时会误判，
/// 致使所有 fastboot 子命令落入兜底分支被当 adb 未知命令而静默 exit(0) 空跑。
pub fn invoked_as_adb() -> bool {
    env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::file_stem)
        .map(|s| s.to_string_lossy().to_lowercase() == "adb")
        .unwrap_or(false)
}

pub fn try_handle_adb_args() -> Option<i32> {
    let mut args: Vec<String> = env::args().skip(1).collect();

    while let Some(pos) = args.iter().position(|x| x == "-s") {
        if pos + 1 < args.len() {
            args.remove(pos + 1);
        }
        args.remove(pos);
    }

    args.retain(|x| x != "-d" && x != "-e");

    if args.is_empty() {
        if invoked_as_adb() {
            process::exit(0);
        }
        return None;
    }

    let cmd = args[0].as_str();

    match cmd {
        "wait-for-device" | "start-server" => {
            process::exit(0);
        }
        "version" => {
            println!("Android Debug Bridge version 1.0.41");
            println!("Version 34.0.5-10900879");
            process::exit(0);
        }
        "reboot" => {
            // adb/fastboot reboot 一律透传给 fastboot CLI 的 Reboot 分支：
            // 它会枚举 adb 设备(dev.reboot)与 fastboot 设备(cmd_reboot)，把设备送进
            // bootloader/fastbootd/recovery/system，避免旧逻辑落兜底 exit(0) 空跑。
            None
        }
        "devices" | "devices-l" => {
            if invoked_as_adb() {
                handle_devices_native()
            } else {
                // 以 fastboot 身份调用：交给 fastboot CLI 的 Devices 列 fastboot 设备。
                None
            }
        }
        "reverse" => handle_reverse(&args[1..]),
        "forward" => handle_forward(&args[1..]),
        "push" => {
            if args.len() >= 3 {
                let local = &args[1];
                let remote = &args[2];
                handle_push_native(local, remote)
            } else {
                None
            }
        }
        "shell" => {
            let shell_args = &args[1..];
            handle_shell(shell_args)
        }
        _ => {
            if invoked_as_adb() {
                process::exit(0);
            }
            None
        }
    }
}

fn handle_devices_native() -> Option<i32> {
    match AdbWinUsbDevice::enumerate() {
        Ok(devices) => {
            if devices.is_empty() {
                println!("List of devices attached");
                return Some(0);
            }

            println!("List of devices attached");

            for info in devices {
                let device_path = info.device_path.clone();
                match AdbWinUsbDevice::open_device(&device_path) {
                    Ok(mut dev) => match dev.connect() {
                        Ok(()) => {
                            let serial = extract_serial_from_path(&device_path);
                            println!("{}\tdevice", serial);
                        }
                        Err(_) => {
                            let serial = extract_serial_from_path(&device_path);
                            println!("{}\tunauthorized", serial);
                        }
                    },
                    Err(_) => {
                        let serial = extract_serial_from_path(&device_path);
                        println!("{}\tunauthorized", serial);
                    }
                }
            }
            Some(0)
        }
        Err(_) => {
            println!("List of devices attached");
            Some(0)
        }
    }
}

fn extract_serial_from_path(path: &str) -> String {
    // 设备路径形如: \\?\usb#vid_18d1&pid_4ee7#a7ab3ab5#{guid}
    // 序列号是按 '#' 分割后的第 3 段(index 2)。
    // 旧实现取 index 1(vid_xxxx&pid_xxxx)再 split('&'),错误地显示成 vid_18d1。
    let parts: Vec<&str> = path.split('#').collect();
    if parts.len() >= 3 {
        let serial = parts[2];
        if !serial.is_empty()
            && !serial.starts_with('{')
            && !serial.contains('&')
            && serial.len() < 64
        {
            return serial.to_string();
        }
    }
    "unknown".to_string()
}

fn handle_reverse(args: &[String]) -> Option<i32> {
    if args.len() < 2 {
        return Some(1);
    }
    let remote = &args[0];
    let local = &args[1];
    let cmd = format!("host:reverse:forward:{};{}", remote, local);

    match crate::adb_handler::send_host_command(&cmd) {
        Ok(_) => {
            println!("{}", local);
            Some(0)
        }
        Err(_) => Some(1),
    }
}

fn handle_forward(args: &[String]) -> Option<i32> {
    if args.len() < 2 {
        return Some(1);
    }
    let local = &args[0];
    let remote = &args[1];
    let cmd = format!("host:forward:tcp:{};{}", local, remote);

    match crate::adb_handler::send_host_command(&cmd) {
        Ok(_) => {
            println!("{}", local);
            Some(0)
        }
        Err(_) => Some(1),
    }
}

fn handle_push_native(local: &str, remote: &str) -> Option<i32> {
    let local_path = Path::new(local);
    if !local_path.exists() {
        eprintln!("本地文件不存在: {}", local);
        return Some(1);
    }

    // 统一走 WinUSB 直连 + 完善的 dev.push()(与 main.rs 的 Commands::Push 一致)。
    // 旧实现是手写的简化 sync 协议(write_stream 逐包硬等 A_OKAY、未跳过 arg1 不匹配的
    // 幽灵包、未校验最终 SYNC ack),真机 push 稳定失败 exit 1。
    let devices = match AdbWinUsbDevice::enumerate() {
        Ok(d) => d,
        Err(_) => {
            eprintln!("枚举 ADB 设备失败");
            return Some(1);
        }
    };
    if devices.is_empty() {
        eprintln!("未检测到处于 ADB 模式的设备");
        return Some(1);
    }

    let mut dev = match AdbWinUsbDevice::open_device(&devices[0].device_path) {
        Ok(d) => d,
        Err(_) => {
            eprintln!("无法打开设备句柄");
            return Some(1);
        }
    };

    match dev.push(local, remote) {
        Ok(_) => {
            println!("{}: 1 file pushed", remote);
            Some(0)
        }
        Err(e) => {
            eprintln!("Push 失败: {}", e);
            Some(1)
        }
    }
}

fn handle_shell(args: &[String]) -> Option<i32> {
    // 统一走 WinUSB 直连(与 main.rs 的 Commands::Shell 一致)。
    // 旧实现依赖本地 127.0.0.1:5037 的官方 ADB server,但本程序是 WinUSB 直连、
    // 从不启动 ADB server,导致连接被拒 -> shell 返回空且 exit 1。
    let devices = match AdbWinUsbDevice::enumerate() {
        Ok(d) => d,
        Err(_) => {
            eprintln!("枚举 ADB 设备失败");
            return Some(1);
        }
    };
    if devices.is_empty() {
        eprintln!("未检测到处于 ADB 模式的设备");
        return Some(1);
    }

    let mut dev = match AdbWinUsbDevice::open_device(&devices[0].device_path) {
        Ok(d) => d,
        Err(_) => {
            eprintln!("无法打开设备句柄");
            return Some(1);
        }
    };

    if args.is_empty() {
        // 无参数 = 交互式终端
        if let Err(e) = dev.true_pty_shell() {
            eprintln!("终端异常 {}", e);
            return Some(1);
        }
        Some(0)
    } else {
        // 单条命令: 一次性执行并输出
        let cmd_string = args.join(" ");
        match dev.shell_command(&cmd_string) {
            Ok(output) => {
                print!("{}", output);
                let _ = io::stdout().flush();
                Some(0)
            }
            Err(e) => {
                eprintln!("执行失败 {}", e);
                Some(1)
            }
        }
    }
}
