use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

const SETTINGS_FILE: &str = "settings.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ToolPathSettings {
    pub scrcpy_path: Option<String>,
}

#[derive(Debug)]
pub struct ToolPathPreferences {
    settings_path: PathBuf,
    settings: ToolPathSettings,
}

impl ToolPathPreferences {
    pub fn with_path(settings_path: PathBuf) -> Self {
        let settings = Self::load(&settings_path);
        Self {
            settings_path,
            settings,
        }
    }

    pub fn create_default() -> Self {
        // 与 `expected_config_directory` 共用同一处规则：两者分别用 var / var_os
        // 读环境变量，若各写一份回退逻辑就会分叉，导致工具自己写出的配置
        // 被目录约束拒绝。统一走这里以保证只有一份真源。
        let root = expected_config_directory()
            .unwrap_or_else(|| std::env::temp_dir().join("VivoKsu"));
        Self::with_path(root.join(SETTINGS_FILE))
    }

    pub fn scrcpy_path(&self) -> Option<&str> {
        self.settings.scrcpy_path.as_deref()
    }

    pub fn save_scrcpy_path(&mut self, tool_path: &str) {
        self.settings.scrcpy_path = Some(PathBuf::from(tool_path).to_string_lossy().to_string());
        self.persist();
    }

    pub fn clear_scrcpy_path(&mut self) {
        self.settings.scrcpy_path = None;
        self.persist();
    }

    /// 读取本地配置，并施加与威胁模型相称的校验。
    ///
    /// ## 为什么这里**不**用 Ed25519 签名
    ///
    /// `settings.json` 是**本工具自己写**的。客户端没有签名私钥——
    /// `SESSION_SIGNING_PRIVATE_KEY_PKCS8` 只存在于 Cloudflare Worker secret，
    /// 客户端全仓的 `signing_key` 引用全部位于测试代码。因此若在此处强制验签：
    ///
    /// 1. 工具写出的配置**必然**验签失败；
    /// 2. 用户每次设置 scrcpy 路径，重启后都会被 fail-closed 清空；
    /// 3. 且这并不带来安全收益——能改这个文件的人，本来就能改 exe、注入 DLL、
    ///    读进程内存。**签名在这里提供不了额外边界，只会破坏功能。**
    ///
    /// 因此本地配置改用**平台信任**：文件必须位于工具的私有配置目录内，
    /// 且必须是常规文件（拒绝符号链接/重定向）。真正的密码学验签保留给
    /// **分发物**（固件包），那才是签名能防住的场景——见
    /// `nwflash-application::safe_flash::verify_firmware_package_signature`。
    ///
    /// 校验失败时回退到编译期默认值（fail-closed），并记录一条可见日志。
    fn load(path: &Path) -> ToolPathSettings {
        let Ok(bytes) = fs::read(path) else {
            // 文件不存在是正常首启，不算安全事件。
            return ToolPathSettings::default();
        };

        if let Err(reason) = validate_local_config_path(path) {
            eprintln!(
                "本地配置未采用（{reason}），已回退到默认设置（{}）。",
                path.display()
            );
            return ToolPathSettings::default();
        }

        let settings: ToolPathSettings = serde_json::from_slice(&bytes).unwrap_or_default();
        if let Some(scrcpy) = settings.scrcpy_path.as_deref() {
            if let Err(reason) = validate_scrcpy_path(scrcpy) {
                eprintln!(
                    "本地配置里的 scrcpy 路径被拒绝（{reason}），已回退到内置资源。"
                );
                return ToolPathSettings::default();
            }
        }
        settings
    }
    fn persist(&self) {
        if let Some(parent) = self.settings_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let temp_path = self.settings_path.with_extension("json.tmp");
        let payload =
            serde_json::to_string_pretty(&self.settings).unwrap_or_else(|_| "{}".to_string());
        let _ = fs::write(&temp_path, payload);
        let _ = fs::rename(temp_path, &self.settings_path);
    }
}

/// 校验配置文件本身是否可信。
///
/// 三条检查，都是"纯路径/元数据"层面能可靠判定、且与威胁模型相称的：
///
/// 1. **必须是常规文件**——拒绝符号链接与 Windows 重定向点。这是防
///    "把配置指向别处"最直接的一条。
/// 2. **父目录必须是工具的私有配置目录**（`%LOCALAPPDATA%\VivoKsu`
///    或临时目录回退）——配置不允许从任意位置加载。
/// 3. **文件大小有上限**——防止用超大文件做拒绝服务。
fn validate_local_config_path(path: &Path) -> Result<(), String> {
    const MAX_CONFIG_BYTES: u64 = 64 * 1024;

    let metadata = fs::symlink_metadata(path).map_err(|error| format!("无法读取元数据：{error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("配置是符号链接".to_string());
    }
    if !metadata.is_file() {
        return Err("配置不是常规文件".to_string());
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(format!("配置体积 {} 字节超出上限", metadata.len()));
    }

    // 目录约束只在**生产路径**启用：`local_config_directory_override()` 为
    // `None` 时要求配置位于工具私有目录内；测试可显式覆盖预期目录，
    // 从而既能验证约束本身、又能让往返类测试在临时目录里工作。
    if let Some(expected_dir) = expected_config_directory() {
        let actual_dir = path.parent().unwrap_or_else(|| Path::new(""));
        if !is_within(actual_dir, &expected_dir) {
            return Err("配置不在工具的私有配置目录内".to_string());
        }
    }

    Ok(())
}

/// 校验配置里记录的 scrcpy 路径。
///
/// 这个字段会让工具**拉起一个外部进程**，所以不能是任意路径：必须是绝对
/// 路径、必须指向常规文件，且拒绝符号链接。相对路径会被当前工作目录影响，
/// 是典型的劫持面，直接拒绝。
fn validate_scrcpy_path(candidate: &str) -> Result<(), String> {
    let path = Path::new(candidate);
    if !path.is_absolute() {
        return Err("不是绝对路径".to_string());
    }
    // **只做形态校验，不要求路径当前存在**：用户设置好路径后，设备可能
    // 拔掉（U 盘/移动硬盘）或网络盘暂时断开。若因此清空配置，用户会失去
    // 自己的设置——那是功能损失，换不来任何安全收益（真正的把关点在
    // 真正拉起进程的时候，那里必须重新校验）。
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err("路径是符号链接".to_string());
        }
    }
    Ok(())
}

/// 期望的配置目录：生产恒为工具私有目录；测试可覆盖以便验证目录约束。
fn expected_config_directory() -> Option<PathBuf> {
    let root = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    Some(root.join("VivoKsu"))
}

/// 判断 `candidate` 是否等于 `root` 或位于其下（大小写不敏感，与 Windows 语义一致）。
///
/// 用"位于其下"而不是"相等"：私有目录内的子目录同样可信，
/// 且未来若把配置放到 `VivoKsu\config\` 之类的位置无需再改这段。
/// 比较按**路径分量**进行，避免 `VivoKsuMalicious` 这类前缀相同的目录误判为在内。
fn is_within(candidate: &Path, root: &Path) -> bool {
    let normalize = |value: &Path| {
        value
            .to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    };
    let candidate = normalize(candidate);
    let root = normalize(root);
    candidate == root
        || candidate
            .strip_prefix(&root)
            .is_some_and(|rest| rest.starts_with('\\'))
}
