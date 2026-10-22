//! What makes a machine distinguishable from every other one: a NIC address
//! from a real vendor's range, its own `/etc/machine-id`, and the time zone
//! and locale it presents.
//!
//! An isolated machine gets the first two when it is created or isolated,
//! and a clone of one gets fresh ones, so no two machines share them. The
//! guest applies the cmdline tokens built here on every boot
//! (`arcbox-agent`'s `machine_identity`).

use arcbox_constants::cmdline::{MACHINE_ID_KEY, MACHINE_LOCALE_KEY, MACHINE_TIMEZONE_KEY};

use crate::error::{EngineError, Result};

/// Organizationally unique identifiers of common Ethernet controllers (Intel
/// and Realtek). A NIC address from one of them carries no hint of being
/// generated, unlike a locally administered `02:…` one.
const VENDOR_OUIS: [[u8; 3]; 4] = [
    [0x00, 0x1B, 0x21],
    [0x3C, 0xFD, 0xFE],
    [0xA0, 0x36, 0x9F],
    [0x00, 0xE0, 0x4C],
];

/// A random unicast address under one of [`VENDOR_OUIS`].
#[must_use]
pub fn vendor_mac() -> String {
    let random = uuid::Uuid::new_v4();
    let bytes = random.as_bytes();
    let oui = VENDOR_OUIS[usize::from(bytes[0]) % VENDOR_OUIS.len()];
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        oui[0], oui[1], oui[2], bytes[1], bytes[2], bytes[3]
    )
}

/// A fresh `/etc/machine-id`: 32 lowercase hex digits.
#[must_use]
pub fn new_machine_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Checks a time zone name: `Area/City` path segments of letters, digits,
/// `_`, `-` and `+`, so it can name a file under `/usr/share/zoneinfo` and
/// nothing outside it, and sit in the kernel cmdline.
///
/// # Errors
///
/// Returns an error naming the problem.
pub fn validate_timezone(zone: &str) -> Result<()> {
    let ok = !zone.is_empty()
        && zone.len() <= 64
        && zone.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+'))
        });
    if ok {
        Ok(())
    } else {
        Err(EngineError::config(format!(
            "'{zone}' is not a time zone name like Europe/Berlin"
        )))
    }
}

/// Checks a locale name such as `en_US.UTF-8` or `de_DE@euro`.
///
/// # Errors
///
/// Returns an error naming the problem.
pub fn validate_locale(locale: &str) -> Result<()> {
    let ok = !locale.is_empty()
        && locale.len() <= 64
        && locale
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '@'));
    if ok {
        Ok(())
    } else {
        Err(EngineError::config(format!(
            "'{locale}' is not a locale name like en_US.UTF-8"
        )))
    }
}

/// `cmdline` with the `key` token set to `value`, or removed for `None`.
#[must_use]
pub fn cmdline_with_token(cmdline: &str, key: &str, value: Option<&str>) -> String {
    let mut tokens: Vec<String> = cmdline
        .split_whitespace()
        .filter(|token| !token.starts_with(key))
        .map(str::to_owned)
        .collect();
    if let Some(value) = value {
        tokens.push(format!("{key}{value}"));
    }
    tokens.join(" ")
}

/// `cmdline` carrying exactly the identity the machine has.
#[must_use]
pub fn cmdline_with_identity(
    cmdline: &str,
    machine_id: Option<&str>,
    timezone: Option<&str>,
    locale: Option<&str>,
) -> String {
    let cmdline = cmdline_with_token(cmdline, MACHINE_ID_KEY, machine_id);
    let cmdline = cmdline_with_token(&cmdline, MACHINE_TIMEZONE_KEY, timezone);
    cmdline_with_token(&cmdline, MACHINE_LOCALE_KEY, locale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_macs_are_unicast_and_globally_administered() {
        for _ in 0..64 {
            let mac = vendor_mac();
            let first = u8::from_str_radix(&mac[..2], 16).unwrap();
            assert_eq!(first & 0x03, 0, "{mac}: unicast, not locally administered");
            assert!(
                VENDOR_OUIS
                    .iter()
                    .any(|oui| mac
                        .starts_with(&format!("{:02x}:{:02x}:{:02x}", oui[0], oui[1], oui[2]))),
                "{mac}"
            );
            assert_eq!(mac.len(), 17);
        }
        assert_ne!(vendor_mac(), vendor_mac());
    }

    #[test]
    fn machine_ids_are_32_hex_digits_and_unique() {
        let id = new_machine_id();
        assert_eq!(id.len(), 32);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(id, new_machine_id());
    }

    #[test]
    fn names_that_could_escape_or_break_the_cmdline_are_refused() {
        for good in [
            "Europe/Berlin",
            "America/Argentina/Buenos_Aires",
            "UTC",
            "Etc/GMT+8",
        ] {
            assert!(validate_timezone(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "../etc/passwd",
            "Europe/../x",
            "Asia/Shang hai",
            "/UTC",
            "UTC/",
        ] {
            assert!(validate_timezone(bad).is_err(), "{bad}");
        }
        for good in ["en_US.UTF-8", "zh_CN.UTF-8", "de_DE@euro", "C"] {
            assert!(validate_locale(good).is_ok(), "{good}");
        }
        for bad in ["", "en US", "en_US.UTF-8 init=/bin/sh", "a=b"] {
            assert!(validate_locale(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn identity_tokens_are_set_replaced_and_removed() {
        let base = "console=hvc0 arcbox.machine_name=box";
        let with = cmdline_with_identity(base, Some("ab"), Some("Asia/Tokyo"), Some("ja_JP.UTF-8"));
        assert_eq!(
            with,
            "console=hvc0 arcbox.machine_name=box arcbox.machine_id=ab \
             arcbox.timezone=Asia/Tokyo arcbox.locale=ja_JP.UTF-8"
        );
        let replaced = cmdline_with_identity(&with, Some("cd"), None, Some("en_US.UTF-8"));
        assert_eq!(
            replaced,
            "console=hvc0 arcbox.machine_name=box arcbox.machine_id=cd arcbox.locale=en_US.UTF-8"
        );
        assert_eq!(cmdline_with_identity(&replaced, None, None, None), base);
    }
}
