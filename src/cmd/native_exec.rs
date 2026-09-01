use binport::hop::ExecHop;
use binport::ssh::{Destination, NativeSsh};
use clap::Args;
use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct ExecArgs {
    /// SSH host configured in binport or ~/.ssh/config
    target: String,
    /// Remote command and arguments
    #[arg(required = true, trailing_var_arg = true)]
    command: Vec<OsString>,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// SSH host configured in binport or ~/.ssh/config
    target: String,
    /// Local script file, or - to read it from stdin
    script: PathBuf,
    /// Remote interpreter
    #[arg(long, default_value = "sh")]
    interpreter: String,
    /// Arguments passed to the script
    #[arg(trailing_var_arg = true)]
    arguments: Vec<OsString>,
}

pub fn exec(args: ExecArgs, use_password: bool, tty: bool, json: bool) -> io::Result<u8> {
    if tty && json {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--tty and --json cannot be used together",
        ));
    }
    let executable = args.command.first().expect("clap requires a command");
    let executable = executable
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "command is not valid UTF-8"))?;
    let command = binport::execute_command(executable, &args.command[1..])?;
    let input = if io::stdin().is_terminal() {
        Vec::new()
    } else {
        let mut input = Vec::new();
        io::stdin().read_to_end(&mut input)?;
        input
    };
    execute(args.target, command, input, use_password, tty, json)
}

pub fn run(args: RunArgs, use_password: bool, json: bool) -> io::Result<u8> {
    let script = if args.script == std::path::Path::new("-") {
        let mut script = Vec::new();
        io::stdin().read_to_end(&mut script)?;
        script
    } else {
        fs::read(&args.script)?
    };
    let command = binport::execute_command(&args.interpreter, &script_arguments(&args.arguments))?;
    execute(args.target, command, script, use_password, false, json)
}

fn script_arguments(arguments: &[OsString]) -> Vec<OsString> {
    let mut values = vec![OsString::from("-s"), OsString::from("--")];
    values.extend_from_slice(arguments);
    values
}

fn execute(
    target: String,
    command: String,
    input: Vec<u8>,
    use_password: bool,
    tty: bool,
    json: bool,
) -> io::Result<u8> {
    let password = use_password
        .then(|| rpassword::prompt_password("SSH password: "))
        .transpose()?;
    tokio::runtime::Runtime::new()
        .map_err(io::Error::other)?
        .block_on(execute_async(
            &target,
            command,
            input,
            password.as_deref(),
            tty,
            json,
        ))
}

async fn execute_async(
    target: &str,
    command: String,
    input: Vec<u8>,
    password: Option<&str>,
    tty: bool,
    json: bool,
) -> io::Result<u8> {
    if let Some(entry) = binport::host::find(target)?
        && entry.strategy.as_deref() == Some("exec-hop")
    {
        let hop = ExecHop::connect_host(&entry, password, !json).await?;
        if tty {
            return hop
                .execute_tty(command, false)
                .await
                .map(|status| u8::try_from(status).unwrap_or(1));
        }
        let (status, stdout, stderr) = hop.execute_capture_with_input(command, input).await?;
        return emit(status, stdout, stderr, json);
    }

    let destination = Destination::resolve(target)?;
    let ssh = if let Some(alias) = destination.proxy_jump.as_deref() {
        let jump = NativeSsh::connect_jump(alias, password).await?;
        NativeSsh::connect_with_jump(&destination, password, &jump).await?
    } else {
        NativeSsh::connect(&destination, password).await?
    };
    if tty {
        return ssh
            .execute_tty_fresh(&command, false)
            .await
            .map(|status| u8::try_from(status).unwrap_or(1));
    }
    let (status, stdout, stderr) = ssh.execute_capture_with_input(&command, input).await?;
    emit(status, stdout, stderr, json)
}

pub async fn capture_remote(
    target: &str,
    command: String,
    password: Option<&str>,
) -> io::Result<(u32, Vec<u8>, Vec<u8>)> {
    if let Some(entry) = binport::host::find(target)?
        && entry.strategy.as_deref() == Some("exec-hop")
    {
        return ExecHop::connect_host(&entry, password, false)
            .await?
            .execute_capture_with_input(command, Vec::new())
            .await;
    }

    let destination = Destination::resolve(target)?;
    let ssh = if let Some(alias) = destination.proxy_jump.as_deref() {
        let jump = NativeSsh::connect_jump(alias, password).await?;
        NativeSsh::connect_with_jump(&destination, password, &jump).await?
    } else {
        NativeSsh::connect(&destination, password).await?
    };
    ssh.execute_capture_with_input_fresh(&command, Vec::new())
        .await
}

fn emit(status: u32, stdout: Vec<u8>, stderr: Vec<u8>, json: bool) -> io::Result<u8> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": status,
                "stdout": String::from_utf8_lossy(&stdout),
                "stderr": String::from_utf8_lossy(&stderr),
            }))
            .map_err(io::Error::other)?
        );
    } else {
        io::stdout().write_all(&stdout)?;
        io::stderr().write_all(&stderr)?;
    }
    Ok(u8::try_from(status).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::script_arguments;
    use std::ffi::OsString;

    #[test]
    fn script_arguments_use_stdin_mode_and_preserve_values() {
        assert_eq!(
            script_arguments(&[OsString::from("hello world")]),
            ["-s", "--", "hello world"].map(OsString::from)
        );
    }
}
