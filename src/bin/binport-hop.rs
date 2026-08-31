use binport::hop;
use binport::progress::TransferProgress;
use binport::ssh::{Destination, NativeSsh};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::process::ExitCode;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

fn main() -> ExitCode {
    match run() {
        Ok(status) => ExitCode::from(status.min(u8::MAX as u32) as u8),
        Err(error) => {
            eprintln!("binport-hop: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<u32> {
    let runtime = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
    let relay = std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--relay");
    runtime.block_on(async move {
        if relay {
            run_relay().await
        } else {
            run_exec().await
        }
    })
}

async fn run_exec() -> io::Result<u32> {
    let mut stdin = tokio::io::stdin();
    let request = hop::read_request_header_async(&mut stdin).await?;
    let mut destination = Destination::resolve(&request.target)?;
    destination.port = request.target_port;
    if destination.proxy_jump.is_some() || destination.bastion_proxy.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "binport-hop target must be directly reachable from the entry host",
        ));
    }
    let ssh = NativeSsh::connect(&destination, None).await?;
    if !request.remaining.is_empty() {
        return run_nested_exec(ssh, request, stdin).await;
    }
    if request.tty {
        if request.stdin_bytes != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "TTY requests cannot contain a fixed stdin payload",
            ));
        }
        return run_tty(ssh, request.command, stdin).await;
    }
    let (stdout_tx, mut stdout_rx) = mpsc::channel(8);
    let (stderr_tx, mut stderr_rx) = mpsc::channel(8);
    let (stdin_tx, stdin_rx) = mpsc::channel(8);
    let input_bytes = request.stdin_bytes;
    let feeder = tokio::spawn(async move {
        let mut remaining = input_bytes;
        let mut buffer = vec![0_u8; 64 * 1024];
        while remaining > 0 {
            let wanted = usize::try_from(remaining.min(buffer.len() as u64)).unwrap();
            let read = stdin.read(&mut buffer[..wanted]).await?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "hop stdin payload ended before the declared length",
                ));
            }
            stdin_tx
                .send(buffer[..read].to_vec())
                .await
                .map_err(io::Error::other)?;
            remaining -= read as u64;
        }
        let _ = stdin_tx.send(Vec::new()).await;
        Ok::<_, io::Error>(())
    });
    let execution = ssh.client().execute_io(
        &request.command,
        stdout_tx,
        Some(stderr_tx),
        Some(stdin_rx),
        false,
        None,
    );
    tokio::pin!(execution);
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let status = loop {
        tokio::select! {
            result = &mut execution => break result.map_err(io::Error::other)?,
            Some(data) = stdout_rx.recv() => stdout.write_all(&data).await?,
            Some(data) = stderr_rx.recv() => stderr.write_all(&data).await?,
        }
    };
    feeder.await.map_err(io::Error::other)??;
    while let Ok(data) = stdout_rx.try_recv() {
        stdout.write_all(&data).await?;
    }
    while let Ok(data) = stderr_rx.try_recv() {
        stderr.write_all(&data).await?;
    }
    stdout.flush().await?;
    stderr.flush().await?;
    Ok(status)
}

async fn run_nested_exec(
    ssh: NativeSsh,
    mut request: hop::ExecRequest,
    stdin: tokio::io::Stdin,
) -> io::Result<u32> {
    let helper = install_self(&ssh).await?;
    let next = request.remaining.remove(0);
    request.target = next.target;
    request.target_port = next.port;
    let header = hop::encode_request_header(&request)?;
    let command = binport::execute_command(&helper, &[] as &[OsString])?;
    relay_process(ssh, command, header, stdin).await
}

async fn install_self(ssh: &NativeSsh) -> io::Result<String> {
    let (status, stdout, stderr) = ssh.execute_capture("uname -s; uname -m").await?;
    if status != 0 {
        return Err(io::Error::other(format!(
            "cannot detect nested entry platform: {}",
            stderr.trim()
        )));
    }
    let mut lines = stdout.lines();
    let target_platform = binport::catalog::Platform::from_uname(
        lines.next().unwrap_or_default(),
        lines.next().unwrap_or_default(),
    )
    .ok_or_else(|| io::Error::other("nested entry platform is unsupported"))?;
    let current_platform = if cfg!(target_arch = "x86_64") {
        binport::catalog::Platform::LinuxAmd64
    } else {
        binport::catalog::Platform::LinuxArm64
    };
    if target_platform != current_platform {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "nested exec-hop changes architecture from {} to {}; mixed-architecture chains are not supported yet",
                current_platform.name(),
                target_platform.name()
            ),
        ));
    }
    let local = std::env::current_exe()?;
    let hash = binport::sha256_file(&local)?;
    let (directory, remote) = binport::remote_paths(&hash, "binport-hop");
    let (cached, _, _) = ssh
        .execute_capture_with_input(&binport::cache_check_command(&remote), Vec::new())
        .await?;
    if cached != 0 {
        let size = fs::metadata(&local)?.len();
        let (status, stderr) = ssh
            .upload_file(
                &binport::upload_command(&directory, &remote),
                &local,
                TransferProgress::new("binport-hop", Some(size), false),
            )
            .await?;
        if status != 0 {
            return Err(io::Error::other(format!(
                "cannot install helper on nested entry: {}",
                String::from_utf8_lossy(&stderr).trim()
            )));
        }
    }
    Ok(remote)
}

async fn relay_process(
    ssh: NativeSsh,
    command: String,
    header: Vec<u8>,
    mut stdin: tokio::io::Stdin,
) -> io::Result<u32> {
    let (stdout_tx, mut stdout_rx) = mpsc::channel(8);
    let (stderr_tx, mut stderr_rx) = mpsc::channel(8);
    let (stdin_tx, stdin_rx) = mpsc::channel(8);
    let feeder = tokio::spawn(async move {
        stdin_tx.send(header).await.map_err(io::Error::other)?;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = stdin.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            stdin_tx
                .send(buffer[..read].to_vec())
                .await
                .map_err(io::Error::other)?;
        }
        let _ = stdin_tx.send(Vec::new()).await;
        Ok::<_, io::Error>(())
    });
    let execution = ssh.client().execute_io(
        &command,
        stdout_tx,
        Some(stderr_tx),
        Some(stdin_rx),
        false,
        None,
    );
    tokio::pin!(execution);
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let status = loop {
        tokio::select! {
            result = &mut execution => break result.map_err(io::Error::other)?,
            Some(data) = stdout_rx.recv() => stdout.write_all(&data).await?,
            Some(data) = stderr_rx.recv() => stderr.write_all(&data).await?,
        }
    };
    feeder.abort();
    while let Ok(data) = stdout_rx.try_recv() {
        stdout.write_all(&data).await?;
    }
    while let Ok(data) = stderr_rx.try_recv() {
        stderr.write_all(&data).await?;
    }
    stdout.flush().await?;
    stderr.flush().await?;
    Ok(status)
}

async fn run_tty(ssh: NativeSsh, command: String, mut stdin: tokio::io::Stdin) -> io::Result<u32> {
    let (output_tx, mut output_rx) = mpsc::channel(32);
    let (input_tx, input_rx) = mpsc::channel(8);
    let feeder = tokio::spawn(async move {
        let mut buffer = vec![0_u8; 4096];
        loop {
            let read = stdin.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            if input_tx.send(buffer[..read].to_vec()).await.is_err() {
                break;
            }
        }
        let _ = input_tx.send(Vec::new()).await;
        Ok::<_, io::Error>(())
    });
    let execution = ssh
        .client()
        .execute_io(&command, output_tx, None, Some(input_rx), true, None);
    tokio::pin!(execution);
    let mut stdout = tokio::io::stdout();
    let status = loop {
        tokio::select! {
            result = &mut execution => break result.map_err(io::Error::other)?,
            Some(data) = output_rx.recv() => {
                stdout.write_all(&data).await?;
                stdout.flush().await?;
            }
        }
    };
    feeder.abort();
    while let Ok(data) = output_rx.try_recv() {
        stdout.write_all(&data).await?;
    }
    stdout.flush().await?;
    Ok(status)
}

async fn run_relay() -> io::Result<u32> {
    let mut stdin = tokio::io::stdin();
    let mut request = hop::read_relay_header_async(&mut stdin).await?;
    let mut destination = Destination::resolve(&request.target)?;
    destination.port = request.target_port;
    if destination.proxy_jump.is_some() || destination.bastion_proxy.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "binport-hop relay target must be directly reachable from the entry host",
        ));
    }
    let ssh = NativeSsh::connect(&destination, None).await?;
    if !request.remaining.is_empty() {
        let helper = install_self(&ssh).await?;
        let next = request.remaining.remove(0);
        request.target = next.target;
        request.target_port = next.port;
        let header = hop::encode_relay_header(&request)?;
        let command = binport::execute_command(&helper, &[OsString::from("--relay")])?;
        return relay_process(ssh, command, header, stdin).await;
    }
    let remote = format!("{}:{}", request.remote_host, request.remote_port);
    let channel = ssh
        .client()
        .open_direct_tcpip_channel(remote.as_str(), None)
        .await
        .map_err(io::Error::other)?;
    let (mut remote_read, mut remote_write) = tokio::io::split(channel.into_stream());
    let mut stdout = tokio::io::stdout();
    let upstream = async {
        tokio::io::copy(&mut stdin, &mut remote_write).await?;
        remote_write.shutdown().await
    };
    let downstream = async {
        tokio::io::copy(&mut remote_read, &mut stdout).await?;
        stdout.flush().await
    };
    tokio::pin!(upstream);
    tokio::pin!(downstream);
    tokio::select! {
        result = &mut upstream => {
            result?;
            downstream.await?;
        }
        result = &mut downstream => {
            result?;
        }
    }
    Ok(0)
}
