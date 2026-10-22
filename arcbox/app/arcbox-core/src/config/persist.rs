//! Writing settings back to the user's `config.toml`.
//!
//! The daemon reads configuration from several files (see
//! [`user_config_paths`](super::user_config_paths)); a setting changed at
//! runtime is written to the documented one, `~/.config/arcbox/config.toml`
//! (or `$XDG_CONFIG_HOME/arcbox/config.toml`), which is also the one merged
//! last and so wins over every other file. The file is edited in place with
//! `toml_edit`, so an operator's comments and formatting survive.

use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, Value};

use crate::error::{CoreError, Result};

/// Sets `[vm] cpus` and `[vm] memory_mb` in the config file at `path`,
/// creating the file (and its directory) when it does not exist yet.
///
/// # Errors
///
/// Returns an error if the file exists but is not valid TOML, or cannot be
/// read or replaced atomically.
pub fn set_vm_resources(path: &Path, cpus: u32, memory_mb: u64) -> Result<()> {
    let memory_mb = i64::try_from(memory_mb).map_err(|_| {
        CoreError::config(format!(
            "memory_mb {memory_mb} does not fit in a TOML integer"
        ))
    })?;
    edit(path, |doc| {
        let vm = table(doc, path, "vm", "the System VM's resources")?;
        set_integer(vm, "cpus", i64::from(cpus));
        set_integer(vm, "memory_mb", memory_mb);
        Ok(())
    })
}

/// Sets `[machine] default_machine` in the config file at `path`, or
/// removes the key for `None`; creates the file when it does not exist yet.
///
/// # Errors
///
/// Returns an error if the file exists but is not valid TOML, or cannot be
/// read or replaced atomically.
pub fn set_default_machine(path: &Path, name: Option<&str>) -> Result<()> {
    edit(path, |doc| {
        let machine = table(doc, path, "machine", "the default machine")?;
        match name {
            Some(name) => match machine
                .get_mut("default_machine")
                .and_then(Item::as_value_mut)
            {
                Some(existing) => {
                    let decor = existing.decor().clone();
                    *existing = Value::from(name);
                    *existing.decor_mut() = decor;
                }
                None => machine["default_machine"] = Item::Value(Value::from(name)),
            },
            None => {
                machine.remove("default_machine");
            }
        }
        Ok(())
    })
}

/// Writes the egress proxy preferences to `[network]`.
///
/// Sets `proxy`, `proxy_exclude`, `udp` and `icmp`. A custom proxy URL is
/// written without its password: secrets belong in the credential store.
///
/// # Errors
///
/// Returns an error if the file exists but is not valid TOML, or cannot be
/// read or replaced atomically.
pub fn set_network_proxy(
    path: &Path,
    preferences: &crate::network_proxy::ProxyPreferences,
) -> Result<()> {
    set_network_proxy_impl(path, preferences, None)
}

/// Atomically stores the default, saved proxies, and machine references.
pub fn set_proxy_library(
    path: &Path,
    preferences: &crate::network_proxy::ProxyPreferences,
    library: &crate::network_proxy::library::ProxyLibrary,
) -> Result<()> {
    set_network_proxy_impl(path, preferences, Some(library))
}

fn set_network_proxy_impl(
    path: &Path,
    preferences: &crate::network_proxy::ProxyPreferences,
    library: Option<&crate::network_proxy::library::ProxyLibrary>,
) -> Result<()> {
    use arcbox_fakeip::proxy_state::{IcmpPolicy, UdpPolicy};

    let udp = match preferences.udp {
        UdpPolicy::Proxy => "proxy",
        UdpPolicy::Direct => "direct",
        UdpPolicy::Block => "block",
    };
    let icmp = match preferences.icmp {
        IcmpPolicy::Direct => "direct",
        IcmpPolicy::Block => "block",
    };
    edit(path, |doc| {
        let network = table(doc, path, "network", "the egress proxy")?;
        // `ProxyPolicy`'s Display omits any password.
        set_value(
            network,
            "proxy",
            Value::from(preferences.policy.to_string()),
        );
        let exclude: toml_edit::Array = preferences.exclude.iter().map(String::as_str).collect();
        set_value(network, "proxy_exclude", Value::Array(exclude));
        set_value(network, "udp", Value::from(udp));
        set_value(network, "icmp", Value::from(icmp));
        if let Some(library) = library {
            let serialized = toml::to_string(library).map_err(|e| {
                CoreError::config(format!("could not serialize proxy library: {e}"))
            })?;
            network["proxy_library"] = serialized
                .parse::<DocumentMut>()
                .map_err(|e| CoreError::config(format!("could not format proxy library: {e}")))?
                .into_item();
        }
        Ok(())
    })
}

/// Loads `path` (an absent file is an empty document), applies `change`,
/// and replaces the file atomically.
fn edit(path: &Path, change: impl FnOnce(&mut DocumentMut) -> Result<()>) -> Result<()> {
    let mut doc = match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse::<DocumentMut>()
            .map_err(|e| CoreError::config(format!("{} is not valid TOML: {e}", path.display())))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DocumentMut::new(),
        Err(e) => {
            return Err(CoreError::config(format!(
                "failed to read {}: {e}",
                path.display()
            )));
        }
    };

    change(&mut doc)?;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    arcbox_atomic_file::write(path, doc.to_string().as_bytes())
        .map_err(|e| CoreError::config(format!("failed to write {}: {e}", path.display())))?;
    Ok(())
}

/// The `[name]` table of `doc`, created when absent.
fn table<'doc>(
    doc: &'doc mut DocumentMut,
    path: &Path,
    name: &str,
    stores: &str,
) -> Result<&'doc mut Table> {
    doc.entry(name)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| {
            CoreError::config(format!(
                "{}: `{name}` is not a table; cannot store {stores}",
                path.display()
            ))
        })
}

/// Sets `key` to `n`, keeping the comment and spacing around an existing
/// value rather than replacing the whole entry.
fn set_integer(table: &mut Table, key: &str, n: i64) {
    set_value(table, key, Value::from(n));
}

/// Sets `key` to `value`, keeping the decor of an existing value.
fn set_value(table: &mut Table, key: &str, value: Value) {
    match table.get_mut(key).and_then(Item::as_value_mut) {
        Some(existing) => {
            let decor = existing.decor().clone();
            *existing = value;
            *existing.decor_mut() = decor;
        }
        None => {
            table[key] = Item::Value(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_the_file_and_keeps_unrelated_content_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");

        set_vm_resources(&path, 4, 4096).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = written.parse().unwrap();
        assert_eq!(parsed["vm"]["cpus"].as_integer(), Some(4));
        assert_eq!(parsed["vm"]["memory_mb"].as_integer(), Some(4096));

        std::fs::write(
            &path,
            "# my settings\n[network]\nproxy = \"none\" # keep\n\n[vm]\ncpus = 2 # old\nautostart = true\n",
        )
        .unwrap();
        set_vm_resources(&path, 6, 8192).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.starts_with("# my settings\n"), "{written}");
        assert!(written.contains("proxy = \"none\" # keep"), "{written}");
        assert!(written.contains("cpus = 6 # old"), "{written}");
        assert!(written.contains("autostart = true"), "{written}");
        assert!(written.contains("memory_mb = 8192"), "{written}");
    }

    #[test]
    fn the_default_machine_is_set_kept_and_cleared_next_to_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[machine]\ndisk_gb = 20 # small\n").unwrap();

        set_default_machine(&path, Some("dev")).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("disk_gb = 20 # small"), "{written}");
        assert!(written.contains("default_machine = \"dev\""), "{written}");

        set_default_machine(&path, Some("ci")).unwrap();
        let parsed: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(parsed["machine"]["default_machine"].as_str(), Some("ci"));

        set_default_machine(&path, None).unwrap();
        let parsed: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert!(parsed["machine"].get("default_machine").is_none());
        assert_eq!(parsed["machine"]["disk_gb"].as_integer(), Some(20));

        // Clearing what is not there creates no file.
        let absent = dir.path().join("none.toml");
        set_default_machine(&absent, None).unwrap();
        assert!(
            absent.exists(),
            "the file is written even when nothing changed"
        );
    }

    #[test]
    fn network_proxy_is_written_without_its_password_and_keeps_comments() {
        use crate::network_proxy::ProxyPreferences;
        use arcbox_fakeip::proxy_state::{IcmpPolicy, UdpPolicy};
        use figment::providers::Format;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[network]\nmtu = 1400 # jumbo off\nproxy = \"system\" # was\n",
        )
        .unwrap();

        let preferences = ProxyPreferences {
            policy: "socks5://alice:do-not-leak@10.0.0.2:1080".parse().unwrap(),
            exclude: vec![".corp.example".into()],
            udp: UdpPolicy::Block,
            icmp: IcmpPolicy::Direct,
        };
        set_network_proxy(&path, &preferences).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("do-not-leak"), "{written}");
        assert!(written.contains("mtu = 1400 # jumbo off"), "{written}");
        assert!(
            written.contains("proxy = \"socks5://alice@10.0.0.2:1080\" # was"),
            "{written}"
        );

        let config: crate::Config = figment::Figment::new()
            .merge(figment::providers::Serialized::defaults(
                crate::Config::default(),
            ))
            .merge(figment::providers::Toml::file(&path))
            .extract()
            .unwrap();
        assert_eq!(
            config.network.proxy.to_string(),
            "socks5://alice@10.0.0.2:1080"
        );
        assert_eq!(config.network.proxy_exclude, vec![".corp.example"]);
        assert_eq!(config.network.udp, UdpPolicy::Block);
        assert_eq!(config.network.icmp, IcmpPolicy::Direct);
    }

    #[test]
    fn refuses_a_broken_file_rather_than_overwriting_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[vm\ncpus = 2\n").unwrap();
        assert!(set_vm_resources(&path, 4, 4096).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[vm\ncpus = 2\n");
    }
}
