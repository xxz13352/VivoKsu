use std::collections::HashMap;
    use crate::error::FastbootError;   
    #[derive(Debug, Clone)]
pub struct PartitionInfo {   
    pub name: String,   
    pub size: u64,   
    pub is_logical: bool,   
    pub slot: Option<String>,
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
    #[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotInfo {   
    pub name: String,   
    pub is_active: bool,   
    pub is_bootable: bool,   
    pub is_successful: bool,
}   
    pub fn get_partition_name(base_name: &str, slot: Option<&str>) -> String {
    match slot {
        Some(s) => format!("{}_{}", base_name, s),
        None => base_name.to_string(),
    }
}   
    pub fn split_partition_name(full_name: &str) -> (&str, Option<&str>) {   
    if full_name.ends_with("_a") {
        (&full_name[..full_name.len() - 2], Some("a"))
    } else if full_name.ends_with("_b") {
        (&full_name[..full_name.len() - 2], Some("b"))
    } else {
        (full_name, None)
    }
}   
    pub fn other_slot(slot: &str) -> &'static str {
    match slot {
        "a" => "b",
        "b" => "a",
        _ => "a",   
    }
}   
    pub fn parse_partition_size(s: &str) -> Result<u64, FastbootError> {
    let s = s.trim();
        if s.starts_with("0x") || s.starts_with("0X") {
        u64::from_str_radix(&s[2..], 16)
    } else {
        s.parse()
    }
    .map_err(|_| FastbootError::Protocol(format!("无效的分区大小: {}", s)))
}   
    pub fn get_standard_images() -> Vec<Image> {
    vec![   
        Image {
            nickname: "bootloader".into(),
            img_name: "bootloader.img".into(),
            part_name: "bootloader".into(),
            image_type: ImageType::BootCritical,
            optional: true,
        },
        Image {
            nickname: "radio".into(),
            img_name: "radio.img".into(),
            part_name: "radio".into(),
            image_type: ImageType::BootCritical,
            optional: true,
        },   
        Image {
            nickname: "boot".into(),
            img_name: "boot.img".into(),
            part_name: "boot".into(),
            image_type: ImageType::Normal,
            optional: false,
        },
        Image {
            nickname: "init_boot".into(),
            img_name: "init_boot.img".into(),
            part_name: "init_boot".into(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "vendor_boot".into(),
            img_name: "vendor_boot.img".into(),
            part_name: "vendor_boot".into(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "recovery".into(),
            img_name: "recovery.img".into(),
            part_name: "recovery".into(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "system".into(),
            img_name: "system.img".into(),
            part_name: "system".into(),
            image_type: ImageType::Normal,
            optional: false,
        },
        Image {
            nickname: "system_ext".into(),
            img_name: "system_ext.img".into(),
            part_name: "system_ext".into(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "vendor".into(),
            img_name: "vendor.img".into(),
            part_name: "vendor".into(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "vendor_dlkm".into(),
            img_name: "vendor_dlkm.img".into(),
            part_name: "vendor_dlkm".into(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "odm".into(),
            img_name: "odm.img".into(),
            part_name: "odm".into(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "odm_dlkm".into(),
            img_name: "odm_dlkm.img".into(),
            part_name: "odm_dlkm".into(),
            image_type: ImageType::Normal,
            optional: true,
        },
        Image {
            nickname: "product".into(),
            img_name: "product.img".into(),
            part_name: "product".into(),
            image_type: ImageType::Normal,
            optional: true,
        },   
        Image {
            nickname: "vbmeta".into(),
            img_name: "vbmeta.img".into(),
            part_name: "vbmeta".into(),
            image_type: ImageType::Extra,
            optional: true,
        },
        Image {
            nickname: "vbmeta_system".into(),
            img_name: "vbmeta_system.img".into(),
            part_name: "vbmeta_system".into(),
            image_type: ImageType::Extra,
            optional: true,
        },
        Image {
            nickname: "vbmeta_vendor".into(),
            img_name: "vbmeta_vendor.img".into(),
            part_name: "vbmeta_vendor".into(),
            image_type: ImageType::Extra,
            optional: true,
        },
        Image {
            nickname: "dtbo".into(),
            img_name: "dtbo.img".into(),
            part_name: "dtbo".into(),
            image_type: ImageType::Extra,
            optional: true,
        },
    ]
}   
    #[derive(Debug, Clone)]
pub struct PartitionManager {   
    pub has_slot: bool,   
    pub current_slot: Option<String>,   
    pub slot_count: u32,   
    pub is_userspace: bool,   
    partitions: HashMap<String, PartitionInfo>,   
    pub super_partition_name: Option<String>,
}
    impl PartitionManager {   
    pub fn new() -> Self {
        Self {
            has_slot: false,
            current_slot: None,
            slot_count: 0,
            is_userspace: false,
            partitions: HashMap::new(),
            super_partition_name: None,
        }
    }   
        pub fn from_device_vars(vars: &HashMap<String, String>) -> Self {
        let mut mgr = Self::new();   
        if let Some(slot_count) = vars.get("slot-count") {
            mgr.slot_count = slot_count.parse().unwrap_or(0);
            mgr.has_slot = mgr.slot_count > 1;
        }   
        if let Some(slot) = vars.get("current-slot") {
            mgr.current_slot = Some(slot.clone());
        }   
        if let Some(is_userspace) = vars.get("is-userspace") {
            mgr.is_userspace = is_userspace == "yes" || is_userspace == "true";
        }   
        if let Some(super_name) = vars.get("super-partition-name") {
            mgr.super_partition_name = Some(super_name.clone());
        }   
        for (key, value) in vars {   
            if let Some(part_name) = key.strip_prefix("partition-size:") {
                let size = parse_partition_size(value).unwrap_or(0);
                let (base_name, slot) = split_partition_name(part_name);
                    let info = mgr
                    .partitions
                    .entry(part_name.to_string())
                    .or_insert(PartitionInfo {
                        name: part_name.to_string(),
                        size: 0,
                        is_logical: false,
                        slot: slot.map(|s| s.to_string()),
                    });
                info.size = size;
            }   
            if let Some(part_name) = key.strip_prefix("is-logical:") {
                let is_logical = value == "yes" || value == "true";
                let (_, slot) = split_partition_name(part_name);
                    let info = mgr
                    .partitions
                    .entry(part_name.to_string())
                    .or_insert(PartitionInfo {
                        name: part_name.to_string(),
                        size: 0,
                        is_logical: false,
                        slot: slot.map(|s| s.to_string()),
                    });
                info.is_logical = is_logical;
            }
        }
            mgr
    }   
        pub fn get_partition_name(&self, base_name: &str) -> String {   
        if base_name.ends_with("_a") || base_name.ends_with("_b") {
            return base_name.to_string();
        }   
        if self.has_slot {
            if let Some(ref slot) = self.current_slot {   
                let with_slot = format!("{}_{}", base_name, slot);
                if self.partitions.contains_key(&with_slot) {
                    return with_slot;
                }
            }
        }   
        base_name.to_string()
    }   
    pub fn get_partition(&self, name: &str) -> Option<&PartitionInfo> {
        let full_name = self.get_partition_name(name);
        self.partitions.get(&full_name)
    }   
    pub fn partition_exists(&self, name: &str) -> bool {
        let full_name = self.get_partition_name(name);
        self.partitions.contains_key(&full_name)
    }   
    pub fn is_logical_partition(&self, name: &str) -> bool {
        self.get_partition(name)
            .map(|p| p.is_logical)
            .unwrap_or(false)
    }   
    pub fn get_partition_size(&self, name: &str) -> Option<u64> {
        self.get_partition(name).map(|p| p.size)
    }   
    pub fn all_partitions(&self) -> Vec<&str> {
        self.partitions.keys().map(|s| s.as_str()).collect()
    }   
    pub fn logical_partitions(&self) -> Vec<&str> {
        self.partitions
            .iter()
            .filter(|(_, p)| p.is_logical)
            .map(|(name, _)| name.as_str())
            .collect()
    }   
    pub fn needs_fastbootd_for(&self, partition: &str) -> bool {   
        if self.is_userspace {
            return false;
        }   
        self.is_logical_partition(partition)
    }   
        pub fn get_target_slot(&self, specified: Option<&str>) -> Option<String> {
        if let Some(slot) = specified {
            return Some(slot.to_string());
        }   
        self.current_slot
            .as_ref()
            .map(|s| other_slot(s).to_string())
    }
}
    impl Default for PartitionManager {
    fn default() -> Self {
        Self::new()
    }
}   
    #[derive(Debug, Clone)]
pub struct FlashTask {   
    pub partition: String,   
    pub image_path: String,   
    pub is_logical: bool,   
    pub image_type: ImageType,   
    pub optional: bool,
}   
    #[derive(Debug, Clone)]
pub struct FlashPlan {   
    pub boot_critical: Vec<FlashTask>,   
    pub normal: Vec<FlashTask>,   
    pub extra: Vec<FlashTask>,   
    pub needs_fastbootd: bool,   
    pub target_slot: Option<String>,
}
    impl FlashPlan {   
    pub fn new() -> Self {
        Self {
            boot_critical: Vec::new(),
            normal: Vec::new(),
            extra: Vec::new(),
            needs_fastbootd: false,
            target_slot: None,
        }
    }   
    pub fn add_task(&mut self, task: FlashTask) {
        match task.image_type {
            ImageType::BootCritical => self.boot_critical.push(task),
            ImageType::Normal => self.normal.push(task),
            ImageType::Extra => self.extra.push(task),
        }
    }   
    pub fn all_tasks(&self) -> Vec<&FlashTask> {
        let mut tasks = Vec::new();
        tasks.extend(self.boot_critical.iter());
        tasks.extend(self.normal.iter());
        tasks.extend(self.extra.iter());
        tasks
    }   
    pub fn task_count(&self) -> usize {
        self.boot_critical.len() + self.normal.len() + self.extra.len()
    }
}
    impl Default for FlashPlan {
    fn default() -> Self {
        Self::new()
    }
}   
    #[cfg(test)]
mod tests {
    use super::*;
        #[test]
    fn test_get_partition_name_with_slot() {
        assert_eq!(get_partition_name("boot", Some("a")), "boot_a");
        assert_eq!(get_partition_name("boot", Some("b")), "boot_b");
        assert_eq!(get_partition_name("system", Some("a")), "system_a");
    }
        #[test]
    fn test_get_partition_name_without_slot() {
        assert_eq!(get_partition_name("boot", None), "boot");
        assert_eq!(get_partition_name("userdata", None), "userdata");
    }
        #[test]
    fn test_split_partition_name() {
        assert_eq!(split_partition_name("boot_a"), ("boot", Some("a")));
        assert_eq!(split_partition_name("boot_b"), ("boot", Some("b")));
        assert_eq!(split_partition_name("userdata"), ("userdata", None));
        assert_eq!(
            split_partition_name("system_ext_a"),
            ("system_ext", Some("a"))
        );
    }
        #[test]
    fn test_other_slot() {
        assert_eq!(other_slot("a"), "b");
        assert_eq!(other_slot("b"), "a");
    }
        #[test]
    fn test_parse_partition_size() {
        assert_eq!(parse_partition_size("1024").unwrap(), 1024);
        assert_eq!(parse_partition_size("0x400").unwrap(), 1024);
        assert_eq!(parse_partition_size("0X400").unwrap(), 1024);
        assert_eq!(parse_partition_size("  1024  ").unwrap(), 1024);
        assert!(parse_partition_size("invalid").is_err());
    }
        #[test]
    fn test_standard_images() {
        let images = get_standard_images();   
        assert!(images.iter().any(|i| i.part_name == "boot"));
        assert!(images.iter().any(|i| i.part_name == "system"));   
        let boot = images.iter().find(|i| i.part_name == "boot").unwrap();
        assert!(!boot.optional);
            let system = images.iter().find(|i| i.part_name == "system").unwrap();
        assert!(!system.optional);
    }
        #[test]
    fn test_image_type_ordering() {   
        let images = get_standard_images();
            let bootloader = images.iter().find(|i| i.part_name == "bootloader").unwrap();
        assert_eq!(bootloader.image_type, ImageType::BootCritical);
            let boot = images.iter().find(|i| i.part_name == "boot").unwrap();
        assert_eq!(boot.image_type, ImageType::Normal);
            let vbmeta = images.iter().find(|i| i.part_name == "vbmeta").unwrap();
        assert_eq!(vbmeta.image_type, ImageType::Extra);
    }
        #[test]
    fn test_partition_manager_new() {
        let mgr = PartitionManager::new();
        assert!(!mgr.has_slot);
        assert!(mgr.current_slot.is_none());
        assert_eq!(mgr.slot_count, 0);
        assert!(!mgr.is_userspace);
    }
        #[test]
    fn test_partition_manager_from_vars() {
        let mut vars = HashMap::new();
        vars.insert("slot-count".to_string(), "2".to_string());
        vars.insert("current-slot".to_string(), "a".to_string());
        vars.insert("is-userspace".to_string(), "yes".to_string());
        vars.insert("super-partition-name".to_string(), "super".to_string());
        vars.insert("partition-size:boot_a".to_string(), "0x4000000".to_string());
        vars.insert("partition-size:boot_b".to_string(), "0x4000000".to_string());
        vars.insert(
            "partition-size:system_a".to_string(),
            "0x80000000".to_string(),
        );
        vars.insert("is-logical:system_a".to_string(), "yes".to_string());
            let mgr = PartitionManager::from_device_vars(&vars);
            assert!(mgr.has_slot);
        assert_eq!(mgr.current_slot, Some("a".to_string()));
        assert_eq!(mgr.slot_count, 2);
        assert!(mgr.is_userspace);
        assert_eq!(mgr.super_partition_name, Some("super".to_string()));   
        assert!(mgr.partition_exists("boot_a"));
        assert!(mgr.partition_exists("system_a"));
        assert!(mgr.is_logical_partition("system_a"));
        assert!(!mgr.is_logical_partition("boot_a"));
    }
        #[test]
    fn test_partition_manager_get_partition_name() {
        let mut vars = HashMap::new();
        vars.insert("slot-count".to_string(), "2".to_string());
        vars.insert("current-slot".to_string(), "a".to_string());
        vars.insert("partition-size:boot_a".to_string(), "0x4000000".to_string());
        vars.insert("partition-size:boot_b".to_string(), "0x4000000".to_string());
        vars.insert(
            "partition-size:userdata".to_string(),
            "0x100000000".to_string(),
        );
            let mgr = PartitionManager::from_device_vars(&vars);   
        assert_eq!(mgr.get_partition_name("boot"), "boot_a");   
        assert_eq!(mgr.get_partition_name("userdata"), "userdata");   
        assert_eq!(mgr.get_partition_name("boot_b"), "boot_b");
    }
        #[test]
    fn test_flash_plan() {
        let mut plan = FlashPlan::new();
            plan.add_task(FlashTask {
            partition: "bootloader".to_string(),
            image_path: "bootloader.img".to_string(),
            is_logical: false,
            image_type: ImageType::BootCritical,
            optional: true,
        });
            plan.add_task(FlashTask {
            partition: "boot".to_string(),
            image_path: "boot.img".to_string(),
            is_logical: false,
            image_type: ImageType::Normal,
            optional: false,
        });
            plan.add_task(FlashTask {
            partition: "vbmeta".to_string(),
            image_path: "vbmeta.img".to_string(),
            is_logical: false,
            image_type: ImageType::Extra,
            optional: true,
        });
            assert_eq!(plan.task_count(), 3);
        assert_eq!(plan.boot_critical.len(), 1);
        assert_eq!(plan.normal.len(), 1);
        assert_eq!(plan.extra.len(), 1);   
        let tasks = plan.all_tasks();
        assert_eq!(tasks[0].partition, "bootloader");
        assert_eq!(tasks[1].partition, "boot");
        assert_eq!(tasks[2].partition, "vbmeta");
    }
}   
    #[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;   
    fn arb_partition_name() -> impl Strategy<Value = String> {
        prop::sample::select(vec![
            "boot",
            "system",
            "vendor",
            "product",
            "odm",
            "recovery",
            "dtbo",
            "vbmeta",
            "userdata",
            "cache",
            "system_ext",
            "vendor_dlkm",
            "odm_dlkm",
        ])
        .prop_map(|s| s.to_string())
    }   
    fn arb_slot() -> impl Strategy<Value = String> {
        prop::sample::select(vec!["a", "b"]).prop_map(|s| s.to_string())
    }
        proptest! {   
        #[test]
        fn prop_slot_suffix_roundtrip(
            base_name in arb_partition_name(),
            slot in arb_slot()
        ) {   
            let with_slot = get_partition_name(&base_name, Some(&slot));   
            let (extracted_base, extracted_slot) = split_partition_name(&with_slot);
                prop_assert_eq!(extracted_base, base_name.as_str());
            prop_assert_eq!(extracted_slot, Some(slot.as_str()));
        }   
        #[test]
        fn prop_no_slot_unchanged(base_name in arb_partition_name()) {   
            let without_slot = get_partition_name(&base_name, None);
            prop_assert_eq!(&without_slot, &base_name);   
            let (extracted_base, extracted_slot) = split_partition_name(&without_slot);
            prop_assert_eq!(extracted_base, base_name.as_str());
            prop_assert!(extracted_slot.is_none());
        }   
        #[test]
        fn prop_other_slot_involution(slot in arb_slot()) {   
            let other = other_slot(&slot);
            let back = other_slot(other);
            prop_assert_eq!(back, slot.as_str());
        }   
        #[test]
        fn prop_partition_size_hex_decimal(size in 1u64..0xFFFFFFFF) {
            let decimal = size.to_string();
            let hex = format!("0x{:x}", size);
            let hex_upper = format!("0X{:X}", size);
                let parsed_decimal = parse_partition_size(&decimal).unwrap();
            let parsed_hex = parse_partition_size(&hex).unwrap();
            let parsed_hex_upper = parse_partition_size(&hex_upper).unwrap();
                prop_assert_eq!(parsed_decimal, size);
            prop_assert_eq!(parsed_hex, size);
            prop_assert_eq!(parsed_hex_upper, size);
        }
    }
}
