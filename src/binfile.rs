use crate::catalog::Platform;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Eq, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct CopySpec {
    pub source: String,
    pub name: String,
    pub platform: Platform,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Binfile {
    pub platforms: Vec<Platform>,
    pub tools: Vec<ToolSpec>,
    pub copies: Vec<CopySpec>,
}

impl Binfile {
    pub fn read(path: &Path) -> io::Result<Self> {
        Self::parse(&fs::read_to_string(path)?)
    }

    pub fn parse(source: &str) -> io::Result<Self> {
        let mut platforms = Vec::new();
        let mut tools = Vec::new();
        let mut copies = Vec::new();
        for (index, raw) in source.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or_default().trim();
            if line.is_empty() {
                continue;
            }
            let (directive, value) = line
                .split_once(char::is_whitespace)
                .ok_or_else(|| invalid(index, "expected a directive followed by a value"))?;
            match directive.to_ascii_uppercase().as_str() {
                "TARGET" => {
                    let platform = Platform::parse(value.trim()).ok_or_else(|| {
                        invalid(index, &format!("unsupported target {:?}", value.trim()))
                    })?;
                    if !platforms.contains(&platform) {
                        platforms.push(platform);
                    }
                }
                "TOOL" => {
                    let value = value.trim();
                    let (name, version) = value
                        .split_once('@')
                        .map_or((value, None), |(name, version)| {
                            (name, Some(version.to_owned()))
                        });
                    if name.is_empty() {
                        return Err(invalid(index, "tool name cannot be empty"));
                    }
                    tools.push(ToolSpec {
                        name: name.to_owned(),
                        version,
                    });
                }
                "COPY" => {
                    let parts = value.split_whitespace().collect::<Vec<_>>();
                    if parts.len() != 4 || parts[2] != "--target" {
                        return Err(invalid(
                            index,
                            "expected COPY <source> <name> --target <platform>",
                        ));
                    }
                    let platform = Platform::parse(parts[3]).ok_or_else(|| {
                        invalid(index, &format!("unsupported target {:?}", parts[3]))
                    })?;
                    copies.push(CopySpec {
                        source: parts[0].to_owned(),
                        name: parts[1].to_owned(),
                        platform,
                    });
                }
                other => return Err(invalid(index, &format!("unknown directive {other}"))),
            }
        }
        if platforms.is_empty() {
            platforms.push(Platform::LinuxAmd64);
        }
        if tools.is_empty() && copies.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Binfile contains no TOOL or COPY entries",
            ));
        }
        Ok(Self {
            platforms,
            tools,
            copies,
        })
    }
}

fn invalid(line: usize, message: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("Binfile:{}: {message}", line + 1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_targets_and_tools() {
        let file =
            Binfile::parse("TARGET linux/amd64\nTARGET linux/arm64\nTOOL rg@15.2.0\nTOOL jq\n")
                .unwrap();
        assert_eq!(file.platforms, Platform::ALL);
        assert_eq!(
            file.tools[0],
            ToolSpec {
                name: "rg".into(),
                version: Some("15.2.0".into())
            }
        );
    }

    #[test]
    fn parses_a_platform_specific_copy() {
        let file = Binfile::parse(
            "COPY ./target/x86_64-unknown-linux-musl/release/logscan logscan --target linux/amd64\n",
        )
        .unwrap();
        assert_eq!(
            file.copies,
            vec![CopySpec {
                source: "./target/x86_64-unknown-linux-musl/release/logscan".into(),
                name: "logscan".into(),
                platform: Platform::LinuxAmd64,
            }]
        );
    }
}
