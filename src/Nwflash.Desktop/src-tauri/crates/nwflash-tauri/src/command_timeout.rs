//! 外部工具（fastboot / adb / scrcpy / payload_dumper）命令的墙钟超时分级。
//!
//! 对齐 C# `FastbootCliRunner` / `PlatformToolsNativeApi` 的分级策略：探测与
//! 短命令用严格超时，刷写/回读用长时间上限兜底。**任何命令都不允许无限挂起**
//! ——USB 半断开或工具卡死时，未设超时的命令会永久占用 operation gate，
//! UI 会一直停留在“执行中”且后续操作无法进入。

use std::time::Duration;

use nwflash_windows::process::ProcessCommand;

/// `fastboot devices` / `adb devices` 一类探测命令（含驱动状态查询）。
pub const PROBE: Duration = Duration::from_secs(15);
/// `getvar` 变量读取与分区路径解析。
pub const GETVAR: Duration = Duration::from_secs(20);
/// `erase`、`reboot`、`set_active`、`fastboot` 等控制/短命令。
pub const CONTROL: Duration = Duration::from_secs(60);
/// ROOT 修补链的 adb 传输/设备端修补命令（`push`/`pull`/`shell`/`install`）：
/// 大镜像走慢速 USB、设备端 `ksud boot-patch`/`magiskboot` 常超 60s，
/// CONTROL 档会在中途强杀 adb 进程树留下截断的远端暂存文件。
pub const ROOT_PATCH: Duration = Duration::from_secs(5 * 60);
/// 分区刷写与 `dd` 回读：只是兜底上限（正常远小于此），防止半断开永久挂起。
pub const FLASH: Duration = Duration::from_secs(30 * 60);
/// 文件传输类：比控制命令宽松，但仍必须是有界的（配合用户取消）。
pub const TRANSFER: Duration = Duration::from_secs(30 * 60);

/// 按命令参数形态挑选超时；无法归类时使用 `fallback`。
///
/// 分类依据 program + 参数形态，而不是裸参数完全相等：ADB Root 通道的
/// `dd`/`blkdiscard` 埋在 `shell -T su -c '<引号串>'` 里，裸参数比较永远
/// 命不中（审计 A1）。这些命令按字节量工作（GB 级分区经慢速 USB），
/// 误落 CONTROL 60s 会在中途强杀进程树——设备端 `dd` 写 super 分区被
/// host 侧超时终止时留下半旧半新的分区，**不可开机**。
pub fn for_command(command: &ProcessCommand, fallback: Duration) -> Duration {
    let args = &command.args;
    if args.iter().any(|argument| argument == "devices") {
        return PROBE;
    }
    if args.iter().any(|argument| argument == "getvar") {
        return GETVAR;
    }
    if args.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "erase" | "reboot" | "set_active" | "fastboot"
        )
    }) {
        return CONTROL;
    }
    if args
        .iter()
        .any(|argument| matches!(argument.as_str(), "flash" | "dd"))
    {
        return FLASH;
    }
    // 暂存上传（`adb push`）：大镜像传输，与 flash 同档兜底（C#
    // AdbRootTransferRunner 的 push 无墙钟超时，仅有界取消兜底）。
    if args.iter().any(|argument| argument == "push") {
        return TRANSFER;
    }
    // ADB Root 设备端写入（`adb shell -T su -c 'dd if=… of=…'` /
    // `blkdiscard`）：命令名埋在引号串内，按「shell+su 组合且串内包含
    // 设备端写入命令」识别。生产形态经 shell_quote 后以 `dd ` 开头
    // （`dd if='…' of='…' bs=4M conv=fsync`）；嵌入形式（`sh -c
    // 'blkdiscard …'`、`cmd && dd if=…`）以词边界 contains 兜底；不带
    // 设备端写入的短 root shell 命令不命中。
    if args.iter().any(|argument| argument == "shell")
        && args.iter().any(|argument| argument == "su")
        && args.iter().any(|argument| {
            let body = argument
                .trim()
                .trim_matches(|character| character == '\'' || character == '"');
            body.starts_with("dd ")
                || body.starts_with("blkdiscard")
                || body.contains(" dd if=")
                || body.contains(" dd of=")
                || body.contains(" blkdiscard ")
                || body.contains("'blkdiscard")
                || body.contains("\"blkdiscard")
        })
    {
        return FLASH;
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(args: &[&str]) -> ProcessCommand {
        ProcessCommand::new("fastboot.exe", args.iter().map(|value| value.to_string()))
    }

    #[test]
    fn device_probes_get_the_strict_short_budget() {
        assert_eq!(for_command(&command(&["devices"]), CONTROL), PROBE);
    }

    #[test]
    fn getvar_reads_get_their_own_budget() {
        assert_eq!(
            for_command(&command(&["-s", "SERIAL", "getvar", "is-userspace"]), FLASH),
            GETVAR
        );
    }

    #[test]
    fn control_commands_get_the_control_budget() {
        for args in [
            ["-s", "SERIAL", "reboot"].as_slice(),
            ["-s", "SERIAL", "erase", "misc"].as_slice(),
            ["-s", "SERIAL", "set_active", "b"].as_slice(),
        ] {
            assert_eq!(for_command(&command(args), PROBE), CONTROL);
        }
    }

    #[test]
    fn flash_and_backup_get_the_long_safety_net() {
        assert_eq!(
            for_command(
                &command(&["-s", "SERIAL", "flash", "boot", "boot.img"]),
                CONTROL
            ),
            FLASH
        );
    }

    #[test]
    fn adb_push_staging_uploads_get_the_transfer_budget() {
        assert_eq!(
            for_command(
                &command(&["-s", "SERIAL", "push", "boot.img", "/data/local/tmp/nwflash/"]),
                CONTROL
            ),
            TRANSFER
        );
    }

    #[test]
    fn adb_root_dd_inside_quoted_su_script_gets_the_flash_budget() {
        // 对应 build_adb_root_copy_staged_file_to_device_command 的真实形态：
        // ["shell", "-T", "su", "-c", "dd if=… of=… bs=4M conv=fsync"]。
        // 裸参数比较永远命不中；被 CONTROL 60s 中途强杀会留下半写分区。
        for quoted in [
            "dd if=/data/local/tmp/x of=/dev/block/foo bs=4M conv=fsync",
            "'dd if=/data/local/tmp/x of=/dev/block/foo bs=4M'",
            "\"dd if=/data/local/tmp/x of=/dev/block/foo\"",
            "sh -c 'blkdiscard /dev/block/foo'",
        ] {
            assert_eq!(
                for_command(
                    &command(&["-s", "SERIAL", "shell", "-T", "su", "-c", quoted]),
                    CONTROL
                ),
                FLASH,
                "quoted script: {quoted}"
            );
        }
    }

    #[test]
    fn adb_root_shell_without_device_write_keeps_the_fallback_budget() {
        // 短 root shell 命令（ls / stat）不应吃 30 分钟档。
        assert_eq!(
            for_command(
                &command(&["-s", "SERIAL", "shell", "-T", "su", "-c", "stat /dev/block/foo"]),
                CONTROL
            ),
            CONTROL
        );
    }

    #[test]
    fn unclassified_commands_fall_back_to_the_caller_budget() {
        assert_eq!(for_command(&command(&["--version"]), CONTROL), CONTROL);
    }
}
