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
        let root = std::env::var("LOCALAPPDATA")
            .or_else(|_| std::env::var("APPDATA"))
            .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().to_string());
        Self::with_path(Path::new(&root).join("VivoKsu").join(SETTINGS_FILE))
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

    fn load(path: &Path) -> ToolPathSettings {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => ToolPathSettings::default(),
        }
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
