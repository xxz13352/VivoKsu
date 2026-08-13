use anyhow::Result;
use clap::Parser;

use crate::boot_patch::{BootPatchArgs, BootRestoreArgs, GetKmiArgs};
use crate::defs;

/// KernelSU cli for non-android
#[derive(Parser, Debug)]
#[command(author, version = defs::VERSION_NAME, about = "KernelSU PATCH \n 编译者：雨纷飞 ", long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    #[cfg(windows)]
    /// Launch the native graphical interface
    Gui,

    /// Patch boot or init_boot images to apply KernelSU
    BootPatch(BootPatchArgs),

    /// Restore boot or init_boot images patched by KernelSU
    BootRestore(BootRestoreArgs),
    // /// Get apk size and hash
    // GetSign {
    //     /// apk path
    //     apk: String,
    // },
    GetKmi(GetKmiArgs),
    // /// show supported kmi versions
    // SupportedKmis,
}

pub fn run() -> Result<()> {
    env_logger::init();

    let cli = Args::parse();

    log::info!("command: {:?}", cli.command);

    let result = match cli.command {
        #[cfg(windows)]
        Commands::Gui => windows_gui::run(),

        Commands::BootPatch(boot_patch) => crate::boot_patch::patch(boot_patch),

        Commands::BootRestore(boot_restore) => crate::boot_patch::restore(boot_restore),
        // // Commands::GetSign { apk } => { ... }
        // // Commands::SupportedKmis => { ... }
        Commands::GetKmi(get_kmi_args) => crate::boot_patch::get_kmi(get_kmi_args),
    };

    if let Err(e) = &result {
        log::error!("Error: {e:?}");
    }
    result
}

#[cfg(windows)]
pub fn run_gui() -> Result<()> {
    windows_gui::run()
}

#[cfg(windows)]
#[allow(unsafe_op_in_unsafe_fn)]
mod windows_gui {
    use crate::boot_patch::{BootPatchArgs, KernelSuSource};
    use anyhow::{Context, Result, ensure};
    use std::ffi::{OsStr, OsString, c_void};
    use std::mem::size_of;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use std::ptr::{null, null_mut};
    use std::thread;
    use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{
        CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET,
        DEFAULT_PITCH, DeleteObject, FF_DONTCARE, FW_NORMAL, HBRUSH, HFONT, OUT_DEFAULT_PRECIS,
        SetBkMode, SetTextColor, TRANSPARENT,
    };
    use windows_sys::Win32::System::Console::GetConsoleWindow;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, GetSaveFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
        OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    use windows_sys::Win32::UI::Controls::SetWindowTheme;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BS_DEFPUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL, CB_GETLBTEXT, CB_GETLBTEXTLEN,
        CB_RESETCONTENT, CB_SETCURSEL, CBN_SELCHANGE, CBS_DROPDOWNLIST, CREATESTRUCTW, CS_HREDRAW,
        CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow,
        DispatchMessageW, ES_AUTOHSCROLL, GWLP_USERDATA, GetMessageW, GetSystemMetrics,
        GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, IDC_ARROW, IDI_APPLICATION,
        LoadCursorW, LoadIconW, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MSG, MessageBoxW,
        PostMessageW, PostQuitMessage, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW,
        SendMessageW, SetWindowLongPtrW, SetWindowTextW, ShowWindow, TranslateMessage, WM_APP,
        WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORSTATIC, WM_DESTROY, WM_NCCREATE, WM_NCDESTROY,
        WM_SETFONT, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE,
        WS_EX_CONTROLPARENT, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
        WS_VSCROLL,
    };
    use windows_sys::core::w;

    const WINDOW_WIDTH: i32 = 720;
    const WINDOW_HEIGHT: i32 = 550;
    const MAX_TEXT_LENGTH: i32 = 32_767;
    const ID_BOOT: i32 = 101;
    const ID_BOOT_BROWSE: i32 = 102;
    const ID_OUTPUT: i32 = 103;
    const ID_OUTPUT_BROWSE: i32 = 104;
    const ID_SOURCE: i32 = 105;
    const ID_KMI: i32 = 106;
    const ID_PATCH: i32 = 107;
    const ID_STATUS: i32 = 108;
    const MSG_PATCH_OK: u32 = WM_APP + 1;
    const MSG_PATCH_ERROR: u32 = WM_APP + 2;
    const MSG_KMIS_OK: u32 = WM_APP + 3;
    const MSG_KMIS_ERROR: u32 = WM_APP + 4;
    const MINT_BACKGROUND: u32 = 0x00f8fbf6;
    const DARK_TEXT: u32 = 0x00453f25;

    struct GuiState {
        boot: HWND,
        output: HWND,
        source: HWND,
        kmi: HWND,
        patch: HWND,
        status: HWND,
        background: HBRUSH,
        body_font: HFONT,
        title_font: HFONT,
        busy: bool,
    }

    struct KmiResult {
        source_index: isize,
        values: Vec<String>,
        error: Option<String>,
    }

    impl GuiState {
        fn new(background: HBRUSH) -> Self {
            Self {
                boot: null_mut(),
                output: null_mut(),
                source: null_mut(),
                kmi: null_mut(),
                patch: null_mut(),
                status: null_mut(),
                background,
                body_font: null_mut(),
                title_font: null_mut(),
                busy: false,
            }
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(Some(0)).collect()
    }

    fn wide_os(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(Some(0)).collect()
    }

    fn menu_id(id: i32) -> *mut c_void {
        id as usize as *mut c_void
    }

    unsafe fn state(hwnd: HWND) -> Option<&'static mut GuiState> {
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState;
        pointer.as_mut()
    }

    unsafe fn set_text(hwnd: HWND, value: &str) {
        let value = wide(value);
        SetWindowTextW(hwnd, value.as_ptr());
    }

    unsafe fn set_path(hwnd: HWND, value: &Path) {
        let value = wide_os(value.as_os_str());
        SetWindowTextW(hwnd, value.as_ptr());
    }

    unsafe fn read_os_text(hwnd: HWND) -> Result<OsString> {
        let length = GetWindowTextLengthW(hwnd);
        ensure!((0..=MAX_TEXT_LENGTH).contains(&length), "输入内容长度异常");
        let mut buffer = vec![0u16; length as usize + 1];
        let actual = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
        ensure!(actual >= 0, "读取输入内容失败");
        buffer.truncate(actual as usize);
        Ok(OsString::from_wide(&buffer))
    }

    unsafe fn create_control(
        parent: HWND,
        class_name: *const u16,
        text: &str,
        style: u32,
        ex_style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        id: i32,
        instance: HINSTANCE,
        font: HFONT,
    ) -> Result<HWND> {
        let text = wide(text);
        let control = CreateWindowExW(
            ex_style,
            class_name,
            text.as_ptr(),
            style,
            x,
            y,
            width,
            height,
            parent,
            menu_id(id),
            instance,
            null(),
        );
        ensure!(!control.is_null(), "创建 GUI 控件失败: {id}");
        if !font.is_null() {
            SendMessageW(control, WM_SETFONT, font as usize, 1);
        }
        let explorer = wide("Explorer");
        SetWindowTheme(control, explorer.as_ptr(), null());
        Ok(control)
    }

    unsafe fn create_label(
        parent: HWND,
        text: &str,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        instance: HINSTANCE,
        font: HFONT,
    ) -> Result<HWND> {
        create_control(
            parent,
            w!("STATIC"),
            text,
            WS_CHILD | WS_VISIBLE,
            0,
            x,
            y,
            width,
            height,
            0,
            instance,
            font,
        )
    }

    unsafe fn create_interface(hwnd: HWND, gui: &mut GuiState) -> Result<()> {
        let instance = GetModuleHandleW(null());
        ensure!(!instance.is_null(), "无法获取程序实例");
        let face = wide("Segoe UI");
        gui.body_font = CreateFontW(
            -18,
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.into(),
            OUT_DEFAULT_PRECIS.into(),
            CLIP_DEFAULT_PRECIS.into(),
            CLEARTYPE_QUALITY.into(),
            (DEFAULT_PITCH | FF_DONTCARE).into(),
            face.as_ptr(),
        );
        gui.title_font = CreateFontW(
            -29,
            0,
            0,
            0,
            600,
            0,
            0,
            0,
            DEFAULT_CHARSET.into(),
            OUT_DEFAULT_PRECIS.into(),
            CLIP_DEFAULT_PRECIS.into(),
            CLEARTYPE_QUALITY.into(),
            (DEFAULT_PITCH | FF_DONTCARE).into(),
            face.as_ptr(),
        );
        ensure!(
            !gui.body_font.is_null() && !gui.title_font.is_null(),
            "创建字体失败"
        );

        create_label(
            hwnd,
            "KernelSU Boot Patcher",
            42,
            28,
            620,
            38,
            instance,
            gui.title_font,
        )?;
        create_label(
            hwnd,
            "本地制作并安全修补启动镜像",
            44,
            69,
            620,
            24,
            instance,
            gui.body_font,
        )?;
        create_label(hwnd, "BOOT 镜像", 42, 112, 180, 24, instance, gui.body_font)?;
        gui.boot = create_control(
            hwnd,
            w!("EDIT"),
            "",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            WS_EX_CLIENTEDGE,
            42,
            139,
            536,
            32,
            ID_BOOT,
            instance,
            gui.body_font,
        )?;
        create_control(
            hwnd,
            w!("BUTTON"),
            "浏览",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            0,
            590,
            138,
            88,
            34,
            ID_BOOT_BROWSE,
            instance,
            gui.body_font,
        )?;

        create_label(hwnd, "输出文件", 42, 188, 180, 24, instance, gui.body_font)?;
        gui.output = create_control(
            hwnd,
            w!("EDIT"),
            "",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            WS_EX_CLIENTEDGE,
            42,
            215,
            536,
            32,
            ID_OUTPUT,
            instance,
            gui.body_font,
        )?;
        create_control(
            hwnd,
            w!("BUTTON"),
            "选择",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            0,
            590,
            214,
            88,
            34,
            ID_OUTPUT_BROWSE,
            instance,
            gui.body_font,
        )?;

        create_label(
            hwnd,
            "KernelSU 版本",
            42,
            265,
            260,
            24,
            instance,
            gui.body_font,
        )?;
        gui.source = create_control(
            hwnd,
            w!("COMBOBOX"),
            "",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
            0,
            42,
            292,
            304,
            180,
            ID_SOURCE,
            instance,
            gui.body_font,
        )?;
        for source in ["KernelSU 原版", "SukiSU Ultra", "KernelSU Next", "Wild KSU"] {
            let source = wide(source);
            SendMessageW(gui.source, CB_ADDSTRING, 0, source.as_ptr() as isize);
        }
        SendMessageW(gui.source, CB_SETCURSEL, 0, 0);

        create_label(hwnd, "KMI", 370, 265, 260, 24, instance, gui.body_font)?;
        gui.kmi = create_control(
            hwnd,
            w!("COMBOBOX"),
            "",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
            0,
            370,
            292,
            308,
            180,
            ID_KMI,
            instance,
            gui.body_font,
        )?;

        gui.patch = create_control(
            hwnd,
            w!("BUTTON"),
            "开始修补",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
            0,
            42,
            355,
            636,
            44,
            ID_PATCH,
            instance,
            gui.body_font,
        )?;
        gui.status = create_control(
            hwnd,
            w!("STATIC"),
            "准备就绪 · 自动选择官方 CDN 与免费加速线路",
            WS_CHILD | WS_VISIBLE,
            0,
            42,
            419,
            636,
            48,
            ID_STATUS,
            instance,
            gui.body_font,
        )?;
        create_label(
            hwnd,
            "不内嵌 KO  ·  动态最新版本  ·  SHA-256 完整性校验",
            42,
            479,
            636,
            24,
            instance,
            gui.body_font,
        )?;
        load_kmis(hwnd, gui)?;
        Ok(())
    }

    unsafe fn file_dialog(owner: HWND, save: bool, initial: Option<&Path>) -> Option<PathBuf> {
        let mut buffer = vec![0u16; MAX_TEXT_LENGTH as usize + 1];
        if let Some(initial) = initial {
            for (destination, source) in buffer
                .iter_mut()
                .zip(initial.as_os_str().encode_wide())
                .take(MAX_TEXT_LENGTH as usize)
            {
                *destination = source;
            }
        }
        let filter = wide("启动镜像 (*.img;*.bin)\0*.img;*.bin\0所有文件 (*.*)\0*.*\0");
        let title = wide(if save {
            "保存修补后的镜像"
        } else {
            "选择 BOOT 镜像"
        });
        let extension = wide("img");
        let mut dialog = OPENFILENAMEW {
            lStructSize: size_of::<OPENFILENAMEW>() as u32,
            hwndOwner: owner,
            lpstrFilter: filter.as_ptr(),
            lpstrFile: buffer.as_mut_ptr(),
            nMaxFile: buffer.len() as u32,
            lpstrTitle: title.as_ptr(),
            lpstrDefExt: extension.as_ptr(),
            Flags: OFN_EXPLORER
                | OFN_HIDEREADONLY
                | OFN_NOCHANGEDIR
                | OFN_PATHMUSTEXIST
                | if save {
                    OFN_OVERWRITEPROMPT
                } else {
                    OFN_FILEMUSTEXIST
                },
            ..Default::default()
        };
        let selected = if save {
            GetSaveFileNameW(&mut dialog)
        } else {
            GetOpenFileNameW(&mut dialog)
        };
        if selected == 0 {
            return None;
        }
        let length = buffer.iter().position(|value| *value == 0)?;
        Some(PathBuf::from(OsString::from_wide(&buffer[..length])))
    }

    fn default_output(boot: &Path) -> PathBuf {
        let stem = boot
            .file_stem()
            .and_then(OsStr::to_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("boot");
        boot.with_file_name(format!("{stem}_patched.img"))
    }

    unsafe fn source_index(combo: HWND) -> Result<isize> {
        let index = SendMessageW(combo, CB_GETCURSEL, 0, 0);
        ensure!((0..=3).contains(&index), "请选择 KernelSU 版本");
        Ok(index)
    }

    fn distribution_for_index(index: isize) -> Result<crate::assets::KernelSuDistribution> {
        use crate::assets::KernelSuDistribution;

        match index {
            0 => Ok(KernelSuDistribution::Official),
            1 => Ok(KernelSuDistribution::SukiSuUltra),
            2 => Ok(KernelSuDistribution::KernelSuNext),
            3 => Ok(KernelSuDistribution::WildKsu),
            _ => anyhow::bail!("KernelSU 版本索引非法"),
        }
    }

    unsafe fn selected_source(combo: HWND) -> Result<KernelSuSource> {
        match source_index(combo)? {
            0 => Ok(KernelSuSource::Official),
            1 => Ok(KernelSuSource::SukiSuUltra),
            2 => Ok(KernelSuSource::KernelSuNext),
            3 => Ok(KernelSuSource::WildKsu),
            _ => unreachable!(),
        }
    }

    unsafe fn add_combo_item(combo: HWND, value: &str) {
        let value = wide(value);
        SendMessageW(combo, CB_ADDSTRING, 0, value.as_ptr() as isize);
    }

    unsafe fn selected_kmi(combo: HWND) -> Result<String> {
        let index = SendMessageW(combo, CB_GETCURSEL, 0, 0);
        ensure!(index > 0, "请选择设备对应的 KMI");
        let length = SendMessageW(combo, CB_GETLBTEXTLEN, index as usize, 0);
        ensure!((1..=32).contains(&length), "KMI 长度异常");
        let mut buffer = vec![0u16; length as usize + 1];
        let actual = SendMessageW(
            combo,
            CB_GETLBTEXT,
            index as usize,
            buffer.as_mut_ptr() as isize,
        );
        ensure!(actual == length, "读取 KMI 失败");
        buffer.truncate(actual as usize);
        String::from_utf16(&buffer).context("KMI 包含无效 Unicode")
    }

    unsafe fn post_kmis(hwnd: usize, source_index: isize, result: Result<Vec<String>>) {
        let (message, values, error) = match result {
            Ok(values) => (MSG_KMIS_OK, values, None),
            Err(error) => (MSG_KMIS_ERROR, Vec::new(), Some(format!("{error:#}"))),
        };
        let pointer = Box::into_raw(Box::new(KmiResult {
            source_index,
            values,
            error,
        }));
        if PostMessageW(hwnd as HWND, message, 0, pointer as isize) == 0 {
            drop(Box::from_raw(pointer));
        }
    }

    unsafe fn load_kmis(hwnd: HWND, gui: &mut GuiState) -> Result<()> {
        let index = source_index(gui.source)?;
        let distribution = distribution_for_index(index)?;
        SendMessageW(gui.kmi, CB_RESETCONTENT, 0, 0);
        add_combo_item(gui.kmi, "本地制作中…");
        SendMessageW(gui.kmi, CB_SETCURSEL, 0, 0);
        EnableWindow(gui.kmi, 0);
        EnableWindow(gui.patch, 0);
        set_text(gui.status, "本地制作中，请稍候…");

        let window = hwnd as usize;
        thread::spawn(move || {
            let result = crate::assets::fetch_distribution_kmis(distribution);
            unsafe {
                post_kmis(window, index, result);
            }
        });
        Ok(())
    }

    unsafe fn post_result(hwnd: usize, message: u32, text: String) {
        let pointer = Box::into_raw(Box::new(text));
        if PostMessageW(hwnd as HWND, message, 0, pointer as isize) == 0 {
            drop(Box::from_raw(pointer));
        }
    }

    unsafe fn start_patch(hwnd: HWND, gui: &mut GuiState) -> Result<()> {
        ensure!(!gui.busy, "已有修补任务正在运行");
        let boot = PathBuf::from(read_os_text(gui.boot)?);
        let output = PathBuf::from(read_os_text(gui.output)?);
        ensure!(!boot.as_os_str().is_empty(), "请选择 BOOT 镜像");
        ensure!(boot.is_file(), "BOOT 镜像不存在");
        ensure!(!output.as_os_str().is_empty(), "请选择输出文件");
        ensure!(boot != output, "输出文件不能覆盖原始 BOOT 镜像");

        let output_name = output
            .file_name()
            .and_then(OsStr::to_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("输出文件名无效"))?
            .to_string();
        let output_dir = output
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or(std::env::current_dir().context("读取当前目录失败")?);
        let kmi = Some(selected_kmi(gui.kmi)?);
        let source = selected_source(gui.source)?;

        gui.busy = true;
        EnableWindow(gui.patch, 0);
        EnableWindow(gui.source, 0);
        EnableWindow(gui.kmi, 0);
        set_text(gui.patch, "正在修补…");
        set_text(gui.status, "本地制作中，请勿关闭窗口");

        let window = hwnd as usize;
        thread::spawn(move || {
            let arguments =
                BootPatchArgs::for_remote_patch(boot, output_dir, output_name, kmi, source);
            match crate::boot_patch::patch(arguments) {
                Ok(()) => unsafe {
                    post_result(
                        window,
                        MSG_PATCH_OK,
                        format!("修补完成 · {}", output.display()),
                    );
                },
                Err(error) => unsafe {
                    post_result(window, MSG_PATCH_ERROR, format!("{error:#}"));
                },
            }
        });
        Ok(())
    }

    unsafe fn show_message(hwnd: HWND, title: &str, message: &str, warning: bool) {
        let title = wide(title);
        let message = wide(message);
        MessageBoxW(
            hwnd,
            message.as_ptr(),
            title.as_ptr(),
            MB_OK
                | if warning {
                    MB_ICONWARNING
                } else {
                    MB_ICONINFORMATION
                },
        );
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_NCCREATE => {
                let create = &*(lparam as *const CREATESTRUCTW);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
                return DefWindowProcW(hwnd, message, wparam, lparam);
            }
            WM_CREATE => {
                let Some(gui) = state(hwnd) else {
                    return -1;
                };
                if let Err(error) = create_interface(hwnd, gui) {
                    show_message(hwnd, "界面初始化失败", &format!("{error:#}"), true);
                    return -1;
                }
                return 0;
            }
            WM_COMMAND => {
                let Some(gui) = state(hwnd) else {
                    return 0;
                };
                let id = (wparam & 0xffff) as i32;
                let notification = ((wparam >> 16) & 0xffff) as u32;
                match id {
                    ID_SOURCE if notification == CBN_SELCHANGE as u32 && !gui.busy => {
                        if let Err(error) = load_kmis(hwnd, gui) {
                            set_text(gui.status, &format!("本地制作失败 · {error:#}"));
                        }
                    }
                    ID_BOOT_BROWSE => {
                        if let Some(path) = file_dialog(hwnd, false, None) {
                            set_path(gui.boot, &path);
                            if read_os_text(gui.output).is_ok_and(|value| value.is_empty()) {
                                set_path(gui.output, &default_output(&path));
                            }
                        }
                    }
                    ID_OUTPUT_BROWSE => {
                        let initial = read_os_text(gui.output)
                            .ok()
                            .filter(|value| !value.is_empty())
                            .map(PathBuf::from);
                        if let Some(path) = file_dialog(hwnd, true, initial.as_deref()) {
                            set_path(gui.output, &path);
                        }
                    }
                    ID_PATCH => {
                        if let Err(error) = start_patch(hwnd, gui) {
                            show_message(hwnd, "无法开始修补", &format!("{error:#}"), true);
                        }
                    }
                    _ => {}
                }
                return 0;
            }
            MSG_KMIS_OK | MSG_KMIS_ERROR => {
                let result = Box::from_raw(lparam as *mut KmiResult);
                let Some(gui) = state(hwnd) else {
                    return 0;
                };
                if source_index(gui.source).ok() != Some(result.source_index) {
                    return 0;
                }

                SendMessageW(gui.kmi, CB_RESETCONTENT, 0, 0);
                if let Some(error) = &result.error {
                    add_combo_item(gui.kmi, "本地制作失败");
                    SendMessageW(gui.kmi, CB_SETCURSEL, 0, 0);
                    EnableWindow(gui.kmi, 0);
                    EnableWindow(gui.patch, 0);
                    set_text(gui.status, &format!("本地制作失败 · {error}"));
                    return 0;
                }

                add_combo_item(gui.kmi, "请选择设备对应的 KMI");
                for kmi in &result.values {
                    add_combo_item(gui.kmi, kmi);
                }
                SendMessageW(gui.kmi, CB_SETCURSEL, 0, 0);
                EnableWindow(gui.kmi, 1);
                if !gui.busy {
                    EnableWindow(gui.patch, 1);
                }
                set_text(
                    gui.status,
                    &format!(
                        "本地识别完成，共 {} 个 KMI，请选择设备对应项",
                        result.values.len()
                    ),
                );
                return 0;
            }
            MSG_PATCH_OK | MSG_PATCH_ERROR => {
                let text = Box::from_raw(lparam as *mut String);
                if let Some(gui) = state(hwnd) {
                    gui.busy = false;
                    EnableWindow(gui.patch, 1);
                    EnableWindow(gui.source, 1);
                    EnableWindow(gui.kmi, 1);
                    set_text(gui.patch, "开始修补");
                    set_text(gui.status, &text);
                    if message == MSG_PATCH_OK {
                        show_message(hwnd, "修补完成", &text, false);
                    } else {
                        show_message(hwnd, "修补失败", &text, true);
                    }
                }
                return 0;
            }
            WM_CTLCOLORSTATIC => {
                if let Some(gui) = state(hwnd) {
                    SetBkMode(wparam as *mut c_void, TRANSPARENT as i32);
                    SetTextColor(wparam as *mut c_void, DARK_TEXT);
                    return gui.background as isize;
                }
            }
            WM_CLOSE => {
                if state(hwnd).is_some_and(|gui| gui.busy) {
                    show_message(hwnd, "任务进行中", "请等待当前修补任务完成", true);
                    return 0;
                }
                DestroyWindow(hwnd);
                return 0;
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                return 0;
            }
            WM_NCDESTROY => {
                let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                if !pointer.is_null() {
                    let gui = Box::from_raw(pointer);
                    if !gui.body_font.is_null() {
                        DeleteObject(gui.body_font);
                    }
                    if !gui.title_font.is_null() {
                        DeleteObject(gui.title_font);
                    }
                }
            }
            _ => {}
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }

    pub fn run() -> Result<()> {
        // SAFETY: 所有 Win32 句柄仅由当前 GUI 线程创建和销毁。
        unsafe {
            let console = GetConsoleWindow();
            if !console.is_null() {
                ShowWindow(console, SW_HIDE);
            }

            let instance = GetModuleHandleW(null());
            ensure!(!instance.is_null(), "无法获取程序实例");
            let class_name = wide("KsudNativeGui");
            let background = CreateSolidBrush(MINT_BACKGROUND);
            ensure!(!background.is_null(), "创建窗口背景失败");
            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                hIcon: LoadIconW(null_mut(), IDI_APPLICATION),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                hbrBackground: background,
                lpszClassName: class_name.as_ptr(),
                ..Default::default()
            };
            ensure!(RegisterClassW(&class) != 0, "注册 GUI 窗口失败");

            let state = Box::new(GuiState::new(background));
            let state = Box::into_raw(state);
            let x = (GetSystemMetrics(SM_CXSCREEN) - WINDOW_WIDTH) / 2;
            let y = (GetSystemMetrics(SM_CYSCREEN) - WINDOW_HEIGHT) / 2;
            let title = wide("KernelSU Boot Patcher");
            let hwnd = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
                if x >= 0 { x } else { CW_USEDEFAULT },
                if y >= 0 { y } else { CW_USEDEFAULT },
                WINDOW_WIDTH,
                WINDOW_HEIGHT,
                null_mut(),
                null_mut(),
                instance,
                state.cast(),
            );
            if hwnd.is_null() {
                drop(Box::from_raw(state));
                anyhow::bail!("创建 GUI 窗口失败");
            }

            ShowWindow(hwnd, SW_SHOW);
            let mut message = MSG::default();
            loop {
                let result = GetMessageW(&mut message, null_mut(), 0, 0);
                if result == -1 {
                    anyhow::bail!("GUI 消息循环失败");
                }
                if result == 0 {
                    break;
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            Ok(())
        }
    }
}
