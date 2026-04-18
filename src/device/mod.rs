use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DeviceRegistry {
    devices: BTreeMap<String, Device>,
    #[serde(skip)]
    path: PathBuf,
}

impl DeviceRegistry {
    pub fn config_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .context("cannot determine config directory")?
            .join("clawcam");
        fs::create_dir_all(&config_dir)?;
        Ok(config_dir.join("devices.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        let mut registry = if path.exists() {
            let data = fs::read_to_string(&path)?;
            serde_json::from_str::<DeviceRegistry>(&data)
                .context("failed to parse device registry")?
        } else {
            DeviceRegistry::default()
        };
        registry.path = path;
        Ok(registry)
    }

    fn save(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(self)?;
        fs::write(&self.path, data)?;
        Ok(())
    }

    pub fn add(&mut self, name: &str, host: &str, port: u16, user: &str) -> Result<()> {
        validate_device_name(name)?;
        validate_host(host)?;
        validate_user(user)?;
        if self.devices.contains_key(name) {
            bail!("device '{name}' already exists");
        }
        self.devices.insert(
            name.to_string(),
            Device {
                name: name.to_string(),
                host: host.to_string(),
                port,
                user: user.to_string(),
            },
        );
        self.save()
    }

    pub fn remove(&mut self, name: &str) -> Result<()> {
        if self.devices.remove(name).is_none() {
            bail!("device '{name}' not found");
        }
        self.save()
    }

    pub fn get(&self, name: &str) -> Result<Device> {
        self.devices
            .get(name)
            .cloned()
            .context(format!("device '{name}' not found — run `clawcam device add` first"))
    }

    pub fn list(&self) -> Vec<&Device> {
        self.devices.values().collect()
    }
}

/// Device names must be alphanumeric with dashes or underscores, 1-64 chars.
fn validate_device_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        bail!("device name must be 1-64 characters");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("device name may only contain alphanumeric characters, dashes, and underscores");
    }
    Ok(())
}

/// Host must look like an IP address or hostname — no shell metacharacters.
fn validate_host(host: &str) -> Result<()> {
    if host.is_empty() || host.len() > 253 {
        bail!("host must be 1-253 characters");
    }
    if !host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == ':') {
        bail!("host contains invalid characters — expected IP address or hostname");
    }
    Ok(())
}

/// SSH user must be a valid Unix username — alphanumeric, dashes, underscores.
fn validate_user(user: &str) -> Result<()> {
    if user.is_empty() || user.len() > 32 {
        bail!("user must be 1-32 characters");
    }
    if !user.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("user contains invalid characters");
    }
    Ok(())
}
