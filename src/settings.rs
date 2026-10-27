use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderFormat {
    #[default]
    Landscape,
    Portrait,
}

impl RenderFormat {
    pub fn id(self) -> &'static str {
        match self {
            Self::Landscape => "landscape",
            Self::Portrait => "portrait",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Landscape => "1280 × 720 · 16:9",
            Self::Portrait => "1080 × 1920 · 9:16",
        }
    }

    pub fn load(root: &Path) -> Self {
        let path = root.join("settings.json");
        let result = std::fs::read(&path).and_then(|bytes| {
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            match value["render_format"].as_str() {
                Some("landscape") => Ok(Self::Landscape),
                Some("portrait") => Ok(Self::Portrait),
                _ => Err(std::io::Error::other("Unknown render format")),
            }
        });
        match result {
            Ok(format) => format,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("Cannot load {}: {error}", path.display());
                }
                Self::default()
            }
        }
    }

    pub fn save(self, root: &Path) -> Result<(), String> {
        save_setting(root, "render_format", self.id())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderProfile {
    #[default]
    Hq,
    Distilled,
}

impl RenderProfile {
    pub fn id(self) -> &'static str {
        match self {
            Self::Hq => "hq",
            Self::Distilled => "distilled",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Hq => "Final (HQ)",
            Self::Distilled => "Preview (distilled)",
        }
    }

    pub fn load(root: &Path) -> Self {
        let path = root.join("settings.json");
        let result = std::fs::read(&path).and_then(|bytes| {
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            match value["render_profile"].as_str() {
                Some("hq") | None => Ok(Self::Hq),
                Some("distilled") => Ok(Self::Distilled),
                _ => Err(std::io::Error::other("Unknown render profile")),
            }
        });
        match result {
            Ok(profile) => profile,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("Cannot load {}: {error}", path.display());
                }
                Self::default()
            }
        }
    }

    pub fn save(self, root: &Path) -> Result<(), String> {
        save_setting(root, "render_profile", self.id())
    }
}

pub fn load_planning_budget(root: &Path) -> u64 {
    let path = root.join("settings.json");
    let result = std::fs::read(&path).and_then(|bytes| {
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        if value.get("planning_budget_seconds").is_none() {
            return Ok(600);
        }
        value["planning_budget_seconds"]
            .as_u64()
            .filter(|seconds| (60..=3600).contains(seconds))
            .ok_or_else(|| std::io::Error::other("Planning budget must be 60–3600 seconds"))
    });
    match result {
        Ok(seconds) => seconds,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!("Cannot load {}: {error}", path.display());
            }
            600
        }
    }
}

pub fn save_planning_budget(root: &Path, seconds: u64) -> Result<(), String> {
    if !(60..=3600).contains(&seconds) {
        return Err("Planning budget must be 1–60 minutes.".into());
    }
    save_setting(root, "planning_budget_seconds", seconds)
}

fn save_setting(root: &Path, key: &str, value: impl Into<serde_json::Value>) -> Result<(), String> {
    let path = root.join("settings.json");
    let mut settings = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)
            .map_err(|error| format!("Could not read settings: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(error) => return Err(format!("Could not read settings: {error}")),
    };
    let object = settings
        .as_object_mut()
        .ok_or_else(|| "Could not save settings: expected a JSON object.".to_owned())?;
    object.insert(key.to_owned(), value.into());
    let bytes = serde_json::to_vec_pretty(&settings).map_err(|error| error.to_string())?;
    let temporary = root.join(".settings.json.tmp");
    std::fs::write(&temporary, bytes)
        .and_then(|()| std::fs::rename(&temporary, path))
        .map_err(|error| format!("Could not save settings: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_format_defaults_to_landscape_and_persists_both_options() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(RenderFormat::load(root.path()), RenderFormat::Landscape);
        for format in [RenderFormat::Portrait, RenderFormat::Landscape] {
            format.save(root.path()).unwrap();
            assert_eq!(RenderFormat::load(root.path()), format);
        }
        std::fs::write(root.path().join("settings.json"), "{}").unwrap();
        assert_eq!(RenderFormat::load(root.path()), RenderFormat::Landscape);
    }
}
