use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use zip::ZipArchive;

use crate::error::FastbootError;
use crate::sparse::{MappedSparseFile, SparseFragment, SPARSE_HEADER_MAGIC};   
   
pub trait ImageSource: Send + Sync {   
    fn read_file(&self, name: &str) -> io::Result<Vec<u8>>;   
    fn exists(&self, name: &str) -> bool;   
    fn list_files(&self) -> io::Result<Vec<String>>;   
    fn file_size(&self, name: &str) -> io::Result<u64>;   
    fn get_file_path(&self, name: &str) -> Option<PathBuf>;
}   
pub struct LocalImageSource {
    path: PathBuf,
}

impl LocalImageSource {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }   
    pub fn base_path(&self) -> &Path {
        &self.path
    }
}

impl ImageSource for LocalImageSource {
    fn read_file(&self, name: &str) -> io::Result<Vec<u8>> {
        let file_path = self.path.join(name);
        let mut file = File::open(&file_path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        Ok(buf)
    }

    fn exists(&self, name: &str) -> bool {
        self.path.join(name).exists()
    }

    fn list_files(&self) -> io::Result<Vec<String>> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(&self.path)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                if let Some(name) = entry.file_name().to_str() {
                    files.push(name.to_string());
                }
            }
        }
        Ok(files)
    }

    fn file_size(&self, name: &str) -> io::Result<u64> {
        let file_path = self.path.join(name);
        let metadata = std::fs::metadata(&file_path)?;
        Ok(metadata.len())
    }

    fn get_file_path(&self, name: &str) -> Option<PathBuf> {
        let path = self.path.join(name);
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }
}   
   
pub struct ZipImageSource<R: Read + Seek> {   
    archive: Arc<Mutex<ZipArchive<R>>>,   
    file_list: Vec<String>,
}

impl ZipImageSource<File> {   
    pub fn from_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = File::open(path.as_ref())?;
        Self::new(file)
    }
}

impl<R: Read + Seek> ZipImageSource<R> {   
    pub fn new(reader: R) -> io::Result<Self> {
        let mut archive = ZipArchive::new(reader).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("无效的 ZIP 文件: {}", e),
            )
        })?;   
        let file_list: Vec<String> = (0..archive.len())
            .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
            .collect();

        Ok(Self {
            archive: Arc::new(Mutex::new(archive)),
            file_list,
        })
    }   
    fn find_file_index(&self, name: &str) -> Option<usize> {
        for (i, file_name) in self.file_list.iter().enumerate() {   
            if file_name == name {
                return Some(i);
            }   
            if file_name.ends_with(&format!("/{}", name)) {
                return Some(i);
            }   
            if let Some(base_name) = file_name.rsplit('/').next() {
                if base_name == name {
                    return Some(i);
                }
            }
        }
        None
    }
}

impl<R: Read + Seek + Send + Sync> ImageSource for ZipImageSource<R> {
    fn read_file(&self, name: &str) -> io::Result<Vec<u8>> {
        let idx = self.find_file_index(name).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("ZIP 中找不到文件 {}", name),
            )
        })?;

        let mut archive = self.archive.lock();
        let mut file = archive.by_index(idx).map_err(|e| {
            io::Error::new(io::ErrorKind::NotFound, format!("读取 ZIP 文件失败: {}", e))
        })?;

        let mut buf = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut buf)?;
        Ok(buf)
    }

    fn exists(&self, name: &str) -> bool {
        self.find_file_index(name).is_some()
    }

    fn list_files(&self) -> io::Result<Vec<String>> {
        Ok(self.file_list.clone())
    }

    fn file_size(&self, name: &str) -> io::Result<u64> {
        let idx = self.find_file_index(name).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("ZIP 中找不到文件 {}", name),
            )
        })?;

        let mut archive = self.archive.lock();
        let file = archive.by_index(idx).map_err(|e| {
            io::Error::new(io::ErrorKind::NotFound, format!("读取 ZIP 文件失败: {}", e))
        })?;

        Ok(file.size())
    }

    fn get_file_path(&self, _name: &str) -> Option<PathBuf> {   
   
        None
    }
}   
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageType {   
    BootCritical,   
    Normal,   
    Extra,
}   
#[derive(Debug, Clone)]
pub struct Image {   
    pub nickname: String,   
    pub img_name: String,   
    pub part_name: String,   
    pub image_type: ImageType,   
    pub optional: bool,
}   
   
pub fn get_default_images() -> Vec<Image> {
    vec![   
        Image {
            nickname: "bootloader".to_string(),
            img_name: "bootloader.img".to_string(),
            part_name: "bootloader".to_string(),
            image_type: ImageType::BootCritical,
            optional: true,
        },
        Image {
            nickname: "radio".to_string(),
            img_name: "radio.img".to_string(),
            part_name: "radio".to_string(),
            image_type: ImageType::BootCritical,
            optional: true,
        },   
        Image {
            nickname: "boot".to_string(),
            img_name: "boot.img".to_string(),
            part_name: "boot".to_string(),
            image_type: ImageType::Normal,
            optional: false,
        },
        Image {
            nickname: "init_boot".to_string(),
            img_name: "init_boot.img".to_string(),
            part_name: "init_boot".to_string(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "vendor_boot".to_string(),
            img_name: "vendor_boot.img".to_string(),
            part_name: "vendor_boot".to_string(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "dtbo".to_string(),
            img_name: "dtbo.img".to_string(),
            part_name: "dtbo".to_string(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "vbmeta".to_string(),
            img_name: "vbmeta.img".to_string(),
            part_name: "vbmeta".to_string(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "vbmeta_system".to_string(),
            img_name: "vbmeta_system.img".to_string(),
            part_name: "vbmeta_system".to_string(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "vbmeta_vendor".to_string(),
            img_name: "vbmeta_vendor.img".to_string(),
            part_name: "vbmeta_vendor".to_string(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "super".to_string(),
            img_name: "super.img".to_string(),
            part_name: "super".to_string(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "system".to_string(),
            img_name: "system.img".to_string(),
            part_name: "system".to_string(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "system_ext".to_string(),
            img_name: "system_ext.img".to_string(),
            part_name: "system_ext".to_string(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "vendor".to_string(),
            img_name: "vendor.img".to_string(),
            part_name: "vendor".to_string(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "vendor_dlkm".to_string(),
            img_name: "vendor_dlkm.img".to_string(),
            part_name: "vendor_dlkm".to_string(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "odm".to_string(),
            img_name: "odm.img".to_string(),
            part_name: "odm".to_string(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "odm_dlkm".to_string(),
            img_name: "odm_dlkm.img".to_string(),
            part_name: "odm_dlkm".to_string(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "product".to_string(),
            img_name: "product.img".to_string(),
            part_name: "product".to_string(),
            image_type: ImageType::Normal,
            optional: true,
        },   
        Image {
            nickname: "userdata".to_string(),
            img_name: "userdata.img".to_string(),
            part_name: "userdata".to_string(),
            image_type: ImageType::Extra,
            optional: true,
        },
        Image {
            nickname: "cache".to_string(),
            img_name: "cache.img".to_string(),
            part_name: "cache".to_string(),
            image_type: ImageType::Extra,
            optional: true,
        },
    ]
}   
#[derive(Debug)]
pub struct FlashingPlan {   
    pub wants_wipe: bool,   
    pub skip_reboot: bool,   
    pub slot_override: Option<String>,   
    pub force_flash: bool,   
    pub sparse_limit: u64,
}

impl Default for FlashingPlan {
    fn default() -> Self {
        Self {
            wants_wipe: false,
            skip_reboot: false,
            slot_override: None,
            force_flash: false,
            sparse_limit: 0,
        }
    }
}   
   
   
   
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlashCommand {   
    Flash { partition: String, filename: String },   
    Erase { partition: String },   
    RebootBootloader,   
    RebootFastboot,   
    GetVar { name: String },   
    IfWipe,   
    EndIf,   
    UpdateSuper { partition: String },
}   
   
pub fn parse_fastboot_info(content: &str) -> Vec<FlashCommand> {
    let mut commands = Vec::new();

    for line in content.lines() {
        let line = line.trim();   
        if line.is_empty() || line.starts_with('#') {
            continue;
        }   
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        let cmd = match parts[0] {
            "flash" if parts.len() >= 3 => {   
   
                let (partition, filename) = if parts[1].starts_with("--") {   
                    let mut idx = 1;
                    while idx < parts.len() && parts[idx].starts_with("--") {
                        idx += 1;
                    }
                    if idx + 1 < parts.len() {
                        (parts[idx].to_string(), parts[idx + 1].to_string())
                    } else {
                        continue;
                    }
                } else {
                    (parts[1].to_string(), parts[2].to_string())
                };
                FlashCommand::Flash {
                    partition,
                    filename,
                }
            }
            "erase" if parts.len() >= 2 => FlashCommand::Erase {
                partition: parts[1].to_string(),
            },
            "reboot-bootloader" => FlashCommand::RebootBootloader,
            "reboot-fastboot" => FlashCommand::RebootFastboot,
            "getvar" if parts.len() >= 2 => FlashCommand::GetVar {
                name: parts[1].to_string(),
            },
            "if-wipe" => FlashCommand::IfWipe,
            "endif" => FlashCommand::EndIf,
            "update-super" if parts.len() >= 2 => FlashCommand::UpdateSuper {
                partition: parts[1].to_string(),
            },
            _ => continue,   
        };

        commands.push(cmd);
    }

    commands
}   
   
   
   
#[derive(Debug, Clone)]
pub struct FlashTask {   
    pub partition: String,   
    pub filename: String,   
    pub optional: bool,   
    pub needs_resparse: bool,
}   
   
pub struct FlashAllTool<S: ImageSource> {   
    source: S,   
    plan: FlashingPlan,   
    max_download_size: u64,   
    current_slot: Option<String>,   
    has_slot: bool,   
    is_logical: bool,
}

impl<S: ImageSource> FlashAllTool<S> {   
    pub fn new(source: S, plan: FlashingPlan) -> Self {
        Self {
            source,
            plan,
            max_download_size: 512 * 1024 * 1024,   
            current_slot: None,
            has_slot: false,
            is_logical: false,
        }
    }   
    pub fn set_device_info(
        &mut self,
        max_download_size: u64,
        current_slot: Option<String>,
        has_slot: bool,
        is_logical: bool,
    ) {
        self.max_download_size = max_download_size;
        self.current_slot = current_slot;
        self.has_slot = has_slot;
        self.is_logical = is_logical;
    }   
    pub fn source(&self) -> &S {
        &self.source
    }   
    pub fn plan(&self) -> &FlashingPlan {
        &self.plan
    }   
   
    pub fn generate_tasks(&self) -> io::Result<Vec<FlashTask>> {   
        if self.source.exists("fastboot-info.txt") {
            let content = self.source.read_file("fastboot-info.txt")?;
            let content = String::from_utf8_lossy(&content);
            return Ok(self.tasks_from_fastboot_info(&content));
        }   
        Ok(self.tasks_from_default_images())
    }   
    fn tasks_from_fastboot_info(&self, content: &str) -> Vec<FlashTask> {
        let commands = parse_fastboot_info(content);
        let mut tasks = Vec::new();
        let mut in_wipe_block = false;

        for cmd in commands {
            match cmd {
                FlashCommand::IfWipe => {
                    in_wipe_block = true;
                }
                FlashCommand::EndIf => {
                    in_wipe_block = false;
                }
                FlashCommand::Flash {
                    partition,
                    filename,
                } => {   
                    if in_wipe_block && !self.plan.wants_wipe {
                        continue;
                    }   
                    let partition = self.add_slot_suffix(&partition);

                    tasks.push(FlashTask {
                        partition,
                        filename,
                        optional: true,   
                        needs_resparse: false,   
                    });
                }   
                _ => {}
            }
        }   
        self.mark_resparse_tasks(&mut tasks);

        tasks
    }   
    fn tasks_from_default_images(&self) -> Vec<FlashTask> {
        let images = get_default_images();
        let mut tasks = Vec::new();

        for image in images {   
            if image.image_type == ImageType::Extra && !self.plan.wants_wipe {
                continue;
            }   
            if !self.source.exists(&image.img_name) {
                if image.optional {
                    continue;
                }   
            }   
            let partition = self.add_slot_suffix(&image.part_name);

            tasks.push(FlashTask {
                partition,
                filename: image.img_name,
                optional: image.optional,
                needs_resparse: false,
            });
        }   
        self.mark_resparse_tasks(&mut tasks);

        tasks
    }   
    fn add_slot_suffix(&self, partition: &str) -> String {   
        if let Some(ref slot) = self.plan.slot_override {
            return format!("{}_{}", partition, slot);
        }   
        if self.has_slot {
            if let Some(ref slot) = self.current_slot {
                return format!("{}_{}", partition, slot);
            }
        }

        partition.to_string()
    }   
    fn mark_resparse_tasks(&self, tasks: &mut [FlashTask]) {
        for task in tasks.iter_mut() {
            if let Ok(size) = self.source.file_size(&task.filename) {   
                if size > self.max_download_size {
                    task.needs_resparse = true;
                }
            }
        }
    }   
    pub fn is_sparse(&self, filename: &str) -> io::Result<bool> {   
        let data = self.source.read_file(filename)?;
        if data.len() < 4 {
            return Ok(false);
        }
        let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        Ok(magic == SPARSE_HEADER_MAGIC)
    }   
   
   
    pub fn read_image_data(&self, task: &FlashTask) -> io::Result<ImageData> {   
        if task.needs_resparse {
            if let Some(path) = self.source.get_file_path(&task.filename) {   
                if self.is_sparse(&task.filename)? {   
                    let mapped = MappedSparseFile::open(&path)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

                    let fragments = crate::sparse::resparse(&mapped, self.max_download_size)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

                    return Ok(ImageData::SparseFragments { path, fragments });
                }
            }
        }   
        let data = self.source.read_file(&task.filename)?;
        Ok(ImageData::Raw(data))
    }   
    pub fn get_partition_list(&self) -> io::Result<Vec<String>> {
        let tasks = self.generate_tasks()?;
        Ok(tasks.into_iter().map(|t| t.partition).collect())
    }   
    pub fn validate(&self) -> Result<(), FastbootError> {
        let tasks = self.generate_tasks()?;

        for task in tasks {
            if !task.optional && !self.source.exists(&task.filename) {
                return Err(FastbootError::ImageNotFound(task.filename));
            }
        }

        Ok(())
    }
}   
   
#[derive(Debug)]
pub enum ImageData {   
    Raw(Vec<u8>),   
    SparseFragments {   
        path: PathBuf,   
        fragments: Vec<SparseFragment>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use tempfile::TempDir;

    #[test]
    fn test_local_image_source() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.img");   
        let mut file = File::create(&test_file).unwrap();
        file.write_all(b"test data").unwrap();

        let source = LocalImageSource::new(temp_dir.path());   
        assert!(source.exists("test.img"));
        assert!(!source.exists("nonexistent.img"));   
        let data = source.read_file("test.img").unwrap();
        assert_eq!(data, b"test data");   
        let files = source.list_files().unwrap();
        assert!(files.contains(&"test.img".to_string()));   
        let size = source.file_size("test.img").unwrap();
        assert_eq!(size, 9);
    }

    #[test]
    fn test_default_images() {
        let images = get_default_images();   
        assert!(images.iter().any(|i| i.part_name == "boot"));
        assert!(images.iter().any(|i| i.part_name == "system"));
        assert!(images.iter().any(|i| i.part_name == "vendor"));   
        let boot = images.iter().find(|i| i.part_name == "boot").unwrap();
        assert!(!boot.optional);
    }

    #[test]
    fn test_zip_image_source() {   
        let mut zip_data = Vec::new();
        {
            let cursor = Cursor::new(&mut zip_data);
            let mut zip = zip::ZipWriter::new(cursor);

            let options = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);

            zip.start_file("boot.img", options).unwrap();
            zip.write_all(b"boot image data").unwrap();

            zip.start_file("images/system.img", options).unwrap();
            zip.write_all(b"system image data").unwrap();

            zip.finish().unwrap();
        }

        let cursor = Cursor::new(zip_data);
        let source = ZipImageSource::new(cursor).unwrap();   
        assert!(source.exists("boot.img"));
        assert!(source.exists("system.img"));   
        assert!(!source.exists("nonexistent.img"));   
        let data = source.read_file("boot.img").unwrap();
        assert_eq!(data, b"boot image data");   
        let data = source.read_file("system.img").unwrap();
        assert_eq!(data, b"system image data");   
        let files = source.list_files().unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn test_parse_fastboot_info_basic() {
        let content = r#"
# 这是注释
flash bootloader bootloader.img
flash boot boot.img
erase userdata
reboot-bootloader
"#;

        let commands = parse_fastboot_info(content);
        assert_eq!(commands.len(), 4);

        assert_eq!(
            commands[0],
            FlashCommand::Flash {
                partition: "bootloader".to_string(),
                filename: "bootloader.img".to_string(),
            }
        );
        assert_eq!(
            commands[1],
            FlashCommand::Flash {
                partition: "boot".to_string(),
                filename: "boot.img".to_string(),
            }
        );
        assert_eq!(
            commands[2],
            FlashCommand::Erase {
                partition: "userdata".to_string(),
            }
        );
        assert_eq!(commands[3], FlashCommand::RebootBootloader);
    }

    #[test]
    fn test_parse_fastboot_info_with_wipe() {
        let content = r#"
flash boot boot.img
if-wipe
erase userdata
flash userdata userdata.img
endif
flash system system.img
"#;

        let commands = parse_fastboot_info(content);
        assert_eq!(commands.len(), 6);

        assert_eq!(commands[1], FlashCommand::IfWipe);
        assert_eq!(commands[4], FlashCommand::EndIf);
    }

    #[test]
    fn test_flash_all_tool_generate_tasks() {
        let temp_dir = TempDir::new().unwrap();   
        File::create(temp_dir.path().join("boot.img")).unwrap();
        File::create(temp_dir.path().join("system.img")).unwrap();

        let source = LocalImageSource::new(temp_dir.path());
        let plan = FlashingPlan::default();
        let tool = FlashAllTool::new(source, plan);

        let tasks = tool.generate_tasks().unwrap();   
   
        assert!(tasks.iter().any(|t| t.filename == "boot.img"));
    }

    #[test]
    fn test_flash_all_tool_with_fastboot_info() {
        let temp_dir = TempDir::new().unwrap();   
        let mut info_file = File::create(temp_dir.path().join("fastboot-info.txt")).unwrap();
        info_file
            .write_all(b"flash boot boot.img\nflash system system.img\n")
            .unwrap();   
        File::create(temp_dir.path().join("boot.img")).unwrap();
        File::create(temp_dir.path().join("system.img")).unwrap();

        let source = LocalImageSource::new(temp_dir.path());
        let plan = FlashingPlan::default();
        let tool = FlashAllTool::new(source, plan);

        let tasks = tool.generate_tasks().unwrap();   
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].partition, "boot");
        assert_eq!(tasks[1].partition, "system");
    }

    #[test]
    fn test_flash_all_tool_slot_suffix() {
        let temp_dir = TempDir::new().unwrap();
        File::create(temp_dir.path().join("boot.img")).unwrap();

        let source = LocalImageSource::new(temp_dir.path());
        let plan = FlashingPlan {
            slot_override: Some("a".to_string()),
            ..Default::default()
        };
        let tool = FlashAllTool::new(source, plan);

        let tasks = tool.generate_tasks().unwrap();   
        let boot_task = tasks.iter().find(|t| t.filename == "boot.img").unwrap();
        assert_eq!(boot_task.partition, "boot_a");
    }

    #[test]
    fn test_flash_all_tool_validate() {
        let temp_dir = TempDir::new().unwrap();   
        let source = LocalImageSource::new(temp_dir.path());
        let plan = FlashingPlan::default();
        let tool = FlashAllTool::new(source, plan);   
        let result = tool.validate();
        assert!(result.is_err());
    }
}
