use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

pub const DEFAULT_ENDPOINT: &str = "https://api.brainpod.io";

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub endpoint: Option<String>,
    pub api_key: Option<String>,
    pub pod: Option<String>,
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        if let Some(path) = std::env::var_os("BRAINPOD_CONFIG") {
            return Ok(PathBuf::from(path));
        }

        if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
            return Ok(PathBuf::from(path).join("brainpod/config.toml"));
        }

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("cannot locate config directory: HOME is not set"))?;
        Ok(home.join(".config/brainpod/config.toml"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read config {}", path.display()));
            }
        };

        toml::from_str(&contents)
            .with_context(|| format!("failed to parse config {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("config path has no parent: {}", path.display()))?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;

        let contents = toml::to_string_pretty(self).context("failed to serialize config")?;
        let temporary = path.with_extension("toml.tmp");
        fs::write(&temporary, contents)
            .with_context(|| format!("failed to write config {}", temporary.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("failed to secure config {}", temporary.display()))?;
        }

        fs::rename(&temporary, path)
            .with_context(|| format!("failed to replace config {}", path.display()))?;
        Ok(())
    }
}
