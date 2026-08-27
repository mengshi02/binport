use std::io;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Preset {
    pub name: &'static str,
    pub format: &'static str,
    pub product: &'static str,
    pub status: &'static str,
    pub source: &'static str,
}

const PRESETS: &[Preset] = &[
    Preset {
        name: "h3c-iware-slash",
        format: "{user}/{host}/{account}",
        product: "H3C i-Ware",
        status: "deployment-verified",
        source: "binport deployment test",
    },
    Preset {
        name: "huawei-cbh-at",
        format: "{user}@{account}@{host}",
        product: "Huawei Cloud CBH",
        status: "vendor-documented",
        source: "https://support.huaweicloud.com/intl/en-us/usermanual-cbh/cbh_02_000302.html",
    },
    Preset {
        name: "jumpserver-koko-at",
        format: "{user}@{account}@{host}",
        product: "JumpServer/Koko",
        status: "community-reported",
        source: "https://github.com/anthropics/claude-code/issues/70461",
    },
    Preset {
        name: "oneidentity-sps-inband",
        format: "{account}@{host}",
        product: "One Identity SPS",
        status: "vendor-documented",
        source: "https://support.oneidentity.com/technical-documents/one-identity-safeguard-for-privileged-sessions/7.0%20lts/administration-guide/the-concepts-of-one-identity-safeguard-for-privileged-sessions-sps/connecting-to-a-server-through-one-identity-safeguard-for-privileged-sessions-sps",
    },
    Preset {
        name: "wallix-bastion-shell",
        format: "{account}@{host}:SSH:{user}",
        product: "WALLIX Bastion",
        status: "vendor-documented",
        source: "https://pam.wallix.one/documentation/user-doc/bastion_en_user_guide.pdf",
    },
    Preset {
        name: "cyberark-psmp-at",
        format: "{user}@{account}@{host}",
        product: "CyberArk PSMP",
        status: "community-reported",
        source: "https://www.reddit.com/r/CyberARk/comments/chp9s9/psmp_implementation/",
    },
];

pub fn presets() -> &'static [Preset] {
    PRESETS
}

pub fn find_preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.name == name)
}

pub fn resolve_format(preset: Option<&str>, custom: Option<&str>) -> io::Result<Option<String>> {
    match (preset, custom) {
        (Some(_), Some(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--bastion-preset and --bastion-format cannot be used together",
        )),
        (Some(name), None) => find_preset(name)
            .map(|preset| Some(preset.format.to_owned()))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "unknown bastion preset {name:?}; run `binport bastion presets` to list available presets"
                    ),
                )
            }),
        (None, Some(format)) => Ok(Some(format.to_owned())),
        (None, None) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_h3c_preset() {
        assert_eq!(
            resolve_format(Some("h3c-iware-slash"), None).unwrap(),
            Some("{user}/{host}/{account}".to_owned())
        );
    }

    #[test]
    fn rejects_unknown_preset_and_preserves_custom_format() {
        assert!(resolve_format(Some("unknown"), None).is_err());
        assert_eq!(
            resolve_format(None, Some("{user}@{host}")).unwrap(),
            Some("{user}@{host}".to_owned())
        );
    }
}
