use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    Sftp,
    Ftp,
    Ftps,
}

impl Protocol {
    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Sftp => 22,
            Protocol::Ftp => 21,
            Protocol::Ftps => 21,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Protocol::Sftp => "SFTP (SSH)",
            Protocol::Ftp => "FTP",
            Protocol::Ftps => "FTPS (Explicit TLS)",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Protocol::Sftp => Protocol::Ftp,
            Protocol::Ftp => Protocol::Ftps,
            Protocol::Ftps => Protocol::Sftp,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Protocol::Sftp => Protocol::Ftps,
            Protocol::Ftp => Protocol::Sftp,
            Protocol::Ftps => Protocol::Ftp,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthKind {
    Password,
    KeyFile,
    SshAgent,
}

impl AuthKind {
    pub fn label(self) -> &'static str {
        match self {
            AuthKind::Password => "Password",
            AuthKind::KeyFile => "SSH Key File",
            AuthKind::SshAgent => "SSH Agent",
        }
    }

    pub fn next(self, protocol: Protocol) -> Self {
        match protocol {
            Protocol::Sftp => match self {
                AuthKind::Password => AuthKind::KeyFile,
                AuthKind::KeyFile => AuthKind::SshAgent,
                AuthKind::SshAgent => AuthKind::Password,
            },
            Protocol::Ftp | Protocol::Ftps => AuthKind::Password,
        }
    }

    pub fn prev(self, protocol: Protocol) -> Self {
        match protocol {
            Protocol::Sftp => match self {
                AuthKind::Password => AuthKind::SshAgent,
                AuthKind::KeyFile => AuthKind::Password,
                AuthKind::SshAgent => AuthKind::KeyFile,
            },
            Protocol::Ftp | Protocol::Ftps => AuthKind::Password,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_kind: AuthKind,
    /// Password (for Password auth) or key passphrase (for KeyFile auth).
    /// Stored only when `save_secret` is true.
    #[serde(default)]
    pub secret: String,
    #[serde(default)]
    pub save_secret: bool,
    #[serde(default)]
    pub key_path: String,
    #[serde(default)]
    pub remote_dir: String,
    #[serde(default)]
    pub local_dir: String,
}

impl ConnectionProfile {
    pub fn new_default() -> Self {
        let default_key = dirs::home_dir()
            .map(|h| h.join(".ssh").join("id_ed25519"))
            .filter(|p| p.exists())
            .or_else(|| {
                dirs::home_dir()
                    .map(|h| h.join(".ssh").join("id_rsa"))
                    .filter(|p| p.exists())
            })
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "~/.ssh/id_ed25519".to_string());

        Self {
            id: Uuid::new_v4().to_string(),
            name: String::new(),
            protocol: Protocol::Sftp,
            host: String::new(),
            port: 22,
            username: whoami_fallback(),
            auth_kind: AuthKind::KeyFile,
            secret: String::new(),
            save_secret: false,
            key_path: default_key,
            remote_dir: String::new(),
            local_dir: String::new(),
        }
    }

    pub fn sanitized_for_storage(&self) -> Self {
        let mut copy = self.clone();
        if !copy.save_secret {
            copy.secret.clear();
        }
        copy
    }
}

fn whoami_fallback() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(default)]
    pub connections: Vec<ConnectionProfile>,
}

pub fn config_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("kftp").join("connections.toml")
}

pub fn load_connections() -> Vec<ConnectionProfile> {
    let path = config_path();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    toml::from_str::<ConfigFile>(&raw)
        .map(|c| c.connections)
        .unwrap_or_default()
}

pub fn save_connections(connections: &[ConnectionProfile]) -> anyhow::Result<PathBuf> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let stored = ConfigFile {
        connections: connections
            .iter()
            .map(ConnectionProfile::sanitized_for_storage)
            .collect(),
    };
    let serialized = toml::to_string_pretty(&stored)?;
    std::fs::write(&path, serialized)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(stripped);
    }
    if path == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_sanitization() {
        let mut p = ConnectionProfile::new_default();
        p.name = "prod".into();
        p.host = "example.com".into();
        p.secret = "super-secret".into();
        p.save_secret = false;

        let sanitized = p.sanitized_for_storage();
        assert!(sanitized.secret.is_empty());

        p.save_secret = true;
        let kept = p.sanitized_for_storage();
        assert_eq!(kept.secret, "super-secret");
    }

    #[test]
    fn test_toml_roundtrip() {
        let mut p = ConnectionProfile::new_default();
        p.name = "SFTP Key".into();
        p.protocol = Protocol::Sftp;
        p.host = "10.0.0.1".into();
        p.port = 2222;
        p.username = "deploy".into();
        p.auth_kind = AuthKind::KeyFile;
        p.key_path = "~/.ssh/id_ed25519".into();
        p.remote_dir = "/var/www".into();

        let cfg = ConfigFile {
            connections: vec![p],
        };
        let s = toml::to_string_pretty(&cfg).unwrap();
        let parsed: ConfigFile = toml::from_str(&s).unwrap();
        assert_eq!(parsed.connections.len(), 1);
        assert_eq!(parsed.connections[0].port, 2222);
        assert_eq!(parsed.connections[0].auth_kind, AuthKind::KeyFile);
        assert_eq!(parsed.connections[0].key_path, "~/.ssh/id_ed25519");
    }
}
