use std::fs;
use std::process::Command;

#[test]
fn lists_a_binfile_with_the_short_top_level_command() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("Binfile"),
        "TARGET linux/amd64\nTOOL rg@15.2.0\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_binport"))
        .arg("ls")
        .arg(project.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "TOOL\tREPLACES\tDESCRIPTION\tVERSION\tPLATFORMS\nrg\tgrep\trecursive text search\t15.2.0\tlinux/amd64\n"
    );
}

#[test]
fn exposes_fleet_lifecycle_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_binport"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for command in [
        "auth", "bastion", "host", "resolve", "cp", "rm", "pack", "unpack", "pull", "push",
        "doctor", "warm", "plan", "watch",
    ] {
        assert!(help.contains(command), "help is missing {command}");
    }
}

#[test]
fn lists_bastion_presets() {
    let output = Command::new(env!("CARGO_BIN_EXE_binport"))
        .args(["bastion", "presets"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("h3c-iware-slash"));
    assert!(stdout.contains("{user}/{host}/{account}"));
    assert!(stdout.contains("deployment-verified"));
    assert!(stdout.contains("huawei-cbh-at"));
    assert!(stdout.contains("vendor-documented"));
    assert!(stdout.contains("jumpserver-koko-at"));
    assert!(stdout.contains("community-reported"));
    assert!(stdout.contains("oneidentity-sps-inband"));
    assert!(stdout.contains("wallix-bastion-shell"));
    assert!(stdout.contains("cyberark-psmp-at"));
}

#[test]
fn host_add_resolves_bastion_preset() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_binport"))
        .env("HOME", home.path())
        .args([
            "host",
            "add",
            "worker",
            "root@10.0.0.52",
            "--bastion",
            "192.0.2.10",
            "--bastion-user",
            "operator",
            "--bastion-account",
            "root",
            "--bastion-preset",
            "h3c-iware-slash",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config = fs::read_to_string(home.path().join(".ssh/binport_config")).unwrap();
    assert!(config.contains("BastionPreset h3c-iware-slash"));
    assert!(config.contains("BastionFormat {user}/{host}/{account}"));
}

#[test]
fn host_add_rejects_missing_preset_fields_and_config_injection() {
    let home = tempfile::tempdir().unwrap();
    let binport = env!("CARGO_BIN_EXE_binport");
    let missing = Command::new(binport)
        .env("HOME", home.path())
        .args([
            "host",
            "add",
            "worker",
            "root@10.0.0.52",
            "--bastion",
            "192.0.2.10",
            "--bastion-preset",
            "h3c-iware-slash",
        ])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--bastion-user"));

    let injection = Command::new(binport)
        .env("HOME", home.path())
        .args([
            "host",
            "add",
            "worker",
            "root@10.0.0.52",
            "--bastion",
            "192.0.2.10",
            "--bastion-user",
            "operator",
            "--bastion-account",
            "root",
            "--bastion-format",
            "{user}/{host}/{account}\nProxyCommand=bad",
        ])
        .output()
        .unwrap();
    assert!(!injection.status.success());
    assert!(String::from_utf8_lossy(&injection.stderr).contains("invalid bastion format"));
}

#[test]
fn bastion_probe_requires_a_bastion_route() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_binport"))
        .env("HOME", home.path())
        .args(["bastion", "probe", "root@192.0.2.1"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not configured with BastionProxy"));
}

#[test]
fn manages_a_host_and_proxy_jump_in_an_isolated_home() {
    let home = tempfile::tempdir().unwrap();
    let binport = env!("CARGO_BIN_EXE_binport");

    for arguments in [
        vec!["host", "add", "jump", "root@203.0.113.10"],
        vec!["host", "add", "app-01", "root@10.0.0.52", "--jump", "jump"],
    ] {
        let output = Command::new(binport)
            .env("HOME", home.path())
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let output = Command::new(binport)
        .env("HOME", home.path())
        .args(["host", "ls"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("jump\troot@203.0.113.10:22\tdirect"));
    assert!(stdout.contains("app-01\troot@10.0.0.52:22\tjump"));
}

#[test]
fn keeps_auth_and_host_subcommand_interfaces_visible() {
    let binport = env!("CARGO_BIN_EXE_binport");
    for (command, expected) in [
        ("auth", ["setup", "status", "remove"]),
        ("host", ["add", "show", "test"]),
    ] {
        let output = Command::new(binport)
            .args([command, "--help"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        for subcommand in expected {
            assert!(
                help.contains(subcommand),
                "{command} help is missing {subcommand}"
            );
        }
    }
}

#[test]
fn tunnel_rejects_port_zero() {
    let output = Command::new(env!("CARGO_BIN_EXE_binport"))
        .args(["tunnel", "0:localhost:8080", "nonexistent-host"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("greater than 0"),
        "expected port validation error, got: {stderr}"
    );
}
