//! Stable command representation for cross-crate command previews.

use std::path::PathBuf;

use nwflash_windows::process::ProcessCommand;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub environment: Vec<(String, String)>,
}

impl From<ProcessCommand> for CommandSpec {
    fn from(value: ProcessCommand) -> Self {
        Self {
            program: value.program,
            args: value.args,
            working_directory: value.working_directory,
            environment: value.environment,
        }
    }
}

impl From<&ProcessCommand> for CommandSpec {
    fn from(value: &ProcessCommand) -> Self {
        Self::from(value.clone())
    }
}
