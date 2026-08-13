use crate::adb::connection::{AdbConnection, AdbTransport};
use std::io::{self, Write};
pub struct ShellSession<'a, T: AdbTransport> {
    conn: &'a mut AdbConnection<T>,
    stream_id: u32,
}

impl<'a, T: AdbTransport> ShellSession<'a, T> {
    pub fn open(conn: &'a mut AdbConnection<T>, command: Option<&str>) -> io::Result<Self> {
        let destination = match command {
            Some(cmd) => format!("shell:{}", cmd),
            None => "shell:".to_string(),
        };

        let stream_id = conn.open_stream(&destination)?;

        Ok(Self { conn, stream_id })
    }
    pub fn execute(conn: &mut AdbConnection<T>, command: &str) -> io::Result<String> {
        let destination = format!("shell:{}", command);
        let stream_id = conn.open_stream(&destination)?;

        let mut output = Vec::new();

        loop {
            match conn.read_stream(stream_id)? {
                None => break,
                Some(data) if !data.is_empty() => {
                    output.extend_from_slice(&data);
                }
                Some(_) => {}
            }
        }

        conn.close_stream(stream_id)?;

        Ok(String::from_utf8_lossy(&output).to_string())
    }
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.conn.write_stream(self.stream_id, data)
    }
    pub fn read(&mut self) -> io::Result<Option<Vec<u8>>> {
        self.conn.read_stream(self.stream_id)
    }
    pub fn close(self) -> io::Result<()> {
        self.conn.close_stream(self.stream_id)
    }
}

#[cfg(windows)]
mod terminal {
    use std::io;
    use winapi::um::consoleapi::{GetConsoleMode, SetConsoleMode};
    use winapi::um::handleapi::INVALID_HANDLE_VALUE;
    use winapi::um::processenv::GetStdHandle;
    use winapi::um::winbase::{STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    use winapi::um::wincon::{
        ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
        ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    };

    pub struct RawModeGuard {
        stdin_handle: *mut winapi::ctypes::c_void,
        stdout_handle: *mut winapi::ctypes::c_void,
        original_stdin_mode: u32,
        original_stdout_mode: u32,
    }

    impl RawModeGuard {
        pub fn enable() -> io::Result<Self> {
            unsafe {
                let stdin_handle = GetStdHandle(STD_INPUT_HANDLE);
                let stdout_handle = GetStdHandle(STD_OUTPUT_HANDLE);

                if stdin_handle == INVALID_HANDLE_VALUE || stdout_handle == INVALID_HANDLE_VALUE {
                    return Err(io::Error::last_os_error());
                }

                let mut original_stdin_mode: u32 = 0;
                let mut original_stdout_mode: u32 = 0;

                if GetConsoleMode(stdin_handle, &mut original_stdin_mode) == 0 {
                    return Err(io::Error::last_os_error());
                }
                if GetConsoleMode(stdout_handle, &mut original_stdout_mode) == 0 {
                    return Err(io::Error::last_os_error());
                }

                let new_stdin_mode = (original_stdin_mode
                    & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
                    | ENABLE_VIRTUAL_TERMINAL_INPUT;
                let new_stdout_mode = original_stdout_mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING;

                if SetConsoleMode(stdin_handle, new_stdin_mode) == 0 {
                    return Err(io::Error::last_os_error());
                }
                if SetConsoleMode(stdout_handle, new_stdout_mode) == 0 {
                    SetConsoleMode(stdin_handle, original_stdin_mode);
                    return Err(io::Error::last_os_error());
                }

                Ok(Self {
                    stdin_handle,
                    stdout_handle,
                    original_stdin_mode,
                    original_stdout_mode,
                })
            }
        }
    }

    impl Drop for RawModeGuard {
        fn drop(&mut self) {
            unsafe {
                SetConsoleMode(self.stdin_handle, self.original_stdin_mode);
                SetConsoleMode(self.stdout_handle, self.original_stdout_mode);
            }
        }
    }
    pub fn read_stdin_nonblocking(buf: &mut [u8]) -> io::Result<usize> {
        use winapi::um::consoleapi::ReadConsoleInputW;
        use winapi::um::processenv::GetStdHandle;
        use winapi::um::synchapi::WaitForSingleObject;
        use winapi::um::winbase::STD_INPUT_HANDLE;
        use winapi::um::winbase::WAIT_OBJECT_0;
        use winapi::um::wincon::{INPUT_RECORD, KEY_EVENT};

        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            let wait_result = WaitForSingleObject(handle, 10);
            if wait_result != WAIT_OBJECT_0 {
                return Ok(0);
            }

            let mut records: [INPUT_RECORD; 16] = std::mem::zeroed();
            let mut num_read: u32 = 0;

            if ReadConsoleInputW(handle, records.as_mut_ptr(), 16, &mut num_read) == 0 {
                return Err(io::Error::last_os_error());
            }

            let mut written = 0;
            for i in 0..num_read as usize {
                if records[i].EventType == KEY_EVENT {
                    let key_event = records[i].Event.KeyEvent();
                    if key_event.bKeyDown != 0 {
                        let ch = *key_event.uChar.AsciiChar() as u8;
                        if ch != 0 && written < buf.len() {
                            buf[written] = ch;
                            written += 1;
                        }
                    }
                }
            }

            Ok(written)
        }
    }
}

#[cfg(not(windows))]
mod terminal {
    use std::io;

    pub struct RawModeGuard;

    impl RawModeGuard {
        pub fn enable() -> io::Result<Self> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "shell 不支持"))
        }
    }

    pub fn read_stdin_nonblocking(_buf: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
}

pub use terminal::RawModeGuard;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub fn run_interactive_shell<T: AdbTransport>(conn: &mut AdbConnection<T>) -> io::Result<()> {
    let _raw_guard = match RawModeGuard::enable() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("raw mode 失败: {}，使用简单模式", e);
            return run_simple_shell(conn);
        }
    };
    let stream_id = conn.open_stream("shell:")?;

    let running = Arc::new(AtomicBool::new(true));
    let mut stdout = io::stdout();
    while running.load(Ordering::Relaxed) {
        let mut input_buf = [0u8; 256];
        match terminal::read_stdin_nonblocking(&mut input_buf) {
            Ok(n) if n > 0 => {
                if let Err(e) = conn.write_stream(stream_id, &input_buf[..n]) {
                    eprintln!("\n写入失败: {}", e);
                    running.store(false, Ordering::Relaxed);
                }
            }
            Ok(_) => {}
            Err(e) => {
                if e.kind() != io::ErrorKind::WouldBlock {
                    eprintln!("\n读取失败: {}", e);
                    running.store(false, Ordering::Relaxed);
                }
            }
        }
        match conn.try_read_stream(stream_id, 10) {
            Ok(Some(data)) if !data.is_empty() => {
                stdout.write_all(&data)?;
                stdout.flush()?;
            }
            Ok(None) => {
                running.store(false, Ordering::Relaxed);
            }
            Ok(_) => {}
            Err(e) => {
                if e.kind() != io::ErrorKind::TimedOut && e.kind() != io::ErrorKind::WouldBlock {
                    eprintln!("\n错误: {}", e);
                    running.store(false, Ordering::Relaxed);
                }
            }
        }
    }

    conn.close_stream(stream_id)?;
    Ok(())
}
fn run_simple_shell<T: AdbTransport>(conn: &mut AdbConnection<T>) -> io::Result<()> {
    use std::io::BufRead;

    eprintln!("交互式 shell（输入 exit 退出）");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        print!("$ ");
        stdout.flush()?;

        let mut input = String::new();
        match stdin.lock().read_line(&mut input) {
            Ok(0) => break,
            Ok(_) => {
                let cmd = input.trim();
                if cmd.is_empty() {
                    continue;
                }
                if cmd == "exit" || cmd == "quit" {
                    break;
                }

                match ShellSession::execute(conn, cmd) {
                    Ok(output) => print!("{}", output),
                    Err(e) => eprintln!("error: {}", e),
                }
            }
            Err(_) => break,
        }
    }

    Ok(())
}
