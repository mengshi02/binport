#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    LinuxAmd64,
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

#[derive(Clone, Copy, Debug)]
pub enum Archive {
    Binary,
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

#[derive(Clone, Copy, Debug)]
pub struct Artifact {
    pub tool: &'static str,
    pub version: &'static str,
    pub platform: Platform,
    pub url: &'static str,
    pub sha256: &'static str,
    pub archive: Archive,
}

pub const TOOLS: &[(&str, &str)] = &[("rg", "15.2.0"), ("fd", "10.4.2"), ("jq", "1.8.2")];

pub fn artifact(tool: &str, version: Option<&str>, platform: Platform) -> Option<Artifact> {
    let entry = ARTIFACTS
        .iter()
        .find(|entry| entry.tool == tool && entry.platform == platform)?;
    if version.is_some_and(|requested| requested != entry.version) {
        return None;
    }
    Some(*entry)
}

const ARTIFACTS: &[Artifact] = &[
    Artifact {
        tool: "rg",
        version: "15.2.0",
        platform: Platform::LinuxAmd64,
        url: "https://github.com/BurntSushi/ripgrep/releases/download/15.2.0/ripgrep-15.2.0-x86_64-unknown-linux-musl.tar.gz",
        sha256: "33e15bcf1624b25cdd2a55813a47a2f95dbe126268203e76aa6a585d1e7b149c",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "rg",
        version: "15.2.0",
        platform: Platform::LinuxArm64,
        url: "https://github.com/BurntSushi/ripgrep/releases/download/15.2.0/ripgrep-15.2.0-aarch64-unknown-linux-musl.tar.gz",
        sha256: "800b1e7206afe799dfb5a6901f23147cfaabe0e52210538100f61e86e1740915",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "fd",
        version: "10.4.2",
        platform: Platform::LinuxAmd64,
        url: "https://github.com/sharkdp/fd/releases/download/v10.4.2/fd-v10.4.2-x86_64-unknown-linux-musl.tar.gz",
        sha256: "e3257d48e29a6be965187dbd24ce9af564e0fe67b3e73c9bdcd180f4ec11bdde",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "fd",
        version: "10.4.2",
        platform: Platform::LinuxArm64,
        url: "https://github.com/sharkdp/fd/releases/download/v10.4.2/fd-v10.4.2-aarch64-unknown-linux-musl.tar.gz",
        sha256: "f32d3657473fba74e2600babc8db0b93420d51169223b7e8143b2ed55d8fd9e8",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "jq",
        version: "1.8.2",
        platform: Platform::LinuxAmd64,
        url: "https://github.com/jqlang/jq/releases/download/jq-1.8.2/jq-linux-amd64",
        sha256: "b1c22172dd303f3be49e935aa56aa48a8b7a46e0bc838b4997d3bb451495870f",
        archive: Archive::Binary,
    },
    Artifact {
        tool: "jq",
        version: "1.8.2",
        platform: Platform::LinuxArm64,
        url: "https://github.com/jqlang/jq/releases/download/jq-1.8.2/jq-linux-arm64",
        sha256: "8b85c817833814ddca00a144c33705546355afccf0cf39b188f3cdb48b852309",
        archive: Archive::Binary,
    },
];

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
}
