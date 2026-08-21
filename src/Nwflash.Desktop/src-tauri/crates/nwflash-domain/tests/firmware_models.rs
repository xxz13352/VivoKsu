use nwflash_domain::{FirmwarePackageInspection, PayloadPartitionEntry};

#[test]
fn firmware_package_manages_only_known_images() {
    let inspection = FirmwarePackageInspection {
        package_path: "fake.zip".to_string(),
        package_name: "fake".to_string(),
        entry_count: 5,
        image_entries: vec![
            "images/boot.img".to_string(),
            "images/init_boot.img".to_string(),
            "images/vendor_boot.img".to_string(),
            "images/super.img".to_string(),
            "images/lk.img".to_string(),
        ],
    };

    assert_eq!(
        inspection.managed_image_entries(),
        vec![
            "images/boot.img".to_string(),
            "images/init_boot.img".to_string(),
            "images/vendor_boot.img".to_string(),
            "images/lk.img".to_string()
        ]
    );
}

#[test]
fn payload_partition_entry_sizes_are_formatted_like_csharp() {
    let entries = [
        PayloadPartitionEntry {
            name: "small".to_string(),
            size_bytes: 512,
            compression_type: "store".to_string(),
        },
        PayloadPartitionEntry {
            name: "kb".to_string(),
            size_bytes: 1024,
            compression_type: "store".to_string(),
        },
        PayloadPartitionEntry {
            name: "mb".to_string(),
            size_bytes: 4 * 1024 * 1024,
            compression_type: "store".to_string(),
        },
        PayloadPartitionEntry {
            name: "gb".to_string(),
            size_bytes: 5 * 1024 * 1024 * 1024,
            compression_type: "store".to_string(),
        },
    ];

    assert_eq!(entries[0].size_text(), "512 B");
    assert_eq!(entries[1].size_text(), "1.0 KB");
    assert_eq!(entries[2].size_text(), "4.0 MB");
    assert_eq!(entries[3].size_text(), "5.00 GB");
}
