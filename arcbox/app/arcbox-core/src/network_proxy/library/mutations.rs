use super::*;

impl NetworkProxy {
    /// Creates or edits a profile. Credentials are isolated by immutable ID,
    /// even when two saved proxies share the same host and user name.
    pub fn save_proxy(
        &self,
        id: &str,
        name: &str,
        mut preferences: ProxyPreferences,
        mut password: PasswordChange,
    ) -> Result<SavedProxyView> {
        let mut current = self.lock();
        let mut library = self.proxy_library();
        let name = name.trim();
        if name.is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
            return Err(CoreError::config(
                "proxy name must contain 1–80 bytes without control characters",
            ));
        }
        if library
            .profiles
            .iter()
            .any(|p| p.id != id && p.name.eq_ignore_ascii_case(name))
        {
            return Err(CoreError::config("a saved proxy already has this name"));
        }
        let id = if id.is_empty() {
            if library.profiles.len() >= 128 {
                return Err(CoreError::config("at most 128 proxies can be saved"));
            }
            uuid::Uuid::new_v4().to_string()
        } else {
            find(&library, id)?;
            id.to_string()
        };
        let ProxyPolicy::Custom(url) = &mut preferences.policy else {
            return Err(CoreError::config("a saved proxy requires a SOCKS5 URL"));
        };
        if let Some(auth) = url.server.auth.clone() {
            if let Some(inline) = auth.password()
                && matches!(password, PasswordChange::Keep)
            {
                password = PasswordChange::Set(Zeroizing::new(inline.to_string()));
            }
            url.server.auth = Some(Arc::new(
                ProxyAuth::new(auth.username().to_string(), None).map_err(CoreError::config)?,
            ));
        } else {
            password = PasswordChange::Clear;
        }
        let profile = SavedProxy {
            id: id.clone(),
            name: name.to_string(),
            url: preferences.policy.to_string(),
            exclude: preferences.exclude.clone(),
            udp: preferences.udp,
            icmp: preferences.icmp,
        };
        profile.preferences()?;
        if matches!(password, PasswordChange::Keep)
            && let Some(original) = library.profiles.iter().find(|p| p.id == id)
            && credential_account(&original.preferences()?.policy)
                != credential_account(&preferences.policy)
        {
            return Err(CoreError::config(
                "enter a password for the changed proxy account",
            ));
        }
        let key = account(&id);
        let previous = self.credentials.get(&key)?;
        let secret = match &password {
            PasswordChange::Keep => previous.as_deref().map(String::as_str),
            PasswordChange::Set(value) => Some(value.as_str()),
            PasswordChange::Clear => None,
        };
        let environment = resolve(
            &preferences,
            &*self.credentials,
            secret.map_or(Secret::Cleared, Secret::Given),
        )?;
        let resolved = snapshot(&preferences, environment.clone());
        let selected = library.default_profile == id;
        library.profiles.retain(|p| p.id != id);
        library.profiles.push(profile.clone());
        library
            .profiles
            .sort_by_key(|p| p.name.to_ascii_lowercase());
        let default = if selected { &preferences } else { &current };
        match &password {
            PasswordChange::Keep => {}
            PasswordChange::Set(value) => self.credentials.set(&key, value)?,
            PasswordChange::Clear => self.credentials.delete(&key)?,
        }
        if let Err(error) =
            crate::config::persist::set_proxy_library(&self.config_path, default, &library)
        {
            self.restore_secret(&key, previous.as_deref().map(String::as_str));
            return Err(error);
        }
        *self.library.write().expect("proxy library lock poisoned") = library.clone();
        if selected {
            *current = preferences.clone();
            self.publish(&preferences, environment);
        }
        for (machine, choice) in &library.machines {
            if *choice == id {
                self.routes.set_machine(machine, Some(resolved.clone()));
            }
        }
        self.profile_view(&profile, &library)
    }

    pub fn remove_proxy(&self, id: &str) -> Result<()> {
        let current = self.lock();
        let mut library = self.proxy_library();
        find(&library, id)?;
        if library.default_profile == id || library.machines.values().any(|choice| choice == id) {
            return Err(CoreError::config(
                "proxy is in use; change the default or machine assignments before deleting it",
            ));
        }
        let key = account(id);
        let previous = self.credentials.get(&key)?;
        library.profiles.retain(|p| p.id != id);
        self.credentials.delete(&key)?;
        if let Err(error) =
            crate::config::persist::set_proxy_library(&self.config_path, &current, &library)
        {
            self.restore_secret(&key, previous.as_deref().map(String::as_str));
            return Err(error);
        }
        *self.library.write().expect("proxy library lock poisoned") = library;
        Ok(())
    }

    pub fn select_default_proxy(&self, id: &str) -> Result<ProxySettingsView> {
        let mut current = self.lock();
        let mut library = self.proxy_library();
        let profile = find(&library, id)?;
        let preferences = profile.preferences()?;
        let environment = self.resolve_profile(profile, None)?;
        library.default_profile = id.to_string();
        crate::config::persist::set_proxy_library(&self.config_path, &preferences, &library)?;
        *self.library.write().expect("proxy library lock poisoned") = library;
        *current = preferences.clone();
        self.publish(&preferences, environment);
        Ok(self.view(preferences))
    }

    /// Changes only new connections of the selected machine; no VM restart.
    pub fn set_machine_proxy(&self, machine: &str, choice: &str) -> Result<()> {
        if machine.is_empty()
            || machine == "default"
            || machine.len() > 128
            || machine.chars().any(char::is_control)
        {
            return Err(CoreError::config(
                "select a Linux machine, not the System VM",
            ));
        }
        let current = self.lock();
        let mut library = self.proxy_library();
        let route = match choice {
            "" => None,
            "direct" => Some(direct()),
            id => {
                let profile = find(&library, id)?;
                Some(snapshot(
                    &profile.preferences()?,
                    self.resolve_profile(profile, None)?,
                ))
            }
        };
        if choice.is_empty() {
            library.machines.remove(machine);
        } else {
            library
                .machines
                .insert(machine.to_string(), choice.to_string());
        }
        crate::config::persist::set_proxy_library(&self.config_path, &current, &library)?;
        *self.library.write().expect("proxy library lock poisoned") = library;
        self.routes.set_machine(machine, route);
        Ok(())
    }

    fn restore_secret(&self, key: &str, value: Option<&str>) {
        let result = match value {
            Some(value) => self.credentials.set(key, value),
            None => self.credentials.delete(key),
        };
        if let Err(error) = result {
            tracing::error!(%error, "could not restore previous saved proxy credential");
        }
    }
}
