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

pub const TOOLS: &[(&str, &str)] = &[
    ("rg", "15.2.0"),
    ("fd", "10.4.2"),
    ("jq", "1.8.2"),
    ("eza", "0.23.5"),
    ("bat", "0.26.1"),
    ("dust", "1.2.5"),
    ("btm", "0.14.8"),
    ("sd", "1.1.0"),
    ("delta", "0.19.2"),
    ("micro", "2.0.15"),
];

pub fn replacement(tool: &str) -> &'static str {
    match tool {
        "rg" => "grep",
        "fd" => "find",
        "eza" => "ls",
        "bat" => "cat",
        "dust" => "du",
        "btm" => "top",
        "sd" => "sed",
        "delta" => "diff",
        "micro" => "edit",
        _ => "-",
    }
}

pub fn description(tool: &str) -> &'static str {
    match tool {
        "rg" => "recursive text search",
        "fd" => "filesystem search",
        "jq" => "JSON processor",
        "eza" => "directory listing",
        "bat" => "file viewer",
        "dust" => "disk usage",
        "btm" => "system monitor",
        "sd" => "find and replace",
        "delta" => "syntax-aware diff",
        "micro" => "terminal editor",
        _ => "custom tool",
    }
}

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
    Artifact {
        tool: "eza",
        version: "0.23.5",
        platform: Platform::LinuxAmd64,
        url: "https://github.com/eza-community/eza/releases/download/v0.23.5/eza_x86_64-unknown-linux-musl.tar.gz",
        sha256: "e06eebab74b73d6b7d51a796a353824b001bea82df077706382e100815d28904",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "eza",
        version: "0.23.5",
        platform: Platform::LinuxArm64,
        url: "https://github.com/eza-community/eza/releases/download/v0.23.5/eza_aarch64-unknown-linux-gnu_no_libgit.tar.gz",
        sha256: "1c01b578b5bd3f23b7de5a4b41936cde20fb16ff16a03e63266317ac1eb821e0",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "bat",
        version: "0.26.1",
        platform: Platform::LinuxAmd64,
        url: "https://github.com/sharkdp/bat/releases/download/v0.26.1/bat-v0.26.1-x86_64-unknown-linux-musl.tar.gz",
        sha256: "0dcd8ac79732c0d5b136f11f4ee00e581440e16a44eab5b3105b611bbf2cf191",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "bat",
        version: "0.26.1",
        platform: Platform::LinuxArm64,
        url: "https://github.com/sharkdp/bat/releases/download/v0.26.1/bat-v0.26.1-aarch64-unknown-linux-musl.tar.gz",
        sha256: "6369242c584065f195fb20cb36fbd7cb63ae690605bbe89868a7596b596c2c23",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "dust",
        version: "1.2.5",
        platform: Platform::LinuxAmd64,
        url: "https://github.com/bootandy/dust/releases/download/v1.2.5/dust-v1.2.5-x86_64-unknown-linux-musl.tar.gz",
        sha256: "79813b5743fab1e04c1d9c34042aab865dbe09efb76719e7c7d260568850fabc",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "dust",
        version: "1.2.5",
        platform: Platform::LinuxArm64,
        url: "https://github.com/bootandy/dust/releases/download/v1.2.5/dust-v1.2.5-aarch64-unknown-linux-musl.tar.gz",
        sha256: "331b328233a70e56f0509ff962b2a6ea606eb2e654ce8a0e9a3bebe1bd54a2be",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "btm",
        version: "0.14.8",
        platform: Platform::LinuxAmd64,
        url: "https://github.com/ClementTsang/bottom/releases/download/0.14.8/bottom_x86_64-unknown-linux-musl.tar.gz",
        sha256: "9d071c11a5b5bf266f05aa6519a43ad353330155d3d5627a750572d85ed19f54",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "btm",
        version: "0.14.8",
        platform: Platform::LinuxArm64,
        url: "https://github.com/ClementTsang/bottom/releases/download/0.14.8/bottom_aarch64-unknown-linux-musl.tar.gz",
        sha256: "cc6ad0527598fb45eff929ac168e0c93eb7c3cd604723876b9df9b487ac10a47",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "sd",
        version: "1.1.0",
        platform: Platform::LinuxAmd64,
        url: "https://github.com/chmln/sd/releases/download/v1.1.0/sd-v1.1.0-x86_64-unknown-linux-musl.tar.gz",
        sha256: "02f00f4777d43e8e95b7b8d49e1a0d6e502fed4b8e79c1c8b8063857a30caa2e",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "sd",
        version: "1.1.0",
        platform: Platform::LinuxArm64,
        url: "https://github.com/chmln/sd/releases/download/v1.1.0/sd-v1.1.0-aarch64-unknown-linux-musl.tar.gz",
        sha256: "ec8c93c0533ff21f4851d11566808d4082544baf063d9b96ea77c27e98b7cd99",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "delta",
        version: "0.19.2",
        platform: Platform::LinuxAmd64,
        url: "https://github.com/dandavison/delta/releases/download/0.19.2/delta-0.19.2-x86_64-unknown-linux-musl.tar.gz",
        sha256: "f1ea01ca7728ce3462debc359f39dfc7cbbc1a63224b71fefabf92042864aa1b",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "delta",
        version: "0.19.2",
        platform: Platform::LinuxArm64,
        url: "https://github.com/dandavison/delta/releases/download/0.19.2/delta-0.19.2-aarch64-unknown-linux-gnu.tar.gz",
        sha256: "0bfce159a5cddd5feb3d6db4a616d883ff51253ce08ac7ec11cb1d208cfaab9e",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "micro",
        version: "2.0.15",
        platform: Platform::LinuxAmd64,
        url: "https://github.com/micro-editor/micro/releases/download/v2.0.15/micro-2.0.15-linux64-static.tar.gz",
        sha256: "267d238eac1e26ed053d13d4d48bd421b87f9eb538b604f0b2f74a85598b6cc2",
        archive: Archive::TarGz,
    },
    Artifact {
        tool: "micro",
        version: "2.0.15",
        platform: Platform::LinuxArm64,
        url: "https://github.com/micro-editor/micro/releases/download/v2.0.15/micro-2.0.15-linux-arm64.tar.gz",
        sha256: "5ca127857bf5500be3879f1a70b27556e737a49da04a1be5334de9e8e8781ad9",
        archive: Archive::TarGz,
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

    #[test]
    fn every_curated_tool_has_an_artifact_for_each_target() {
        for (tool, version) in TOOLS {
            for platform in Platform::ALL {
                assert!(
                    artifact(tool, Some(version), platform).is_some(),
                    "missing {tool}@{version} for {}",
                    platform.name()
                );
            }
        }
    }
}
