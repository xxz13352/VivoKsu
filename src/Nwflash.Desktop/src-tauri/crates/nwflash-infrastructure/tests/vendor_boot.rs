use nwflash_infrastructure::resolve_vendor_boot_module_directories;

#[test]
fn keeps_the_official_modules_directory_and_only_safe_gki_directories() {
    let directories = resolve_vendor_boot_module_directories(
        "lib/modules/5.15.148-gki\nlib/modules/6.1.75-android14-gki\nlib/modules/6.1.75-gki;reboot\nlib/modules/6.1.75-gki/extra\n",
    );

    assert_eq!(
        directories,
        vec![
            "lib/modules".to_string(),
            "lib/modules/5.15.148-gki".to_string(),
            "lib/modules/6.1.75-android14-gki".to_string(),
            // `6.1.75-gki/extra` still names the GKI directory `6.1.75-gki`
            // (the WPF pattern treats the marker as a substring), but the
            // `;reboot` shell-injection attempt is rejected.
            "lib/modules/6.1.75-gki".to_string(),
        ]
    );
}

#[test]
fn tolerates_a_trailing_slash_on_the_gki_directory() {
    let directories = resolve_vendor_boot_module_directories(
        "lib/modules/6.1.75-android14-gki/\nlib/modules/5.15.148-gki\r\n",
    );

    assert_eq!(
        directories,
        vec![
            "lib/modules".to_string(),
            "lib/modules/6.1.75-android14-gki".to_string(),
            "lib/modules/5.15.148-gki".to_string(),
        ]
    );
}
