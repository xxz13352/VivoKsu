use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_infrastructure::{
    collect_payload_extraction_results, collect_required_payload_extraction_results,
    parse_payload_metadata, PayloadDumperCommand, PayloadDumperError,
};

#[test]
fn metadata_command_uses_the_fixed_payload_dumper_arguments() {
    let command = PayloadDumperCommand::metadata(
        r"C:\tools\payload_dumper.exe",
        "https://example.invalid/ota/payload.bin",
        r"C:\temp\metadata",
    )
    .expect("remote payload source should build a metadata command");

    assert_eq!(command.program, r"C:\tools\payload_dumper.exe");
    assert_eq!(
        command.args,
        vec![
            "https://example.invalid/ota/payload.bin".to_string(),
            "--metadata".to_string(),
            "-o".to_string(),
            r"C:\temp\metadata".to_string(),
            "--quiet".to_string(),
        ]
    );
}

#[test]
fn metadata_parser_projects_only_complete_partition_records() {
    let partitions = parse_payload_metadata(
        r#"{
          "partitions": [
            {"partition_name":"boot","size_in_bytes":2048,"compression_type":"none"},
            {"partition_name":"init_boot","size_in_bytes":1024},
            {"size_in_bytes":5,"compression_type":"brotli"}
          ]
        }"#,
    )
    .expect("metadata json should parse");

    assert_eq!(partitions.len(), 2);
    assert_eq!(partitions[0].name, "boot");
    assert_eq!(partitions[0].size_bytes, 2048);
    assert_eq!(partitions[0].compression_type, "none");
    assert_eq!(partitions[1].name, "init_boot");
    assert_eq!(partitions[1].compression_type, "none");
}

#[test]
fn extraction_command_and_results_use_only_selected_non_empty_images() {
    let command = PayloadDumperCommand::extract(
        r"C:\tools\payload_dumper.exe",
        r"C:\firmware\payload.bin",
        &["boot", "init_boot"],
        r"C:\temp\output",
    )
    .expect("selected partitions should build an extraction command");
    assert_eq!(
        command.args,
        vec![
            r"C:\firmware\payload.bin".to_string(),
            "-i".to_string(),
            "boot,init_boot".to_string(),
            "-o".to_string(),
            r"C:\temp\output".to_string(),
        ]
    );

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let output = std::env::temp_dir().join(format!("nwflash-payload-output-{nonce}"));
    fs::create_dir_all(&output).expect("output directory should be created");
    fs::write(output.join("boot.img"), b"boot").expect("boot image should be written");
    fs::write(output.join("init_boot.img"), []).expect("empty image should be written");

    let results = collect_payload_extraction_results(&output, &["boot", "init_boot"])
        .expect("existing output directory should be inspected");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].partition_name, "boot");
    assert_eq!(results[0].size_bytes, 4);
    assert_eq!(
        results[0].output_path,
        output.join("boot.img").to_string_lossy()
    );

    fs::remove_dir_all(output).expect("output directory should be removed");
}

#[test]
fn extraction_command_rejects_partition_names_that_could_escape_the_staging_directory() {
    for unsafe_name in [
        "../boot",
        "boot/vendor",
        "boot\\vendor",
        "C:\\boot",
        "/boot",
    ] {
        let error = PayloadDumperCommand::extract(
            r"C:\tools\payload_dumper.exe",
            r"C:\firmware\payload.bin",
            &[unsafe_name],
            r"C:\temp\output",
        )
        .expect_err("unsafe partition names must not reach payload_dumper");
        assert!(error.to_string().contains("分区名"));
    }
}

#[test]
fn required_results_reject_missing_or_empty_tool_outputs() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let output = std::env::temp_dir().join(format!("nwflash-payload-required-output-{nonce}"));
    fs::create_dir_all(&output).expect("output directory should be created");
    fs::write(output.join("boot.img"), []).expect("empty output should be written");

    let empty_error = collect_required_payload_extraction_results(&output, &["boot"])
        .expect_err("an empty output cannot satisfy a selected partition");
    assert!(matches!(empty_error, PayloadDumperError::MissingOutput(name) if name == "boot"));

    let missing_error = collect_required_payload_extraction_results(&output, &["vendor"])
        .expect_err("a missing output cannot satisfy a selected partition");
    assert!(matches!(missing_error, PayloadDumperError::MissingOutput(name) if name == "vendor"));

    fs::remove_dir_all(output).expect("output directory should be removed");
}

#[test]
fn metadata_parser_filters_partition_names_that_could_escape_extraction_staging() {
    let partitions = parse_payload_metadata(
        r#"{
          "partitions": [
            {"partition_name":"boot","size_in_bytes":4},
            {"partition_name":"../vendor","size_in_bytes":4},
            {"partition_name":"system/vendor","size_in_bytes":4},
            {"partition_name":"C:\\windows","size_in_bytes":4}
          ]
        }"#,
    )
    .expect("metadata json should parse");

    assert_eq!(
        partitions
            .iter()
            .map(|partition| partition.name.as_str())
            .collect::<Vec<_>>(),
        vec!["boot"]
    );
}
