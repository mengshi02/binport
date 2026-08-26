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
        "auth", "host", "resolve", "cp", "rm", "pack", "unpack", "pull", "push", "doctor", "warm",
        "plan", "watch",
    ] {
        assert!(help.contains(command), "help is missing {command}");
    }
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
