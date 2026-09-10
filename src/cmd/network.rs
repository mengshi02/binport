use super::native_exec::capture_remote;
use std::collections::BTreeMap;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct BandwidthResult {
    pub metrics: BTreeMap<String, String>,
    pub observations: Vec<String>,
}

pub async fn measure(
    source: &str,
    peer: &str,
    peer_address: &str,
    duration: u8,
    password: Option<&str>,
) -> BandwidthResult {
    let mut result = BandwidthResult {
        metrics: BTreeMap::new(),
        observations: Vec::new(),
    };
    if duration == 0 {
        result
            .observations
            .push("Bandwidth duration must be greater than zero".into());
        return result;
    }
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let offset = nonce
        .bytes()
        .fold(0_u16, |value, byte| value.wrapping_add(byte.into()))
        % 10_000;
    let tcp_port = 30_000 + offset;
    match measure_tcp(
        source,
        peer,
        peer_address,
        tcp_port,
        duration,
        &nonce,
        password,
    )
    .await
    {
        Ok(metrics) => result.metrics.extend(metrics),
        Err(error) => {
            result
                .metrics
                .insert("tcp_throughput".into(), "unavailable".into());
            result
                .observations
                .push(format!("TCP throughput test failed: {error}"));
        }
    }

    let source_address = match binport::ssh::Destination::resolve(source) {
        Ok(destination) => destination.hostname,
        Err(error) => {
            result
                .observations
                .push(format!("RDMA throughput test skipped: {error}"));
            return result;
        }
    };
    match measure_rdma(
        source,
        peer,
        &source_address,
        peer_address,
        tcp_port.saturating_add(1),
        duration,
        &nonce,
        password,
    )
    .await
    {
        Ok(Some(metrics)) => result.metrics.extend(metrics),
        Ok(None) => result.observations.push(
            "RDMA throughput test skipped: ib_write_bw or a route-matched RDMA device is unavailable on one of the nodes".into(),
        ),
        Err(error) => {
            result
                .metrics
                .insert("rdma_throughput".into(), "unavailable".into());
            result
                .observations
                .push(format!("RDMA throughput test failed: {error}"));
        }
    }
    result
}

const TCP_SERVER: &str = r#"import socket,sys
token=sys.argv[1].encode(); port=int(sys.argv[2]); timeout=int(sys.argv[3])+10
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1); s.bind(("",port)); s.listen(1); s.settimeout(timeout)
c,_=s.accept(); c.settimeout(timeout); auth=b""
while not auth.endswith(b"\n") and len(auth)<256: auth+=c.recv(1)
if auth.rstrip()!=token: raise SystemExit(2)
while c.recv(1024*1024): pass
c.close(); s.close()"#;

const TCP_CLIENT: &str = r#"import socket,sys,time
host=sys.argv[1]; port=int(sys.argv[2]); duration=float(sys.argv[3]); token=sys.argv[4].encode()
s=socket.socket(); s.settimeout(5); deadline=time.monotonic()+5
while True:
 try: s.connect((host,port)); break
 except OSError:
  if time.monotonic()>=deadline: raise
  time.sleep(.1)
s.settimeout(duration+5); s.sendall(token+b"\n"); payload=bytes(1024*1024); sent=0; start=time.monotonic(); end=start+duration
while time.monotonic()<end: s.sendall(payload); sent+=len(payload)
s.shutdown(socket.SHUT_WR); elapsed=time.monotonic()-start; s.close()
print("tcp_throughput_gbps\t"+str(round(sent*8/elapsed/1e9,3)))
print("tcp_bytes_sent\t"+str(sent)); print("tcp_duration_seconds\t"+str(round(elapsed,3)))"#;

async fn measure_tcp(
    source: &str,
    peer: &str,
    address: &str,
    port: u16,
    duration: u8,
    nonce: &str,
    password: Option<&str>,
) -> io::Result<BTreeMap<String, String>> {
    let log = format!("/tmp/binport-tcp-{nonce}.log");
    let port = port.to_string();
    let duration = duration.to_string();
    let server = background_command(
        &log,
        "python3",
        &["-c", TCP_SERVER, nonce, &port, &duration],
    )?;
    let (status, stdout, stderr) = capture_remote(peer, server, password).await?;
    if status != 0 {
        return Err(io::Error::other(format!(
            "could not start receiver: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    let pid = parse_pid(&stdout)?;
    let client = binport::execute_command(
        "python3",
        &[
            "-c".into(),
            TCP_CLIENT.into(),
            address.into(),
            port.into(),
            duration.into(),
            nonce.into(),
        ],
    )?;
    let measured = capture_remote(source, client, password).await;
    cleanup(peer, pid, &log, password).await;
    let (status, stdout, stderr) = measured?;
    if status != 0 {
        return Err(io::Error::other(
            String::from_utf8_lossy(&stderr).trim().to_owned(),
        ));
    }
    Ok(parse_metrics(&stdout))
}

#[allow(clippy::too_many_arguments)]
async fn measure_rdma(
    source: &str,
    peer: &str,
    source_address: &str,
    peer_address: &str,
    port: u16,
    duration: u8,
    nonce: &str,
    password: Option<&str>,
) -> io::Result<Option<BTreeMap<String, String>>> {
    let Some(source_device) = rdma_device(source, peer_address, password).await? else {
        return Ok(None);
    };
    let Some(peer_device) = rdma_device(peer, source_address, password).await? else {
        return Ok(None);
    };
    let log = format!("/tmp/binport-rdma-{nonce}.log");
    let port = port.to_string();
    let duration = duration.to_string();
    let server = background_command(
        &log,
        "ib_write_bw",
        &[
            "-d",
            &peer_device,
            "-F",
            "--report_gbits",
            "-D",
            &duration,
            "-p",
            &port,
        ],
    )?;
    let (status, stdout, _) = capture_remote(peer, server, password).await?;
    if status != 0 {
        return Ok(None);
    }
    let pid = parse_pid(&stdout)?;
    let client = binport::execute_command(
        "ib_write_bw",
        &[
            "-d".into(),
            source_device.clone().into(),
            "-F".into(),
            "--report_gbits".into(),
            "-D".into(),
            duration.into(),
            "-p".into(),
            port.into(),
            peer_address.into(),
        ],
    )?;
    let measured = capture_remote(source, client, password).await;
    cleanup(peer, pid, &log, password).await;
    let (status, stdout, stderr) = measured?;
    if status != 0 {
        return Err(io::Error::other(
            String::from_utf8_lossy(&stderr).trim().to_owned(),
        ));
    }
    let text = String::from_utf8_lossy(&stdout);
    let (throughput, rate) = text
        .lines()
        .filter_map(parse_rdma_row)
        .next_back()
        .ok_or_else(|| io::Error::other("ib_write_bw returned no result row"))?;
    Ok(Some(BTreeMap::from([
        ("rdma_device".into(), source_device),
        ("rdma_throughput_gbps".into(), throughput),
        ("rdma_message_rate_mpps".into(), rate),
    ])))
}

async fn rdma_device(
    host: &str,
    remote: &str,
    password: Option<&str>,
) -> io::Result<Option<String>> {
    let script = r#"iface=$(ip route get "$1" 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p' | head -n1); command -v ib_write_bw >/dev/null 2>&1 || exit 0; command -v ibdev2netdev >/dev/null 2>&1 || exit 0; ibdev2netdev 2>/dev/null | awk -v iface="$iface" '$5 == iface && $6 == "(Up)" {print $1; exit}'"#;
    let command = binport::execute_command(
        "sh",
        &[
            "-c".into(),
            script.into(),
            "binport-rdma-device".into(),
            remote.into(),
        ],
    )?;
    let (status, stdout, _) = capture_remote(host, command, password).await?;
    if status != 0 {
        return Ok(None);
    }
    let value = String::from_utf8_lossy(&stdout).trim().to_owned();
    Ok((!value.is_empty()).then_some(value))
}

fn background_command(log: &str, executable: &str, args: &[&str]) -> io::Result<String> {
    let mut values = vec![
        "-c".into(),
        "log=$1; shift; nohup \"$@\" >\"$log\" 2>&1 </dev/null & echo $!".into(),
        "binport-background".into(),
        log.into(),
        executable.into(),
    ];
    values.extend(args.iter().map(|value| (*value).into()));
    binport::execute_command("sh", &values)
}

fn parse_pid(output: &[u8]) -> io::Result<u32> {
    String::from_utf8_lossy(output)
        .trim()
        .parse::<u32>()
        .map_err(|_| io::Error::other("remote receiver did not return a process id"))
}

async fn cleanup(host: &str, pid: u32, log: &str, password: Option<&str>) {
    if let Ok(command) = binport::execute_command(
        "sh",
        &[
            "-c".into(),
            "kill \"$1\" 2>/dev/null || true; rm -f -- \"$2\"".into(),
            "binport-cleanup".into(),
            pid.to_string().into(),
            log.into(),
        ],
    ) {
        let _ = capture_remote(host, command, password).await;
    }
}

fn parse_metrics(output: &[u8]) -> BTreeMap<String, String> {
    String::from_utf8_lossy(output)
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(key, value)| (key.into(), value.into()))
        .collect()
}

fn parse_rdma_row(line: &str) -> Option<(String, String)> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    (fields.len() >= 5
        && fields[0].parse::<u64>().is_ok()
        && fields[1].parse::<u64>().is_ok()
        && fields[3].parse::<f64>().is_ok())
    .then(|| (fields[3].into(), fields[4].into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rdma_bandwidth_rows() {
        let row = parse_rdma_row("65536 5000 23.14 22.85 0.04358").unwrap();
        assert_eq!(row, ("22.85".into(), "0.04358".into()));
        assert!(parse_rdma_row("#bytes #iterations BW").is_none());
    }

    #[test]
    fn parses_tab_separated_tcp_metrics() {
        let metrics = parse_metrics(b"tcp_throughput_gbps\t8.2\ntcp_bytes_sent\t42\n");
        assert_eq!(metrics["tcp_throughput_gbps"], "8.2");
    }
}
