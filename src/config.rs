// Two files live in the config dir:
//   settings.json - account + theme (key only if "remember me" is on, 0600 on unix)
//   clawrun.json  - process + snippet definitions. Only secret *paths* go in
//                   here, never values, so it's fine to commit. You can also
//                   point the app at a project file: `clawrun ./clawrun.json`

use crate::api::{DEFAULT_BASE_URL, KeyKind};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    pub base_url: String,
    pub kind: KeyKind,
    pub agent_id: String,
    pub dark: bool,
    pub seed: [u8; 3],
    pub remember: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    /// how often to check for rotated secrets, seconds
    pub rotation_check_secs: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            kind: KeyKind::User,
            agent_id: String::new(),
            dark: true,
            seed: [0x00, 0x89, 0x7B], // teal, so it doesn't look exactly like the explorer
            remember: false,
            api_key: String::new(),
            rotation_check_secs: 30,
        }
    }
}

/// `DATABASE_URL <- production/db/url`
#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
pub struct SecretRef {
    pub env: String,
    pub vault: String,
    pub path: String,
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct ProcDef {
    pub id: String,
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub env: Vec<EnvVar>,
    pub secrets: Vec<SecretRef>,
    pub autorestart: bool,
    pub restart_on_rotation: bool,
    pub autostart: bool,
}

impl Default for ProcDef {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: String::new(),
            command: String::new(),
            cwd: String::new(),
            env: vec![],
            secrets: vec![],
            autorestart: true,
            restart_on_rotation: true,
            autostart: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct SnippetDef {
    pub id: String,
    pub name: String,
    pub interpreter: String,
    pub code: String,
    pub cwd: String,
    pub secrets: Vec<SecretRef>,
}

impl Default for SnippetDef {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: "untitled".into(),
            interpreter: "sh".into(),
            code: String::new(),
            cwd: String::new(),
            secrets: vec![],
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Project {
    pub processes: Vec<ProcDef>,
    pub snippets: Vec<SnippetDef>,
}

pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("{:x}{:x}", t.as_millis(), N.fetch_add(1, Ordering::Relaxed))
}

fn dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("co", "1claw", "clawrun").map(|d| d.config_dir().to_path_buf())
}

pub fn default_project_path() -> PathBuf {
    dir().map(|d| d.join("clawrun.json")).unwrap_or_else(|| PathBuf::from("clawrun.json"))
}

fn load<T: for<'de> Deserialize<'de> + Default>(p: &Path) -> T {
    std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save<T: Serialize>(p: &Path, v: &T, private: bool) {
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(s) = serde_json::to_string_pretty(v) {
        let _ = write(p, s.as_bytes(), private);
    }
}

impl Settings {
    pub fn load() -> Self {
        dir().map(|d| load(&d.join("settings.json"))).unwrap_or_default()
    }
    pub fn save(&self) {
        if let Some(d) = dir() {
            save(&d.join("settings.json"), self, true);
        }
    }
}

impl Project {
    pub fn load(p: &Path) -> Self {
        load(p)
    }
    pub fn save(&self, p: &Path) {
        save(p, self, false);
    }
}

#[cfg(unix)]
fn write(p: &Path, data: &[u8], private: bool) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mode = if private { 0o600 } else { 0o644 };
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(p)?;
    if private {
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(data)
}

#[cfg(not(unix))]
fn write(p: &Path, data: &[u8], _private: bool) -> std::io::Result<()> {
    std::fs::write(p, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_never_has_values() {
        let mut pr = Project::default();
        pr.processes.push(ProcDef {
            name: "api".into(),
            secrets: vec![SecretRef { env: "DB".into(), vault: "prod".into(), path: "db/url".into() }],
            ..Default::default()
        });
        let s = serde_json::to_string(&pr).unwrap();
        assert!(s.contains("db/url"));
        let back: Project = serde_json::from_str(&s).unwrap();
        assert_eq!(back.processes[0].secrets, pr.processes[0].secrets);
    }

    #[test]
    fn ids_are_unique() {
        assert_ne!(new_id(), new_id());
    }
}
