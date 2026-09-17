use std::{
    path::PathBuf,
    process::{Command, Output},
    sync::{Mutex, MutexGuard, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_application::FileManagerService;

const SERIAL: &str = "FT-HOST-CONTRACT";

#[derive(Clone)]
struct HostShell {
    program: PathBuf,
    prefix: Vec<String>,
    label: String,
}

impl HostShell {
    fn discover() -> Result<Self, String> {
        #[cfg(windows)]
        {
            let program = PathBuf::from(r"C:\Windows\System32\wsl.exe");
            let distribution = std::env::var("NWFLASH_SHELL_CONTRACT_WSL_DISTRO")
                .unwrap_or_else(|_| "Ubuntu-22.04".to_string());
            if !program.is_file() {
                return Err("wsl.exe is unavailable".to_string());
            }
            let shell = Self {
                program,
                prefix: vec![
                    "--distribution".to_string(),
                    distribution.clone(),
                    "--exec".to_string(),
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                ],
                label: format!("wsl:{distribution}:/bin/sh"),
            };
            let probe = shell.run(
                "command -v sh >/dev/null && command -v mv >/dev/null && command -v rm >/dev/null",
            )?;
            if !probe.status.success() {
                return Err(format!(
                    "fixed WSL distribution {distribution} lacks sh/mv/rm"
                ));
            }
            Ok(shell)
        }

        #[cfg(not(windows))]
        {
            let program = PathBuf::from("/bin/sh");
            if !program.is_file() {
                return Err("/bin/sh is unavailable".to_string());
            }
            Ok(Self {
                program,
                prefix: vec!["-c".to_string()],
                label: "host:/bin/sh".to_string(),
            })
        }
    }

    fn required() -> Self {
        if std::env::var("NWFLASH_RUN_HOST_SHELL_CONTRACT").as_deref() != Ok("1") {
            panic!("{}", "FT_SHELL_CONTRACT_BLOCKED={\"status\":\"blocked\",\"reason\":\"set NWFLASH_RUN_HOST_SHELL_CONTRACT=1 for the explicit host-only suite\"}");
        }
        static HOST_SHELL: OnceLock<Result<HostShell, String>> = OnceLock::new();
        HOST_SHELL
            .get_or_init(Self::discover)
            .clone()
            .unwrap_or_else(|reason| {
                panic!(
                    "FT_SHELL_CONTRACT_BLOCKED={{\"status\":\"blocked\",\"reason\":\"{reason}\"}}"
                )
            })
    }

    fn run(&self, script: &str) -> Result<Output, String> {
        Command::new(&self.program)
            .args(&self.prefix)
            .arg(script)
            .output()
            .map_err(|error| format!("failed to start fixed host shell: {error}"))
    }

    fn checked(&self, script: &str) -> Output {
        let output = self
            .run(script)
            .unwrap_or_else(|error| panic!("FT_SHELL_CONTRACT_BLOCKED={error}"));
        assert!(
            output.status.success(),
            "host fixture setup/probe failed: status={:?}",
            output.status.code()
        );
        output
    }
}

fn serialize_host_shell() -> MutexGuard<'static, ()> {
    static HOST_SHELL_LOCK: Mutex<()> = Mutex::new(());
    HOST_SHELL_LOCK
        .lock()
        .expect("host shell contract lock should not be poisoned")
}

struct ShellSandbox {
    shell: HostShell,
    root: String,
    files: Vec<String>,
    directories: Vec<String>,
}

impl ShellSandbox {
    fn new(label: &str) -> Self {
        let shell = HostShell::required();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let root = format!(
            "/tmp/nwflash-ft-contract-{label}-{}-{nonce}",
            std::process::id()
        );
        shell.checked(&format!("mkdir -- {}", quote(&root)));
        eprintln!(
            "FT_SHELL_CONTRACT_ENV={{\"status\":\"host-only\",\"shell\":\"{}\"}}",
            shell.label
        );
        Self {
            shell,
            root,
            files: Vec::new(),
            directories: Vec::new(),
        }
    }

    fn path(&self, name: &str) -> String {
        format!("{}/{name}", self.root)
    }

    fn write(&mut self, path: &str, bytes: &str) {
        self.shell
            .checked(&format!("printf '%s' {} > {}", quote(bytes), quote(path)));
        self.files.push(path.to_string());
    }

    fn directory(&mut self, path: &str) {
        self.shell.checked(&format!("mkdir -- {}", quote(path)));
        self.directories.push(path.to_string());
    }

    fn symlink(&mut self, target: &str, link: &str) {
        self.shell
            .checked(&format!("ln -s -- {} {}", quote(target), quote(link)));
        self.files.push(link.to_string());
    }

    fn kind(&self, path: &str) -> String {
        let path = quote(path);
        let output = self.shell.checked(&format!(
            "if [ -L {path} ]; then printf symlink; \
             elif [ -f {path} ]; then printf file; \
             elif [ -d {path} ]; then printf directory; \
             elif [ -e {path} ]; then printf other; \
             else printf absent; fi"
        ));
        String::from_utf8(output.stdout).expect("fixture kind must be UTF-8")
    }

    fn read(&self, path: &str) -> String {
        let output = self.shell.checked(&format!("cat -- {}", quote(path)));
        String::from_utf8(output.stdout).expect("fixture bytes must be UTF-8")
    }

    fn execute(&self, script: &str) -> i32 {
        self.shell
            .run(script)
            .unwrap_or_else(|error| panic!("host contract execution failed: {error}"))
            .status
            .code()
            .unwrap_or(-1)
    }
}

impl Drop for ShellSandbox {
    fn drop(&mut self) {
        // Every operand is an exact path owned by this UUID-like sandbox.  No
        // wildcard, parent traversal, or recursive deletion is used.
        let mut script = String::new();
        self.files.sort();
        self.files.dedup();
        for path in self.files.iter().rev() {
            script.push_str(&format!("rm -f -- {} 2>/dev/null; ", quote(path)));
        }
        self.directories
            .sort_by_key(|path| std::cmp::Reverse(path.len()));
        self.directories.dedup();
        for path in &self.directories {
            script.push_str(&format!("rmdir -- {} 2>/dev/null; ", quote(path)));
        }
        script.push_str(&format!(
            "rmdir -- {} 2>/dev/null; exit 0",
            quote(&self.root)
        ));
        let _ = self.shell.run(&script);
    }
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn command_script(command: nwflash_application::CommandSpec) -> String {
    assert_eq!(
        command.args.get(0..3),
        Some(["-s", SERIAL, "shell"].map(String::from).as_slice())
    );
    command
        .args
        .get(3)
        .expect("shell script must be the final structured argument")
        .clone()
}

fn promote_script(temp: &str, destination: &str) -> String {
    command_script(
        FileManagerService::with_platform_tools("adb.exe", "fastboot.exe")
            .build_remote_promote_no_replace_command(SERIAL, temp, destination)
            .expect("promote command should build"),
    )
}

fn cleanup_script(temp: &str) -> String {
    command_script(
        FileManagerService::with_platform_tools("adb.exe", "fastboot.exe")
            .build_remote_remove_file_command(SERIAL, temp)
            .expect("cleanup command should build"),
    )
}

#[test]
#[ignore = "explicit host-only test: set NWFLASH_RUN_HOST_SHELL_CONTRACT=1 and run --ignored"]
fn host_shell_contract_promotes_a_regular_file_to_an_exact_special_character_target() {
    let _serial = serialize_host_shell();
    let mut sandbox = ShellSandbox::new("promote-success");
    let temp = sandbox.path("temporary file '雪'.partial");
    let destination = sandbox.path("-final file '雪'.bin");
    sandbox.write(&temp, "complete-bytes");
    sandbox.files.push(destination.clone());

    let script = promote_script(&temp, &destination);
    assert!(script.contains("mv -n --"));
    assert!(!script.contains('*'));
    assert_eq!(sandbox.execute(&script), 0);
    assert_eq!(sandbox.kind(&temp), "absent");
    assert_eq!(sandbox.kind(&destination), "file");
    assert_eq!(sandbox.read(&destination), "complete-bytes");
}

#[test]
#[ignore = "explicit host-only test: set NWFLASH_RUN_HOST_SHELL_CONTRACT=1 and run --ignored"]
fn host_shell_contract_rejects_existing_file_directory_and_links_without_replacing_them() {
    let _serial = serialize_host_shell();
    for target_kind in ["file", "directory", "symlink", "broken-symlink"] {
        let mut sandbox = ShellSandbox::new(target_kind);
        let temp = sandbox.path("source.partial");
        let destination = sandbox.path("destination");
        sandbox.write(&temp, "new");

        match target_kind {
            "file" => sandbox.write(&destination, "old"),
            "directory" => sandbox.directory(&destination),
            "symlink" => {
                let target = sandbox.path("existing-target");
                sandbox.write(&target, "old");
                sandbox.symlink(&target, &destination);
            }
            "broken-symlink" => {
                let missing = sandbox.path("missing-target");
                sandbox.symlink(&missing, &destination);
            }
            _ => unreachable!(),
        }

        let before = sandbox.kind(&destination);
        assert_eq!(sandbox.execute(&promote_script(&temp, &destination)), 73);
        assert_eq!(sandbox.kind(&temp), "file", "target kind {target_kind}");
        assert_eq!(
            sandbox.kind(&destination),
            before,
            "target kind {target_kind}"
        );
        if target_kind == "file" {
            assert_eq!(sandbox.read(&destination), "old");
        }
    }
}

#[test]
#[ignore = "explicit host-only test: set NWFLASH_RUN_HOST_SHELL_CONTRACT=1 and run --ignored"]
fn host_shell_contract_rejects_missing_directory_and_symlink_temps() {
    let _serial = serialize_host_shell();
    for temp_kind in ["missing", "directory", "symlink", "broken-symlink"] {
        let mut sandbox = ShellSandbox::new(&format!("bad-temp-{temp_kind}"));
        let temp = sandbox.path("source.partial");
        let destination = sandbox.path("destination.bin");
        sandbox.files.push(destination.clone());
        match temp_kind {
            "missing" => {}
            "directory" => sandbox.directory(&temp),
            "symlink" => {
                let target = sandbox.path("real-source");
                sandbox.write(&target, "new");
                sandbox.symlink(&target, &temp);
            }
            "broken-symlink" => {
                let missing = sandbox.path("missing-source");
                sandbox.symlink(&missing, &temp);
            }
            _ => unreachable!(),
        }

        assert_eq!(sandbox.execute(&promote_script(&temp, &destination)), 72);
        assert_eq!(sandbox.kind(&destination), "absent");
    }
}

#[test]
#[ignore = "explicit host-only test: set NWFLASH_RUN_HOST_SHELL_CONTRACT=1 and run --ignored"]
fn host_shell_contract_cleanup_is_idempotent_and_checks_regular_and_broken_links() {
    let _serial = serialize_host_shell();
    for temp_kind in ["file", "missing", "symlink", "broken-symlink"] {
        let mut sandbox = ShellSandbox::new(&format!("cleanup-{temp_kind}"));
        let temp = sandbox.path("temporary '雪' file.partial");
        match temp_kind {
            "file" => sandbox.write(&temp, "partial"),
            "missing" => sandbox.files.push(temp.clone()),
            "symlink" => {
                let target = sandbox.path("real-file");
                sandbox.write(&target, "owned-target");
                sandbox.symlink(&target, &temp);
            }
            "broken-symlink" => {
                let missing = sandbox.path("missing-file");
                sandbox.symlink(&missing, &temp);
            }
            _ => unreachable!(),
        }

        let script = cleanup_script(&temp);
        assert!(script.contains("rm -f --"));
        assert!(script.contains("[ -e ") && script.contains("[ -L "));
        assert!(!script.contains('*'));
        assert_eq!(sandbox.execute(&script), 0);
        assert_eq!(sandbox.kind(&temp), "absent");
    }

    let mut sandbox = ShellSandbox::new("cleanup-directory");
    let temp_directory = sandbox.path("temporary-directory");
    sandbox.directory(&temp_directory);
    assert_ne!(sandbox.execute(&cleanup_script(&temp_directory)), 0);
    assert_eq!(sandbox.kind(&temp_directory), "directory");
}

#[test]
fn host_shell_contract_builder_rejects_root_and_equal_paths_before_execution() {
    let service = FileManagerService::with_platform_tools("adb.exe", "fastboot.exe");
    assert!(service
        .build_remote_promote_no_replace_command(SERIAL, "/", "/tmp/final")
        .is_err());
    assert!(service
        .build_remote_promote_no_replace_command(SERIAL, "/tmp/same", "/tmp/same")
        .is_err());
    assert!(service
        .build_remote_remove_file_command(SERIAL, "/")
        .is_err());
}
