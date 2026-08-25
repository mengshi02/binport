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
        "TOOL\tVERSION\tPLATFORMS\nrg\t15.2.0\tlinux/amd64\n"
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
        "resolve", "pack", "unpack", "pull", "push", "doctor", "warm", "plan", "watch",
    ] {
        assert!(help.contains(command), "help is missing {command}");
    }
}
