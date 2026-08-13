#![deny(clippy::all, clippy::pedantic)]
#![warn(clippy::nursery)]
#![allow(
    clippy::module_name_repetitions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::cast_possible_wrap
)]

// --- Library surface for embedders (e.g. fastboot) ---
pub mod boot_patch;
pub mod defs;

// --- Internal modules (same crate graph as the `ksud` binary) ---
#[cfg(target_os = "android")]
mod apk_sign;
mod assets;
#[cfg(target_os = "android")]
pub mod cli;
#[cfg(not(target_os = "android"))]
pub mod cli_non_android;
#[cfg(target_os = "android")]
mod debug;
#[cfg(target_os = "android")]
mod feature;
#[cfg(target_os = "android")]
mod init_event;
#[cfg(target_os = "android")]
mod ksucalls;
#[cfg(target_os = "android")]
mod late_load;
#[cfg(target_os = "android")]
mod magica;
#[cfg(target_os = "android")]
mod metamodule;
#[cfg(target_os = "android")]
mod module;
#[cfg(target_os = "android")]
mod module_config;
#[cfg(target_os = "android")]
mod profile;
#[cfg(target_os = "android")]
mod restorecon;
#[cfg(target_os = "android")]
mod sepolicy;
#[cfg(target_os = "android")]
mod su;
#[cfg(target_os = "android")]
mod utils;
