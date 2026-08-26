use serde::Deserialize;
use std::collections::BTreeSet;
use std::sync::OnceLock;

const OFFICIAL_CATALOG: &str = include_str!("../catalog.yaml");

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
pub enum Platform {
    #[serde(rename = "linux/amd64", alias = "linux/x86_64")]
    LinuxAmd64,
    #[serde(rename = "linux/arm64", alias = "linux/aarch64")]
    LinuxArm64,
}

impl Platform {
    pub const ALL: [Self; 2] = [Self::LinuxAmd64, Self::LinuxArm64];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "linux/amd64" | "linux/x86_64" => Some(Self::LinuxAmd64),
            "linux/arm64" | "linux/aarch64" => Some(Self::LinuxArm64),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::LinuxAmd64 => "linux/amd64",
            Self::LinuxArm64 => "linux/arm64",
        }
    }

    pub fn from_uname(os: &str, arch: &str) -> Option<Self> {
        match (os.trim().to_ascii_lowercase().as_str(), arch.trim()) {
            ("linux", "x86_64" | "amd64") => Some(Self::LinuxAmd64),
            ("linux", "aarch64" | "arm64") => Some(Self::LinuxArm64),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub enum Archive {
    #[serde(rename = "binary")]
    Binary,
    #[serde(rename = "tar+gzip")]
    TarGz,
}

impl Archive {
    pub fn name(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::TarGz => "tar+gzip",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "binary" => Some(Self::Binary),
            "tar+gzip" => Some(Self::TarGz),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    pub version: String,
    pub replaces: String,
    pub description: String,
    pub artifacts: Vec<CatalogArtifact>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogArtifact {
    pub platform: Platform,
    pub url: String,
    pub sha256: String,
    pub archive: Archive,
}

#[derive(Clone, Debug)]
pub struct Artifact<'a> {
    pub tool: &'a str,
    pub version: &'a str,
    pub platform: Platform,
    pub url: &'a str,
    pub sha256: &'a str,
    pub archive: Archive,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema: u32,
    tools: Vec<Tool>,
}

fn official() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let catalog: Catalog = serde_yaml::from_str(OFFICIAL_CATALOG)
            .expect("embedded catalog.yaml must be valid YAML");
        validate(&catalog).expect("embedded catalog.yaml must satisfy the catalog schema");
        catalog
    })
}

fn validate(catalog: &Catalog) -> Result<(), String> {
    if catalog.schema != 1 {
        return Err(format!("unsupported catalog schema {}", catalog.schema));
    }
    let mut names = BTreeSet::new();
    for tool in &catalog.tools {
        if tool.name.is_empty()
            || !tool
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || tool.version.is_empty()
            || tool.replaces.is_empty()
            || tool.description.is_empty()
        {
            return Err(format!("tool {:?} has an empty required field", tool.name));
        }
        if !names.insert(&tool.name) {
            return Err(format!("duplicate tool {}", tool.name));
        }
        let mut platforms = BTreeSet::new();
        for artifact in &tool.artifacts {
            if !platforms.insert(artifact.platform) {
                return Err(format!(
                    "duplicate artifact for {} on {}",
                    tool.name,
                    artifact.platform.name()
                ));
            }
            if !artifact.url.starts_with("https://") {
                return Err(format!("{} has a non-HTTPS artifact URL", tool.name));
            }
            if artifact.sha256.len() != 64
                || !artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(format!("{} has an invalid SHA-256", tool.name));
            }
        }
        for platform in Platform::ALL {
            if !platforms.contains(&platform) {
                return Err(format!(
                    "{} is missing an artifact for {}",
                    tool.name,
                    platform.name()
                ));
            }
        }
    }
    Ok(())
}

pub fn tools() -> &'static [Tool] {
    &official().tools
}

pub fn replacement(tool: &str) -> &'static str {
    tools()
        .iter()
        .find(|entry| entry.name == tool)
        .map_or("-", |entry| entry.replaces.as_str())
}

pub fn description(tool: &str) -> &'static str {
    tools()
        .iter()
        .find(|entry| entry.name == tool)
        .map_or("custom tool", |entry| entry.description.as_str())
}

pub fn artifact(
    tool: &str,
    version: Option<&str>,
    platform: Platform,
) -> Option<Artifact<'static>> {
    let tool = tools().iter().find(|entry| {
        entry.name == tool && version.is_none_or(|requested| requested == entry.version)
    })?;
    let artifact = tool
        .artifacts
        .iter()
        .find(|entry| entry.platform == platform)?;
    Some(Artifact {
        tool: &tool.name,
        version: &tool.version,
        platform,
        url: &artifact.url,
        sha256: &artifact.sha256,
        archive: artifact.archive,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_linux_uname_variants() {
        assert_eq!(
            Platform::from_uname("Linux", "x86_64"),
            Some(Platform::LinuxAmd64)
        );
        assert_eq!(
            Platform::from_uname("linux", "aarch64"),
            Some(Platform::LinuxArm64)
        );
        assert_eq!(Platform::from_uname("Darwin", "arm64"), None);
    }

    #[test]
    fn embedded_catalog_is_valid_and_complete() {
        assert_eq!(official().schema, 1);
        for tool in tools() {
            for platform in Platform::ALL {
                assert!(
                    artifact(&tool.name, Some(&tool.version), platform).is_some(),
                    "missing {}@{} for {}",
                    tool.name,
                    tool.version,
                    platform.name()
                );
            }
        }
    }

    #[test]
    fn rejects_unknown_fields_and_bad_checksums() {
        let unknown = "schema: 1\ntools: []\nextra: true\n";
        assert!(serde_yaml::from_str::<Catalog>(unknown).is_err());
        let invalid: Catalog = serde_yaml::from_str(
            "schema: 1\ntools:\n  - name: x\n    version: '1'\n    replaces: '-'\n    description: x\n    artifacts:\n      - { platform: linux/amd64, url: 'http://x', sha256: bad, archive: binary }\n",
        )
        .unwrap();
        assert!(validate(&invalid).is_err());
    }
}
