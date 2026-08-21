//! Platform tool command builders for adb/fastboot invocations.

use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use nwflash_domain::DomainError;
use sha2::{Digest, Sha256};

use crate::process::{run_command, ProcessCommand, ProcessOutput};

pub trait ProcessExecutor: Send + Sync {
    fn run(&self, command: ProcessCommand) -> Result<ProcessOutput, DomainError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProcessExecutor;

impl ProcessExecutor for SystemProcessExecutor {
    fn run(&self, command: ProcessCommand) -> Result<ProcessOutput, DomainError> {
        run_command(command)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformTools {
    adb_executable: String,
    fastboot_executable: String,
    bundled_tools_root: Option<PathBuf>,
}

const PLATFORM_TOOLS_MANIFEST: &str = "PLATFORM_TOOLS.SHA256";
const REQUIRED_PLATFORM_TOOLS: [(&str, &str); 4] = [
    (
        "adb.exe",
        "2e8a440a90ff1b15c8cf93eaf47fbb8f95fc0d14e9fa665dd9f4a2596387bbbf",
    ),
    (
        "AdbWinApi.dll",
        "9a56e72fe1372cb722a80c00b79dcecf2b37165884e470ed05f00c668c0043b0",
    ),
    (
        "AdbWinUsbApi.dll",
        "5e77ccb2f25cd3a97553745adf1cd28a5fe8137cf64613b7fef9c6f92ff91f37",
    ),
    (
        "fastboot.exe",
        "77d44117bf98b9716e2bd28fbd148ee34ab22e560fbb9c146fe39e4381bccea4",
    ),
];

/// Directory Tauri unpacks `bundle.resources` into: `<exe dir>/resources`.
///
/// `tauri.conf.json` declares `resources/platform-tools/*`, so the shipped
/// `adb.exe` lands at `<exe dir>/resources/platform-tools/adb.exe` — both in a
/// packaged install and under `target/<profile>/` while developing.  Resolving
/// the bundle explicitly is what keeps the app working on machines that never
/// added Android platform-tools to `PATH`, matching the WPF build which always
/// shipped and addressed its own copy.
pub fn bundled_resource_root() -> PathBuf {
    let executable = std::env::current_exe().ok();
    let current_directory = std::env::current_dir().ok();
    let candidates = resource_root_candidates(executable.as_deref(), current_directory.as_deref());

    candidates
        .iter()
        .find(|root| {
            root.join("platform-tools")
                .join(PLATFORM_TOOLS_MANIFEST)
                .is_file()
        })
        .cloned()
        .or_else(|| candidates.into_iter().next())
        .unwrap_or_else(|| PathBuf::from(".").join("resources"))
}

fn resource_root_candidates(
    executable: Option<&Path>,
    current_directory: Option<&Path>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut directory = executable.and_then(Path::parent);
    while let Some(path) = directory {
        candidates.push(path.join("resources"));
        directory = path.parent();
    }
    if let Some(path) = current_directory {
        candidates.push(path.join("resources"));
    }
    candidates
}

/// Absolute path to a tool shipped under `resources/platform-tools/`.
///
/// This deliberately does not use a bare file name as a fallback.  A missing
/// release resource must fail at the command boundary instead of executing a
/// similarly named program supplied through the user's `PATH`.
pub fn bundled_platform_tool(file_name: &str) -> String {
    bundled_platform_tools_root()
        .join(file_name)
        .to_string_lossy()
        .into_owned()
}

pub fn bundled_platform_tools_root() -> PathBuf {
    bundled_resource_root().join("platform-tools")
}

/// Verifies every runtime dependency of the bundled Android platform-tools
/// package.  The manifest is checked against the version-pinned digests before
/// it is used, so replacing both a binary and its adjacent manifest cannot make
/// the modified package appear trusted.
pub fn verify_bundled_platform_tools(root: &Path) -> Result<(), DomainError> {
    let manifest_path = root.join(PLATFORM_TOOLS_MANIFEST);
    let manifest = std::fs::read_to_string(&manifest_path).map_err(|_| {
        DomainError::ExternalTool("内置 Android platform-tools 完整性清单缺失。".to_string())
    })?;
    let declared = parse_platform_tools_manifest(&manifest)?;

    if declared.len() != REQUIRED_PLATFORM_TOOLS.len() {
        return Err(platform_tools_integrity_error());
    }

    for (file_name, expected_hash) in REQUIRED_PLATFORM_TOOLS {
        if declared
            .get(file_name)
            .is_none_or(|declared_hash| !declared_hash.eq_ignore_ascii_case(expected_hash))
        {
            return Err(platform_tools_integrity_error());
        }

        let path = root.join(file_name);
        let actual_hash = compute_sha256(&path).map_err(|_| platform_tools_integrity_error())?;
        if !actual_hash.eq_ignore_ascii_case(expected_hash) {
            return Err(platform_tools_integrity_error());
        }
    }

    Ok(())
}

/// Applies the bundle integrity policy only to the two tool programs that the
/// application generates itself.  Generic process helpers remain available to
/// test doubles and unrelated Windows tooling.
pub fn verify_if_bundled_platform_tool(program: &Path) -> Result<(), DomainError> {
    let root = bundled_platform_tools_root();
    if REQUIRED_PLATFORM_TOOLS
        .iter()
        .filter(|(file_name, _)| matches!(*file_name, "adb.exe" | "fastboot.exe"))
        .any(|(file_name, _)| program == root.join(file_name))
    {
        verify_bundled_platform_tools(&root)?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct PlatformDeviceDiscovery<E = SystemProcessExecutor> {
    tools: PlatformTools,
    executor: E,
}

impl PlatformDeviceDiscovery<SystemProcessExecutor> {
    pub fn new(tools: PlatformTools) -> Self {
        Self::with_executor(tools, SystemProcessExecutor)
    }
}

impl<E> PlatformDeviceDiscovery<E>
where
    E: ProcessExecutor,
{
    pub fn with_executor(tools: PlatformTools, executor: E) -> Self {
        Self { tools, executor }
    }

    pub fn discover_adb(&self) -> Result<String, DomainError> {
        let output = self.executor.run(self.tools.adb_devices_command()?)?;
        output_to_discovery_text("ADB", output)
    }

    pub fn discover_fastboot(&self) -> Result<String, DomainError> {
        let output = self.executor.run(self.tools.fastboot_devices_command()?)?;
        output_to_discovery_text("Fastboot", output)
    }
}

fn output_to_discovery_text(label: &str, output: ProcessOutput) -> Result<String, DomainError> {
    if output.exit_code == 0 {
        return Ok(output.stdout);
    }

    let detail = if output.stderr.trim().is_empty() {
        output.stdout
    } else {
        output.stderr
    };
    Err(DomainError::ExternalTool(format!(
        "{label} 设备检测失败: {detail}"
    )))
}

impl PlatformTools {
    pub fn new(adb_executable: impl Into<String>, fastboot_executable: impl Into<String>) -> Self {
        Self {
            adb_executable: adb_executable.into(),
            fastboot_executable: fastboot_executable.into(),
            bundled_tools_root: None,
        }
    }

    /// Resolves `adb.exe`/`fastboot.exe` from the shipped
    /// `resources/platform-tools` directory rather than trusting the user's
    /// `PATH`.  Prefer this over `new("adb.exe", "fastboot.exe")` everywhere in
    /// production code.
    pub fn bundled() -> Self {
        Self {
            adb_executable: bundled_platform_tool("adb.exe"),
            fastboot_executable: bundled_platform_tool("fastboot.exe"),
            bundled_tools_root: Some(bundled_platform_tools_root()),
        }
    }

    pub fn adb_executable(&self) -> &str {
        &self.adb_executable
    }

    pub fn fastboot_executable(&self) -> &str {
        &self.fastboot_executable
    }

    pub fn adb_environment(&self) -> Vec<(String, String)> {
        if self.adb_executable.trim().is_empty() {
            Vec::new()
        } else {
            vec![("ADB".to_string(), self.adb_executable.clone())]
        }
    }

    pub fn adb_command(
        &self,
        serial: &str,
        arguments: &[String],
    ) -> Result<ProcessCommand, DomainError> {
        let args = build_serial_args("ADB", serial, arguments)?;
        validate_tool_path("ADB", &self.adb_executable)?;
        Ok(ProcessCommand {
            program: self.adb_executable.clone(),
            args,
            working_directory: None,
            environment: Vec::new(),
        })
    }

    pub fn adb_devices_command(&self) -> Result<ProcessCommand, DomainError> {
        validate_tool_path("ADB", &self.adb_executable)?;
        Ok(ProcessCommand {
            program: self.adb_executable.clone(),
            args: vec!["devices".to_string(), "-l".to_string()],
            working_directory: None,
            environment: Vec::new(),
        })
    }

    pub fn fastboot_command(
        &self,
        serial: &str,
        arguments: &[String],
    ) -> Result<ProcessCommand, DomainError> {
        let args = build_serial_args("fastboot", serial, arguments)?;
        validate_tool_path("fastboot", &self.fastboot_executable)?;
        Ok(ProcessCommand {
            program: self.fastboot_executable.clone(),
            args,
            working_directory: None,
            environment: self.adb_environment(),
        })
    }

    pub fn fastboot_devices_command(&self) -> Result<ProcessCommand, DomainError> {
        validate_tool_path("fastboot", &self.fastboot_executable)?;
        Ok(ProcessCommand {
            program: self.fastboot_executable.clone(),
            args: vec!["devices".to_string()],
            working_directory: None,
            environment: self.adb_environment(),
        })
    }

    pub fn adb_available(&self) -> bool {
        Path::new(&self.adb_executable).is_file()
            && self
                .bundled_tools_root
                .as_deref()
                .is_none_or(|root| verify_bundled_platform_tools(root).is_ok())
    }

    pub fn fastboot_available(&self) -> bool {
        Path::new(&self.fastboot_executable).is_file()
            && self
                .bundled_tools_root
                .as_deref()
                .is_none_or(|root| verify_bundled_platform_tools(root).is_ok())
    }

    pub fn resolve_adb_command_path(&self) -> PathBuf {
        PathBuf::from(&self.adb_executable)
    }

    pub fn resolve_fastboot_command_path(&self) -> PathBuf {
        PathBuf::from(&self.fastboot_executable)
    }
}

fn build_serial_args(
    label: &str,
    serial: &str,
    arguments: &[String],
) -> Result<Vec<String>, DomainError> {
    if serial.trim().is_empty() {
        return Err(DomainError::InvalidInput(format!(
            "{} 串口不能为空。",
            label
        )));
    }

    let mut args = Vec::with_capacity(arguments.len() + 2);
    args.push("-s".to_string());
    args.push(serial.to_string());
    args.extend(arguments.iter().cloned());
    Ok(args)
}

fn validate_tool_path(label: &str, path: &str) -> Result<(), DomainError> {
    if path.trim().is_empty() {
        return Err(DomainError::InvalidInput(format!(
            "{} 工具路径不能为空。",
            label
        )));
    }
    Ok(())
}

fn parse_platform_tools_manifest(manifest: &str) -> Result<BTreeMap<String, String>, DomainError> {
    let mut entries = BTreeMap::new();
    for line in manifest
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let mut fields = line.split_whitespace();
        let Some(hash) = fields.next() else {
            return Err(platform_tools_integrity_error());
        };
        let Some(file_name) = fields.next() else {
            return Err(platform_tools_integrity_error());
        };
        if fields.next().is_some()
            || hash.len() != 64
            || !hash.chars().all(|character| character.is_ascii_hexdigit())
            || Path::new(file_name)
                .file_name()
                .and_then(|value| value.to_str())
                .filter(|value| *value == file_name)
                .is_none()
            || !REQUIRED_PLATFORM_TOOLS
                .iter()
                .any(|(required_file, _)| *required_file == file_name)
            || entries
                .insert(file_name.to_string(), hash.to_string())
                .is_some()
        {
            return Err(platform_tools_integrity_error());
        }
    }
    Ok(entries)
}

fn compute_sha256(path: &Path) -> Result<String, std::io::Error> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn platform_tools_integrity_error() -> DomainError {
    DomainError::ExternalTool("内置 Android platform-tools 完整性校验失败。".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_resource_root_points_at_the_tauri_resources_folder() {
        // tauri.conf.json bundles `resources/platform-tools/*`, so the resolved
        // root must end with `resources` next to the running executable.
        let root = bundled_resource_root();
        assert_eq!(
            root.file_name().and_then(|name| name.to_str()),
            Some("resources")
        );
    }

    #[test]
    fn resource_root_searches_parent_of_a_test_executable_directory() {
        let root =
            std::env::temp_dir().join(format!("nwflash-resource-root-{}", std::process::id()));
        let tools_root = root
            .join("target")
            .join("debug")
            .join("resources")
            .join("platform-tools");
        std::fs::create_dir_all(&tools_root).expect("fixture directory should exist");
        std::fs::write(tools_root.join(PLATFORM_TOOLS_MANIFEST), "fixture")
            .expect("fixture manifest should exist");

        let executable = root
            .join("target")
            .join("debug")
            .join("deps")
            .join("test.exe");
        let resolved = resource_root_candidates(Some(&executable), None)
            .into_iter()
            .find(|candidate| {
                candidate
                    .join("platform-tools")
                    .join(PLATFORM_TOOLS_MANIFEST)
                    .is_file()
            })
            .expect("parent resources directory should be found");

        assert_eq!(
            resolved,
            root.join("target").join("debug").join("resources")
        );
        std::fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn shipped_platform_tools_manifest_matches_binaries() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("resources")
            .join("platform-tools");

        verify_bundled_platform_tools(&root)
            .expect("shipped platform-tools must pass its integrity manifest");
    }

    #[test]
    fn bundled_platform_tool_never_falls_back_to_path_when_not_shipped() {
        let tool = bundled_platform_tool("definitely-not-shipped.exe");

        assert!(Path::new(&tool).is_absolute());
        assert_ne!(tool, "definitely-not-shipped.exe");
    }

    #[test]
    fn bundled_tools_are_absolute_even_when_the_bundle_is_missing() {
        let adb = bundled_platform_tool("adb.exe");
        assert!(Path::new(&adb).is_absolute());
        assert!(adb.contains("resources"));
    }

    #[test]
    fn bundled_tools_reject_a_missing_integrity_manifest() {
        let root = std::env::temp_dir().join(format!(
            "nwflash-platform-tools-missing-manifest-{}",
            std::process::id()
        ));
        let tools_root = root.join("platform-tools");
        std::fs::create_dir_all(&tools_root).expect("fixture directory should exist");

        let error = verify_bundled_platform_tools(&tools_root)
            .expect_err("a bundled platform-tools directory without its manifest must fail");

        assert!(error.to_string().contains("完整性清单"));
        std::fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn platform_tools_rejects_empty_serial() {
        let tools = PlatformTools::new("adb.exe", "fastboot.exe");
        let err = tools
            .fastboot_command("", &[])
            .expect_err("fastboot serial should be required");
        assert!(err.to_string().contains("fastboot 串口不能为空"));
    }

    #[test]
    fn platform_tools_sets_adb_environment() {
        let tools = PlatformTools::new("C:/Tools/adb.exe", "fastboot.exe");
        let env = tools.adb_environment();
        assert_eq!(
            env,
            vec![("ADB".to_string(), "C:/Tools/adb.exe".to_string())]
        );
    }

    #[test]
    fn platform_tools_builds_device_discovery_commands_without_a_serial() {
        let tools = PlatformTools::new("adb.exe", "fastboot.exe");

        let adb = tools
            .adb_devices_command()
            .expect("adb discovery should build");
        let fastboot = tools
            .fastboot_devices_command()
            .expect("fastboot discovery should build");

        assert_eq!(adb.program, "adb.exe");
        assert_eq!(adb.args, vec!["devices".to_string(), "-l".to_string()]);
        assert_eq!(fastboot.program, "fastboot.exe");
        assert_eq!(fastboot.args, vec!["devices".to_string()]);
        assert_eq!(
            fastboot.environment,
            vec![("ADB".to_string(), "adb.exe".to_string())]
        );
    }

    #[test]
    fn device_discovery_runs_adb_then_fastboot_with_prebuilt_commands() {
        let tools = PlatformTools::new("adb.exe", "fastboot.exe");
        let executor = RecordingExecutor::new(vec!["ADB".to_string(), "FASTBOOT".to_string()]);
        let discovery = PlatformDeviceDiscovery::with_executor(tools, executor.clone());

        assert_eq!(discovery.discover_adb().expect("adb discovery"), "ADB");
        assert_eq!(
            discovery.discover_fastboot().expect("fastboot discovery"),
            "FASTBOOT"
        );
        assert_eq!(executor.programs(), vec!["adb.exe", "fastboot.exe"]);
    }

    #[derive(Clone)]
    struct RecordingExecutor {
        outputs: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        programs: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl RecordingExecutor {
        fn new(outputs: Vec<String>) -> Self {
            Self {
                outputs: std::sync::Arc::new(std::sync::Mutex::new(
                    outputs.into_iter().rev().collect(),
                )),
                programs: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn programs(&self) -> Vec<String> {
            self.programs.lock().expect("programs lock").clone()
        }
    }

    impl ProcessExecutor for RecordingExecutor {
        fn run(&self, command: ProcessCommand) -> Result<ProcessOutput, DomainError> {
            self.programs
                .lock()
                .expect("programs lock")
                .push(command.program);
            Ok(ProcessOutput {
                exit_code: 0,
                stdout: self
                    .outputs
                    .lock()
                    .expect("outputs lock")
                    .pop()
                    .expect("output"),
                stderr: String::new(),
            })
        }
    }
}
