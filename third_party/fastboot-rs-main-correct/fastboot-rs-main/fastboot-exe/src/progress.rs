use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
static MACHINE_READABLE: AtomicBool = AtomicBool::new(false);
pub fn set_machine_readable(enabled: bool) {
    MACHINE_READABLE.store(enabled, Ordering::SeqCst);
}
pub fn is_machine_readable() -> bool {
    MACHINE_READABLE.load(Ordering::SeqCst)
}
#[derive(Clone)]
pub struct ProgressCounter {
    transferred: Arc<AtomicU64>,
}

impl ProgressCounter {
    pub fn new() -> Self {
        Self {
            transferred: Arc::new(AtomicU64::new(0)),
        }
    }
    #[inline(always)]
    pub fn add(&self, bytes: u64) {
        self.transferred.fetch_add(bytes, Ordering::Relaxed);
    }
    #[inline(always)]
    pub fn get(&self) -> u64 {
        self.transferred.load(Ordering::Relaxed)
    }
    #[inline(always)]
    pub fn reset(&self) {
        self.transferred.store(0, Ordering::Relaxed);
    }
}

impl Default for ProgressCounter {
    fn default() -> Self {
        Self::new()
    }
}

pub struct FlashProgress {
    bar: ProgressBar,
    counter: ProgressCounter,
    partition: String,
    start_time: Instant,
    total_size: u64,
    is_chunked: bool,
    chunk_index: usize,
    chunk_total: usize,
}

impl FlashProgress {
    pub fn new(partition: &str, total_size: u64, operation: &str) -> Self {
        let counter = ProgressCounter::new();

        let bar = if is_machine_readable() {
            ProgressBar::hidden()
        } else {
            let bar = ProgressBar::new(total_size);
            bar.set_draw_target(ProgressDrawTarget::stderr_with_hz(10));

            let style = ProgressStyle::default_bar()
                .template(
                    "{msg} [{bar:40.cyan/blue}] {percent:>3}% {binary_bytes_per_sec} ETA {eta}",
                )
                .expect("Invalid progress template")
                .progress_chars("=>-");

            bar.set_style(style);
            bar.set_message(format!("{} '{}'", operation, partition));
            bar.enable_steady_tick(Duration::from_millis(100));
            bar
        };

        Self {
            bar,
            counter,
            partition: partition.to_string(),
            start_time: Instant::now(),
            total_size,
            is_chunked: false,
            chunk_index: 0,
            chunk_total: 1,
        }
    }

    pub fn new_chunked(
        partition: &str,
        chunk_size: u64,
        chunk_index: usize,
        chunk_total: usize,
        operation: &str,
    ) -> Self {
        let counter = ProgressCounter::new();

        let bar = if is_machine_readable() {
            ProgressBar::hidden()
        } else {
            let bar = ProgressBar::new(chunk_size);
            bar.set_draw_target(ProgressDrawTarget::stderr_with_hz(10));

            let style = ProgressStyle::default_bar()
                .template("{prefix} {msg} [{bar:35.cyan/blue}] {percent:>3}% {binary_bytes_per_sec} ETA {eta}")
                .expect("Invalid progress template")
                .progress_chars("=>-");

            bar.set_style(style);
            bar.set_prefix(format!("[{}/{}]", chunk_index + 1, chunk_total));
            bar.set_message(format!("{} '{}'", operation, partition));
            bar.enable_steady_tick(Duration::from_millis(100));
            bar
        };

        Self {
            bar,
            counter,
            partition: partition.to_string(),
            start_time: Instant::now(),
            total_size: chunk_size,
            is_chunked: true,
            chunk_index,
            chunk_total,
        }
    }

    pub fn counter(&self) -> ProgressCounter {
        self.counter.clone()
    }

    pub fn sync(&self) {
        let current = self.counter.get();
        self.bar.set_position(current);
    }
    pub fn set_message(&self, msg: &str) {
        self.bar.set_message(msg.to_string());
    }
    pub fn set_operation(&self, operation: &str) {
        self.bar
            .set_message(format!("{} '{}'", operation, self.partition));
    }

    pub fn finish(&self) {
        let elapsed = self.start_time.elapsed();
        let total = self.total_size;
        let size_str = format_size(total);
        let time_str = format!("{:.2}s", elapsed.as_secs_f64());

        if self.is_chunked {
            crate::logger::log_line(&format!(
                "OKAY  Sending '{}' [{}/{}] ({}) 用时 {}",
                self.partition,
                self.chunk_index + 1,
                self.chunk_total,
                size_str,
                time_str
            ));
        } else {
            crate::logger::log_line(&format!(
                "OKAY  Sending '{}' ({}) 用时 {}",
                self.partition, size_str, time_str
            ));
        }

        if is_machine_readable() {
            println!(
                r#"{{"type":"chunk_done","partition":"{}","chunk":{}/{},"size":{},"time":{:.2}}}"#,
                self.partition,
                self.chunk_index + 1,
                self.chunk_total,
                total,
                elapsed.as_secs_f64()
            );
        } else {
            self.bar.finish_and_clear();

            if self.is_chunked {
                println!(
                    "[{}/{}] Sending '{}' ({})                    OKAY [{}]",
                    self.chunk_index + 1,
                    self.chunk_total,
                    self.partition,
                    size_str,
                    time_str
                );
            } else {
                println!(
                    "Sending '{}' ({})                    OKAY [{}]",
                    self.partition, size_str, time_str
                );
            }
        }
    }
    pub fn finish_with_error(&self, error: &str) {
        crate::logger::log_line(&format!("FAILED 分区 '{}': {}", self.partition, error));
        if is_machine_readable() {
            println!(
                r#"{{"type":"error","partition":"{}","error":"{}"}}"#,
                self.partition, error
            );
        } else {
            self.bar.finish_and_clear();
            eprintln!("Sending '{}' FAILED: {}", self.partition, error);
        }
    }
    pub fn inner(&self) -> &ProgressBar {
        &self.bar
    }
}

impl Drop for FlashProgress {
    fn drop(&mut self) {
        if !self.bar.is_finished() {
            self.bar.finish_and_clear();
        }
    }
}

pub struct ChunkedProgress {
    partition: String,
    total_size: u64,
    total_chunks: usize,
    current_chunk: usize,
    transferred: u64,
    start_time: Instant,
}

impl ChunkedProgress {
    pub fn new(partition: &str, total_size: u64, total_chunks: usize) -> Self {
        crate::logger::log_line(&format!(
            "开始刷写 '{}' ({}, 分 {} 块)",
            partition,
            format_size(total_size),
            total_chunks
        ));
        if !is_machine_readable() {
            println!(
                "Flashing '{}' ({}, {} chunks)...",
                partition,
                format_size(total_size),
                total_chunks
            );
        } else {
            println!(
                r#"{{"type":"start","partition":"{}","size":{},"chunks":{}}}"#,
                partition, total_size, total_chunks
            );
        }

        Self {
            partition: partition.to_string(),
            total_size,
            total_chunks,
            current_chunk: 0,
            transferred: 0,
            start_time: Instant::now(),
        }
    }
    pub fn start_chunk(&mut self, chunk_size: u64) -> FlashProgress {
        let progress = FlashProgress::new_chunked(
            &self.partition,
            chunk_size,
            self.current_chunk,
            self.total_chunks,
            "Sending",
        );
        self.current_chunk += 1;
        progress
    }
    pub fn finish_chunk(&mut self, chunk_size: u64) {
        self.transferred += chunk_size;
    }
    pub fn finish(&self) {
        let elapsed = self.start_time.elapsed();
        let speed = self.total_size as f64 / elapsed.as_secs_f64();

        crate::logger::log_line(&format!(
            "完成刷写 '{}': {} 用时 {:.1}s ({})",
            self.partition,
            format_size(self.total_size),
            elapsed.as_secs_f64(),
            format_speed(speed)
        ));

        if is_machine_readable() {
            println!(
                r#"{{"type":"finished","partition":"{}","size":{},"time":{:.2},"speed":{:.0}}}"#,
                self.partition,
                self.total_size,
                elapsed.as_secs_f64(),
                speed
            );
        } else {
            println!(
                "Finished '{}': {} in {:.1}s ({})",
                self.partition,
                format_size(self.total_size),
                elapsed.as_secs_f64(),
                format_speed(speed)
            );
        }
    }
}

pub fn make_progress_callback(counter: ProgressCounter) -> Box<dyn Fn(u64, u64) + Send + Sync> {
    let last_position = Arc::new(AtomicU64::new(0));

    Box::new(move |current: u64, _total: u64| {
        let last = last_position.swap(current, Ordering::Relaxed);
        if current > last {
            counter.add(current - last);
        }
    })
}

pub fn make_bar_callback(bar: ProgressBar) -> Box<dyn Fn(u64, u64) + Send + Sync> {
    Box::new(move |current: u64, _total: u64| {
        bar.set_position(current);
    })
}

pub fn format_speed(bytes_per_sec: f64) -> String {
    if bytes_per_sec < 1024.0 {
        format!("{:.0} B/s", bytes_per_sec)
    } else if bytes_per_sec < 1024.0 * 1024.0 {
        format!("{:.1} KB/s", bytes_per_sec / 1024.0)
    } else if bytes_per_sec < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB/s", bytes_per_sec / 1024.0 / 1024.0)
    } else {
        format!("{:.1} GB/s", bytes_per_sec / 1024.0 / 1024.0 / 1024.0)
    }
}
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    } else {
        format!("{:.2} GB", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
    }
}
pub fn print_success(message: &str) {
    crate::logger::log_line(message);
    if !is_machine_readable() {
        println!("{}", message);
    }
}
pub fn print_error(message: &str) {
    crate::logger::log_line(&format!("错误: {}", message));
    eprintln!("{}", message);
}
pub fn print_warning(message: &str) {
    crate::logger::log_line(&format!("警告: {}", message));
    if !is_machine_readable() {
        eprintln!("{}", message);
    }
}
pub fn print_info(message: &str) {
    crate::logger::log_line(message);
    if !is_machine_readable() {
        println!("{}", message);
    }
}

pub struct Spinner {
    bar: ProgressBar,
}

impl Spinner {
    pub fn new(message: &str) -> Self {
        let bar = if is_machine_readable() {
            ProgressBar::hidden()
        } else {
            let bar = ProgressBar::new_spinner();
            bar.set_draw_target(ProgressDrawTarget::stderr_with_hz(10));
            bar.set_style(
                ProgressStyle::default_spinner()
                    .template("{spinner:.cyan} {msg}")
                    .expect("Invalid spinner template"),
            );
            bar.set_message(message.to_string());
            bar.enable_steady_tick(Duration::from_millis(100));
            bar
        };

        Self { bar }
    }

    pub fn tick(&self) {
        self.bar.tick();
    }

    pub fn set_message(&self, message: &str) {
        self.bar.set_message(message.to_string());
    }

    pub fn finish(&self, message: &str) {
        if is_machine_readable() {
            return;
        }
        self.bar.finish_and_clear();
        println!("{}", message);
    }

    pub fn fail(&self, message: &str) {
        self.bar.finish_and_clear();
        eprintln!("{}", message);
    }
}

pub struct SimpleProgressBar {
    bar: ProgressBar,
}

impl SimpleProgressBar {
    pub fn new(total: u64, operation: &str) -> Self {
        let bar = if is_machine_readable() {
            ProgressBar::hidden()
        } else {
            let bar = ProgressBar::new(total);
            bar.set_draw_target(ProgressDrawTarget::stderr_with_hz(10));
            let style = ProgressStyle::default_bar()
                .template("{msg} [{bar:40.cyan/blue}] {percent:>3}%")
                .expect("Invalid progress template")
                .progress_chars("=>-");
            bar.set_style(style);
            bar.set_message(operation.to_string());
            bar
        };

        Self { bar }
    }

    pub fn update(&mut self, position: u64) {
        self.bar.set_position(position);
    }

    pub fn finish(&self) {
        self.bar.finish_and_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_speed() {
        assert_eq!(format_speed(500.0), "500 B/s");
        assert_eq!(format_speed(1024.0), "1.0 KB/s");
        assert_eq!(format_speed(1024.0 * 1024.0), "1.0 MB/s");
        assert_eq!(format_speed(1024.0 * 1024.0 * 1024.0), "1.0 GB/s");
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn test_progress_counter() {
        let counter = ProgressCounter::new();
        assert_eq!(counter.get(), 0);

        counter.add(100);
        assert_eq!(counter.get(), 100);

        counter.add(50);
        assert_eq!(counter.get(), 150);

        counter.reset();
        assert_eq!(counter.get(), 0);
    }

    #[test]
    fn test_machine_readable_mode() {
        set_machine_readable(true);
        assert!(is_machine_readable());
        set_machine_readable(false);
        assert!(!is_machine_readable());
    }
}
