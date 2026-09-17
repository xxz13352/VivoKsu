use nwflash_domain::{OperationLogEntry, OperationLogLevel};

#[test]
fn display_level_uses_compact_chinese_labels() {
    let cases = [
        (OperationLogLevel::Info, "信息"),
        (OperationLogLevel::Success, "完成"),
        (OperationLogLevel::Warning, "注意"),
        (OperationLogLevel::Error, "失败"),
    ];

    for (level, expected) in cases {
        let entry = OperationLogEntry {
            timestamp_utc: 0,
            level,
            message: "test".to_string(),
            operation_id: None,
        };
        assert_eq!(entry.display_level(), expected);
    }
}
