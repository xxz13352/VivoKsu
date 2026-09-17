use std::collections::HashSet;

const OFFICIAL_MODULE_DIRECTORY: &str = "lib/modules";

pub fn resolve_vendor_boot_module_directories(listing: &str) -> Vec<String> {
    let mut directories = vec![OFFICIAL_MODULE_DIRECTORY.to_string()];
    let mut seen = HashSet::from([OFFICIAL_MODULE_DIRECTORY.to_string()]);

    for line in listing.lines().map(str::trim) {
        let Some(rest) = line.strip_prefix("lib/modules/") else {
            continue;
        };
        // The `magiskboot cpio "ls /lib/modules/"` listing may render a GKI
        // directory with a trailing '/' or extra path columns, so treat the
        // marker as a substring (the WPF `GkiDirectoryPattern`) but require it
        // to be a directory terminator ('/' or end-of-line), so a shell
        // metacharacter like `;reboot` after `-gki` is still rejected.
        let Some((version, tail)) = rest.split_once("-gki") else {
            continue;
        };
        if !tail.is_empty() && !tail.starts_with('/') {
            continue;
        }
        if version.is_empty()
            || !version.starts_with(|character: char| character.is_ascii_digit())
            || !version.chars().all(is_safe_gki_version_character)
        {
            continue;
        }

        let directory = format!("lib/modules/{version}-gki");
        if seen.insert(directory.clone()) {
            directories.push(directory);
        }
    }

    directories
}

fn is_safe_gki_version_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '+' | '-')
}
