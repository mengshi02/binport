use super::native_exec::capture_remote;
use super::table;
use clap::Args;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal};

const PROBE: &str = r#"
emit() { printf '%s\t%s\n' "$1" "$2"; }
emit_if() { if [ -n "$2" ]; then emit "$1" "$2"; fi; return 0; }
first() { "$@" 2>/dev/null | head -n 1; }
emit system.hostname "$(first hostname)"
emit system.os "$(. /etc/os-release 2>/dev/null; printf '%s' "${PRETTY_NAME:-unknown}")"
emit system.kernel "$(first uname -r)"
emit system.arch "$(first uname -m)"
emit system.glibc "$(first getconf GNU_LIBC_VERSION)"
emit resources.cpu "$(getconf _NPROCESSORS_ONLN 2>/dev/null || true)"
emit resources.cpu_model "$(awk -F: '/^(model name|Hardware)/ {sub(/^[[:space:]]*/, "", $2); print $2; exit}' /proc/cpuinfo 2>/dev/null)"
emit resources.memory_kib "$(awk '/^MemTotal:/ {print $2}' /proc/meminfo 2>/dev/null)"
emit resources.memory_available_kib "$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo 2>/dev/null)"
emit resources.swap_kib "$(awk '/^SwapTotal:/ {print $2}' /proc/meminfo 2>/dev/null)"
emit resources.disk_free_kib "$(df -Pk / 2>/dev/null | awk 'NR==2 {print $4}')"
emit resources.root_filesystem "$(df -PT / 2>/dev/null | awk 'NR==2 {print $2}')"
emit runtime.shell "${SHELL:-unknown}"
emit runtime.container "$(if command -v docker >/dev/null 2>&1; then first docker --version; elif command -v nerdctl >/dev/null 2>&1; then first nerdctl --version; elif command -v podman >/dev/null 2>&1; then first podman --version; fi)"
emit runtime.python "$(first python3 --version)"
emit runtime.java "$(command -v java >/dev/null 2>&1 && java -version 2>&1 | head -n 1)"
emit runtime.gcc "$(first gcc --version)"
emit runtime.cmake "$(first cmake --version)"
emit configuration.nofile "$(ulimit -n 2>/dev/null || true)"
emit configuration.ip_forward "$(cat /proc/sys/net/ipv4/ip_forward 2>/dev/null || true)"
emit configuration.transparent_hugepages "$(cat /sys/kernel/mm/transparent_hugepage/enabled 2>/dev/null || true)"
emit configuration.numa_balancing "$(cat /proc/sys/kernel/numa_balancing 2>/dev/null || true)"
emit configuration.cgroup "$(stat -fc %T /sys/fs/cgroup 2>/dev/null || true)"
cgroup_path="$(awk -F: '$1 == "0" {print $3}' /proc/self/cgroup 2>/dev/null)"
cgroup_base="/sys/fs/cgroup${cgroup_path:-}"
emit configuration.cgroup_memory_limit_bytes "$(if [ -r "$cgroup_base/memory.max" ]; then cat "$cgroup_base/memory.max"; elif [ -r /sys/fs/cgroup/memory/memory.limit_in_bytes ]; then cat /sys/fs/cgroup/memory/memory.limit_in_bytes; fi)"
emit configuration.cgroup_cpu_quota "$(if [ -r "$cgroup_base/cpu.max" ]; then cat "$cgroup_base/cpu.max"; elif [ -r /sys/fs/cgroup/cpu/cpu.cfs_quota_us ]; then printf '%s/' "$(cat /sys/fs/cgroup/cpu/cpu.cfs_quota_us)"; cat /sys/fs/cgroup/cpu/cpu.cfs_period_us; fi)"
emit network.dns "$(awk '/^nameserver/ {print $2; exit}' /etc/resolv.conf 2>/dev/null)"
default_interface="$(ip route show default 2>/dev/null | awk 'NR==1 {print $5}')"
emit_if network.default_interface "$default_interface"
emit_if network.mtu "$([ -n "$default_interface" ] && cat "/sys/class/net/$default_interface/mtu" 2>/dev/null)"
emit_if network.speed_mbps "$([ -n "$default_interface" ] && cat "/sys/class/net/$default_interface/speed" 2>/dev/null)"
emit_if network.rdma "$(find /sys/class/infiniband -mindepth 1 -maxdepth 1 -printf '%f\n' 2>/dev/null | sort | paste -sd ',' -)"
emit accelerator.nvidia_count "$(if command -v nvidia-smi >/dev/null 2>&1; then nvidia-smi -L 2>/dev/null | wc -l | tr -d ' '; else printf 0; fi)"
emit_if accelerator.nvidia_gpus "$(command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi --query-gpu=name,memory.total,compute_cap --format=csv,noheader 2>/dev/null | paste -sd ';' -)"
emit_if accelerator.nvidia_driver "$(command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | sort -u | paste -sd ',' -)"
emit_if accelerator.nvidia_pcie "$(command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi --query-gpu=pcie.link.gen.current,pcie.link.width.current --format=csv,noheader 2>/dev/null | awk -F, '{gsub(/ /, ""); print "Gen " $1 " x" $2}' | sort -u | paste -sd ';' -)"
emit_if accelerator.cuda_driver_api "$(command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi 2>/dev/null | sed -n 's/.*CUDA Version: *\([^ ]*\).*/\1/p' | head -n 1)"
emit_if accelerator.cuda "$(command -v nvcc >/dev/null 2>&1 && nvcc --version 2>/dev/null | awk '/release/ {print $0; exit}')"
emit_if accelerator.rocm "$(if [ -r /opt/rocm/.info/version ]; then cat /opt/rocm/.info/version; elif command -v hipcc >/dev/null 2>&1; then hipcc --version 2>/dev/null | head -n 1; fi)"
hy_smi="$(command -v hy-smi 2>/dev/null || true)"
if [ -z "$hy_smi" ]; then
  for candidate in /opt/hyhal/bin/hy-smi /opt/dtk/bin/hy-smi /opt/hygondtk/bin/hy-smi; do
    if [ -x "$candidate" ]; then hy_smi="$candidate"; break; fi
  done
fi
if [ -n "$hy_smi" ]; then
  hygon_list="$($hy_smi -L 2>/dev/null || true)"
  hygon_info="$($hy_smi 2>/dev/null || true)"
  hygon_memory="$($hy_smi --showmeminfo vram 2>/dev/null || true)"
  hygon_product="$($hy_smi --showproductname 2>/dev/null || true)"
  hygon_hw="$($hy_smi --showhw 2>/dev/null || true)"
  hygon_fw="$($hy_smi --showfwinfo 2>/dev/null || true)"
fi
emit accelerator.hygon_dcu_count "$(if [ -n "$hy_smi" ]; then count=$(printf '%s\n' "$hygon_list" | grep -Eic '^(GPU|DCU)[^0-9]*[0-9]'); if [ "$count" -eq 0 ]; then count=$(printf '%s\n' "$hygon_info" | grep -Eic '^[[:space:]]*[0-9]+[[:space:]]'); fi; printf '%s' "$count"; else printf 0; fi)"
emit_if accelerator.hygon_dcu_products "$(printf '%s\n%s\n%s\n' "$hygon_product" "$hygon_list" "$hygon_info" | grep -Eio '(BW|K|Z)[0-9]+(_AI|L)?' | sort -u | paste -sd ',' -)"
emit_if accelerator.hygon_dcu_vendor "$(printf '%s\n' "$hygon_product" | sed -n 's/.*Card Vendor:[[:space:]]*//p' | sort -u | paste -sd ',' -)"
emit_if accelerator.hygon_dcu_driver "$(if [ -n "$hy_smi" ]; then $hy_smi --showdriverversion 2>/dev/null | sed -n 's/.*[Dd]river[^:]*:[[:space:]]*//p' | head -n 1; fi)"
emit_if accelerator.hygon_dcu_vbios "$(if [ -n "$hy_smi" ]; then $hy_smi --showvbios 2>/dev/null | sed -n 's/.*[Vv][Bb][Ii][Oo][Ss][^:]*:[[:space:]]*//p' | sort -u | paste -sd ',' -; fi)"
emit_if accelerator.hygon_dcu_vram_mib "$(printf '%s\n' "$hygon_memory" | awk 'BEGIN{IGNORECASE=1} /total/ && /memory/ {for(i=1;i<=NF;i++) if($i ~ /^[0-9]+$/){print $i; exit}}')"
emit_if accelerator.hygon_dcu_device_ids "$(printf '%s\n' "$hygon_hw" | awk '/^[0-9]+[[:space:]]+[0-9A-Fa-f]+[[:space:]]+[0-9A-Fa-f]+/ {print "DID=" $2 ",SSID=" $3; exit}')"
emit_if accelerator.hygon_dcu_bus_ids "$(printf '%s\n' "$hygon_hw" | awk '/^[0-9]+[[:space:]]+[0-9A-Fa-f]+[[:space:]]+[0-9A-Fa-f]+/ {print $NF}' | paste -sd ',' -)"
emit_if accelerator.hygon_dcu_firmware "$(printf '%s\n' "$hygon_fw" | awk -F: '/HCU\[0\]/ && /Firmware Version/ {name=$2; sub(/[[:space:]]*Firmware Version[[:space:]]*$/, "", name); value=$NF; gsub(/^[[:space:]]+|[[:space:]]+$/, "", name); gsub(/^[[:space:]]+|[[:space:]]+$/, "", value); printf "%s%s=%s", separator, name, value; separator=","}')"
emit_if accelerator.dtk "$(for file in /opt/dtk/.info/version /opt/dtk/.info/version-dev /opt/hygondtk/.info/version; do [ -r "$file" ] && { head -n 1 "$file"; break; }; done)"
emit_if accelerator.hip "$(hipconfig_bin=$(command -v hipconfig 2>/dev/null || true); if [ -z "$hipconfig_bin" ]; then for candidate in /opt/dtk/bin/hipconfig /opt/dtk/hip/bin/hipconfig /opt/dtk-*/hip/bin/hipconfig /opt/hygondtk/bin/hipconfig /opt/hygondtk/hip/bin/hipconfig; do [ -x "$candidate" ] && { hipconfig_bin="$candidate"; break; }; done; fi; if [ -n "$hipconfig_bin" ]; then $hipconfig_bin --version 2>/dev/null | head -n 1; elif command -v hipcc >/dev/null 2>&1; then hipcc --version 2>/dev/null | sed -n '/HIP version/ {p;q;}'; fi)"
if command -v npu-smi >/dev/null 2>&1; then
  ascend_list="$(npu-smi info -l 2>/dev/null)"
  ascend_id="$(printf '%s\n' "$ascend_list" | sed -n 's/.*NPU ID[^:]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1)"
  [ -n "$ascend_id" ] && ascend_board="$(npu-smi info -t board -i "$ascend_id" 2>/dev/null || true)"
fi
emit accelerator.ascend_count "$(if command -v npu-smi >/dev/null 2>&1; then printf '%s\n' "$ascend_list" | sed -n 's/.*Card Count[^:]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1; else printf 0; fi)"
emit_if accelerator.ascend_products "$(printf '%s\n' "$ascend_list" | sed -n 's/.*Product Name[^:]*:[[:space:]]*//p' | grep -v '^\(NA\|N/A\)$' | sort -u | paste -sd ',' -)"
emit_if accelerator.ascend_chips "$(printf '%s\n' "$ascend_list" | awk -F: '/Chip Count/ {gsub(/ /, "", $2); total += $2; found=1} END {if (found) print total}')"
emit_if accelerator.ascend_firmware "$(printf '%s\n' "$ascend_board" | sed -n 's/.*Firmware Version[^:]*:[[:space:]]*//p' | head -n 1)"
emit_if accelerator.ascend_driver "$(for file in /usr/local/Ascend/driver/version.info /etc/ascend_install.info; do [ -r "$file" ] || continue; sed -n 's/^\([Pp]ackage_\{0,1\}\)\{0,1\}[Vv]ersion[=:][[:space:]]*//p' "$file" | head -n 1; break; done)"
emit_if accelerator.cann "$(file=$(find /usr/local/Ascend/ascend-toolkit -maxdepth 4 -type f \( -name version.info -o -name ascend_toolkit_install.info \) 2>/dev/null | head -n 1); [ -n "$file" ] && sed -n 's/^\([Pp]ackage_\{0,1\}\)\{0,1\}[Vv]ersion[=:][[:space:]]*//p' "$file" | head -n 1)"
emit_if accelerator.intel_xpu "$(command -v xpu-smi >/dev/null 2>&1 && xpu-smi discovery 2>/dev/null | paste -sd ';' -)"
if command -v mthreads-gmi >/dev/null 2>&1; then
  moore_list="$(mthreads-gmi -L 2>/dev/null)"
  moore_info="$(mthreads-gmi -cf 2>/dev/null)"
fi
emit accelerator.moore_threads_count "$(printf '%s\n' "$moore_list" | grep -c '^GPU ')"
emit_if accelerator.moore_threads_gpus "$(printf '%s\n' "$moore_list" | sed -n 's/^GPU [0-9][0-9]* : *\(.*\) *(UUID.*/\1/p' | sort | uniq -c | awk '{count=$1; $1=""; sub(/^ /, ""); printf "%s%s x%s", separator, $0, count; separator=", "}')"
emit_if accelerator.moore_threads_driver "$(printf '%s\n' "$moore_info" | sed -n 's/.*Driver Version: *\([^ ]*\).*/\1/p' | head -n 1)"
emit_if accelerator.moore_threads_vram_mib "$(printf '%s\n' "$moore_info" | sed -n 's/.*MiB(\([0-9][0-9]*\)MiB).*/\1/p' | head -n 1)"
emit_if accelerator.moore_threads_pcie "$(printf '%s\n' "$moore_info" | awk -F'|' '$2 ~ /x\(/ {gsub(/[[:space:]]/, "", $2); print $2}' | sort -u | paste -sd ',' -)"
emit_if accelerator.musa "$(command -v musa_driver_version >/dev/null 2>&1 && musa_driver_version 2>/dev/null | sed -n 's/.*"version":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
emit accelerator.numa_nodes "$(find /sys/devices/system/node -maxdepth 1 -type d -name 'node[0-9]*' 2>/dev/null | wc -l | tr -d ' ')"
emit accelerator.rdma_devices "$(find /sys/class/infiniband -mindepth 1 -maxdepth 1 2>/dev/null | wc -l | tr -d ' ')"
emit accelerator.cpu_features "$(flags=$(awk -F: '/^(flags|Features)/ {print $2; exit}' /proc/cpuinfo 2>/dev/null); for feature in avx2 avx512f amx_tile sve; do printf '%s' "$flags" | grep -qw "$feature" && printf '%s ' "$feature"; done)"
emit_if ai_runtime.packages "$(python3 -c 'import sys; m=__import__("importlib.metadata",fromlist=["metadata"]) if sys.version_info >= (3,8) else __import__("importlib_metadata"); n=["torch","tensorflow","jax","vllm","transformers","deepspeed","onnxruntime","sglang","triton"]; d={str(x.metadata.get("Name","")).lower():x.version for x in m.distributions()}; print(", ".join(f"{x}={d[x]}" for x in n if x in d))' 2>/dev/null)"
emit ai_runtime.shm_kib "$(df -Pk /dev/shm 2>/dev/null | awk 'NR==2 {print $2}')"
emit ai_runtime.hugepages_total "$(awk '/^HugePages_Total:/ {print $2}' /proc/meminfo 2>/dev/null)"
emit_if ai_runtime.cuda_visible_devices "${CUDA_VISIBLE_DEVICES:-}"
emit_if ai_runtime.nvidia_visible_devices "${NVIDIA_VISIBLE_DEVICES:-}"
emit_if ai_runtime.rocr_visible_devices "${ROCR_VISIBLE_DEVICES:-}"
emit_if ai_runtime.hip_visible_devices "${HIP_VISIBLE_DEVICES:-}"
emit_if ai_runtime.ascend_visible_devices "${ASCEND_RT_VISIBLE_DEVICES:-}"
emit_if ai_runtime.musa_visible_devices "${MUSA_VISIBLE_DEVICES:-}"
emit_if ai_runtime.omp_num_threads "${OMP_NUM_THREADS:-}"
emit_if ai_runtime.nccl_debug "${NCCL_DEBUG:-}"
emit_if ai_runtime.collective_libraries "$(ldconfig -p 2>/dev/null | awk '/lib(nccl|hccl|mccl)/ {print $1}' | sort -u | paste -sd ',' -)"
"#;

#[derive(Debug, Args)]
pub struct InspectArgs {
    /// SSH host configured in binport or ~/.ssh/config
    target: String,
    /// Probe connectivity from this host to another configured host
    #[arg(long)]
    peer: Option<String>,
    /// ICMP samples used by --peer
    #[arg(long, default_value_t = 4, requires = "peer")]
    samples: u8,
    /// Measure TCP and, when available, RDMA throughput to --peer
    #[arg(long, requires = "peer")]
    bandwidth: bool,
    /// Seconds to run each bandwidth measurement
    #[arg(long, default_value_t = 5, requires = "bandwidth")]
    bandwidth_duration: u8,
    /// Only show these comma-separated sections
    #[arg(long, value_delimiter = ',')]
    section: Vec<String>,
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// First SSH host
    left: String,
    /// Second SSH host
    right: String,
    /// Only compare these comma-separated sections
    #[arg(long, value_delimiter = ',')]
    section: Vec<String>,
    /// Include fields whose values are equal
    #[arg(long)]
    all: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EnvironmentSnapshot {
    host: String,
    values: BTreeMap<String, BTreeMap<String, String>>,
    raw_values: BTreeMap<String, BTreeMap<String, u64>>,
}

#[derive(Debug, Serialize)]
struct Difference {
    section: String,
    field: String,
    left: Option<String>,
    right: Option<String>,
    equal: bool,
}

#[derive(Debug, Serialize)]
struct PeerReport {
    source: String,
    peer: String,
    address: String,
    port: u16,
    status: String,
    metrics: BTreeMap<String, String>,
    observations: Vec<String>,
}

pub fn inspect(args: InspectArgs, use_password: bool, json: bool) -> io::Result<u8> {
    let password = prompt_password(use_password)?;
    let progress = binport::progress::TaskProgress::new(
        format!("Inspecting {} · connecting and collecting", args.target),
        !json,
    );
    let active_progress = progress.clone();
    let result = runtime()?.block_on(async {
        let snapshot = collect(&args.target, password.as_deref()).await?;
        let peer = match args.peer.as_deref() {
            Some(peer) => {
                active_progress.set_message(format!(
                    "Inspecting {} -> {} · probing network path",
                    args.target, peer
                ));
                let mut report =
                    probe_peer(&args.target, peer, args.samples, password.as_deref()).await?;
                if args.bandwidth {
                    active_progress.set_message(format!(
                        "Inspecting {} -> {} · measuring TCP and RDMA throughput",
                        args.target, peer
                    ));
                    let result = super::network::measure(
                        &args.target,
                        peer,
                        &report.address,
                        args.bandwidth_duration,
                        password.as_deref(),
                    )
                    .await;
                    report.metrics.extend(result.metrics);
                    report
                        .observations
                        .retain(|item| !item.starts_with("Throughput was not measured"));
                    report.observations.extend(result.observations);
                }
                Some(report)
            }
            None => None,
        };
        Ok::<_, io::Error>((snapshot, peer))
    });
    progress.finish();
    let (snapshot, peer) = result?;
    let snapshot = filter(snapshot, &args.section);
    if json {
        let output = if let Some(peer) = peer {
            serde_json::to_string_pretty(&serde_json::json!({
                "environment": snapshot,
                "peer_connectivity": peer,
            }))
        } else {
            serde_json::to_string_pretty(&snapshot)
        };
        println!("{}", output.map_err(io::Error::other)?);
    } else {
        let color = colors_enabled();
        println!(
            "{}: {}\n",
            paint(color, "1;36", "Environment"),
            paint(color, "1", &snapshot.host)
        );
        print!("{}", snapshot_table(&snapshot, color));
        if let Some(peer) = peer {
            print_peer_report(&peer, color);
        }
    }
    Ok(0)
}

async fn probe_peer(
    source: &str,
    peer: &str,
    samples: u8,
    password: Option<&str>,
) -> io::Result<PeerReport> {
    if samples == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--samples must be greater than zero",
        ));
    }
    let destination = binport::ssh::Destination::resolve(peer)?;
    let command = binport::execute_command(
        "sh",
        &[
            "-c".into(),
            PEER_PROBE.into(),
            "binport-peer-probe".into(),
            destination.hostname.clone().into(),
            destination.port.to_string().into(),
            samples.to_string().into(),
        ],
    )?;
    let (status, stdout, stderr) = capture_remote(source, command, password).await?;
    if status != 0 {
        return Err(io::Error::other(format!(
            "peer probe failed on {source}: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(parse_peer_report(
        source,
        peer,
        &destination.hostname,
        destination.port,
        &String::from_utf8_lossy(&stdout),
    ))
}

const PEER_PROBE: &str = r#"
peer=$1; port=$2; samples=$3
emit() { printf '%s\t%s\n' "$1" "$2"; }
resolved=$(getent ahostsv4 "$peer" 2>/dev/null | awk 'NR==1 {print $1}')
[ -z "$resolved" ] && resolved=$(getent hosts "$peer" 2>/dev/null | awk 'NR==1 {print $1}')
emit resolved "${resolved:-unavailable}"
route=$(ip route get "$peer" 2>/dev/null | head -n 1)
emit control_path_interface "$(printf '%s\n' "$route" | sed -n 's/.* dev \([^ ]*\).*/\1/p')"
emit control_path_source_ip "$(printf '%s\n' "$route" | sed -n 's/.* src \([^ ]*\).*/\1/p')"
printf '%s\n' "$route" | grep -q ' via ' && emit control_path_route_type routed || emit control_path_route_type direct
iface=$(printf '%s\n' "$route" | sed -n 's/.* dev \([^ ]*\).*/\1/p')
[ -n "$iface" ] && emit control_path_mtu "$(cat "/sys/class/net/$iface/mtu" 2>/dev/null)"
[ -n "$iface" ] && emit control_path_link_speed_mbps "$(cat "/sys/class/net/$iface/speed" 2>/dev/null)"
[ -n "$iface" ] && emit control_path_numa_node "$(cat "/sys/class/net/$iface/device/numa_node" 2>/dev/null)"
[ -n "$iface" ] && emit control_path_bond_slaves "$(cat "/sys/class/net/$iface/bonding/slaves" 2>/dev/null)"
if command -v ping >/dev/null 2>&1; then
  ping_out=$(LC_ALL=C ping -n -c "$samples" -W 2 "$peer" 2>/dev/null || true)
  emit packet_loss_pct "$(printf '%s\n' "$ping_out" | sed -n 's/.* \([0-9.]*\)% packet loss.*/\1/p' | tail -n 1)"
  rtt=$(printf '%s\n' "$ping_out" | awk -F'= ' '/min\/avg\/max/ {print $2}' | awk '{print $1}')
  emit latency_min_ms "$(printf '%s' "$rtt" | cut -d/ -f1)"
  emit latency_avg_ms "$(printf '%s' "$rtt" | cut -d/ -f2)"
  emit latency_max_ms "$(printf '%s' "$rtt" | cut -d/ -f3)"
  emit latency_jitter_ms "$(printf '%s' "$rtt" | cut -d/ -f4)"
else
  emit packet_loss_pct unavailable
fi
if command -v python3 >/dev/null 2>&1; then
  tcp=$(python3 -c 'import socket,sys,time; s=socket.socket(); s.settimeout(3); t=time.monotonic(); r=s.connect_ex((sys.argv[1],int(sys.argv[2]))); print(("ok" if r==0 else "failed")+" "+str(round((time.monotonic()-t)*1000,2))); s.close()' "$peer" "$port" 2>/dev/null)
  emit tcp "$(printf '%s' "$tcp" | awk '{print $1}')"
  emit tcp_connect_ms "$(printf '%s' "$tcp" | awk '{print $2}')"
elif command -v nc >/dev/null 2>&1; then
  nc -z -w 3 "$peer" "$port" >/dev/null 2>&1 && emit tcp ok || emit tcp failed
else
  emit tcp unavailable
fi
emit rdma_devices "$(find /sys/class/infiniband -mindepth 1 -maxdepth 1 2>/dev/null | wc -l | tr -d ' ')"
emit rdma_active_ports "$(for p in /sys/class/infiniband/*/ports/*/state; do [ -r "$p" ] && grep -q 'ACTIVE' "$p" && echo x; done | wc -l | tr -d ' ')"
emit rdma_link_layers "$(for p in /sys/class/infiniband/*/ports/*/link_layer; do [ -r "$p" ] && cat "$p"; done | sort -u | paste -sd ',' -)"
emit rdma_rates "$(for p in /sys/class/infiniband/*/ports/*/rate; do [ -r "$p" ] && cat "$p"; done | sort -u | paste -sd ',' -)"
emit rdma_active_mtu "$(for p in /sys/class/infiniband/*/ports/*/active_mtu; do [ -r "$p" ] && cat "$p"; done | sort -u | paste -sd ',' -)"
gid_types=$(for p in /sys/class/infiniband/*/ports/*/gid_attrs/types/*; do [ -r "$p" ] && cat "$p"; done 2>/dev/null | sed '/^[[:space:]]*$/d' | sort -u | paste -sd ',' -)
emit rdma_gid_types "${gid_types:-unavailable}"
case "$gid_types" in
  *RoCE*)
    emit rdma_transport RoCE
    roce_versions=$(printf '%s\n' "$gid_types" | tr ',' '\n' | sed -n 's/.*RoCE v\([0-9][0-9]*\).*/v\1/p' | sort -Vu | paste -sd ',' -)
    emit roce_version "${roce_versions:-unavailable}"
    ;;
  *)
    link_layers=$(for p in /sys/class/infiniband/*/ports/*/link_layer; do [ -r "$p" ] && cat "$p"; done 2>/dev/null | sort -u | paste -sd ',' -)
    case "$link_layers" in
      *InfiniBand*) emit rdma_transport InfiniBand ;;
      *Ethernet*) emit rdma_transport "Ethernet RDMA (type unavailable)" ;;
      *) emit rdma_transport unavailable ;;
    esac
    emit roce_version unavailable
    ;;
esac
rdma_ifaces=$(command -v ibdev2netdev >/dev/null 2>&1 && ibdev2netdev 2>/dev/null | awk '$NF == "(Up)" {print $(NF-1)}' | sort -u)
pfc_state=unavailable
if [ -n "$rdma_ifaces" ] && command -v dcb >/dev/null 2>&1; then
  pfc_state=disabled
  for rdma_iface in $rdma_ifaces; do
    pfc_out=$(dcb pfc show dev "$rdma_iface" 2>/dev/null || true)
    [ -n "$pfc_out" ] || continue
    printf '%s\n' "$pfc_out" | grep -Eq 'prio-pfc.*:on([[:space:]]|$)' && { pfc_state=configured; break; }
  done
fi
emit roce_pfc "$pfc_state"
ecn_state=unavailable
if [ -n "$rdma_ifaces" ] && command -v tc >/dev/null 2>&1; then
  ecn_state="not detected"
  for rdma_iface in $rdma_ifaces; do
    tc qdisc show dev "$rdma_iface" 2>/dev/null | grep -Eqi '(^|[[:space:]])ecn([[:space:]]|$)' && { ecn_state=configured; break; }
  done
fi
emit roce_ecn "$ecn_state"
command -v ibv_devinfo >/dev/null 2>&1 && emit rdma_tooling available || emit rdma_tooling unavailable
"#;

fn parse_peer_report(
    source: &str,
    peer: &str,
    address: &str,
    port: u16,
    output: &str,
) -> PeerReport {
    let metrics = output
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(key, value)| {
            (
                key.to_owned(),
                if value.is_empty() {
                    "unavailable".into()
                } else {
                    value.to_owned()
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let tcp_ok = metrics.get("tcp").is_some_and(|value| value == "ok");
    let loss = metrics
        .get("packet_loss_pct")
        .and_then(|value| value.parse::<f64>().ok());
    let status = if tcp_ok && loss.is_some_and(|value| value == 0.0) {
        "connected"
    } else if tcp_ok {
        "degraded"
    } else {
        "unreachable"
    };
    let mut observations = Vec::new();
    if !tcp_ok {
        observations.push(format!("TCP port {port} is not reachable from {source}"));
    }
    if let Some(loss) = loss.filter(|value| *value > 0.0) {
        observations.push(format!("ICMP packet loss is {loss}%"));
    }
    if let Some(latency) = metrics
        .get("latency_avg_ms")
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| *value > 2.0)
    {
        observations.push(format!(
            "Average latency is {latency} ms; collective communication may be latency-sensitive"
        ));
    }
    if metrics
        .get("control_path_mtu")
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|value| value < 9000)
    {
        observations.push(
            "Management/TCP path MTU is below 9000; verify MTU consistency separately from the RDMA fabric"
                .into(),
        );
    }
    if metrics
        .get("rdma_devices")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
        > 0
        && metrics
            .get("rdma_active_ports")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            == 0
    {
        observations.push("RDMA devices exist, but no active RDMA port was detected".into());
    }
    observations.push(
        "Throughput was not measured; pass --bandwidth to run an explicit active test".into(),
    );
    PeerReport {
        source: source.into(),
        peer: peer.into(),
        address: address.into(),
        port,
        status: status.into(),
        metrics,
        observations,
    }
}

fn print_peer_report(report: &PeerReport, color: bool) {
    println!(
        "\n{}: {} {} {} ({}:{})",
        paint(color, "1;36", "Peer connectivity"),
        paint(color, "1", &report.source),
        paint(color, "2", "->"),
        paint(color, "1", &report.peer),
        report.address,
        report.port
    );
    println!(
        "Status: {}",
        match report.status.as_str() {
            "connected" => paint(color, "1;32", "CONNECTED"),
            "degraded" => paint(color, "1;33", "DEGRADED"),
            _ => paint(color, "1;31", "UNREACHABLE"),
        }
    );
    let rows = report
        .metrics
        .iter()
        .map(|(key, value)| vec![key.clone(), value.clone()])
        .collect::<Vec<_>>();
    print!("{}", table::render(&["METRIC", "VALUE"], &rows));
    if !report.observations.is_empty() {
        println!("\nObservations:");
        for item in &report.observations {
            println!("  • {item}");
        }
    }
}

pub fn diff(args: DiffArgs, use_password: bool, json: bool) -> io::Result<u8> {
    let password = prompt_password(use_password)?;
    let runtime = runtime()?;
    let progress = binport::progress::TaskProgress::new(
        format!("Comparing {} <-> {} · connecting", args.left, args.right),
        !json,
    );
    let left_progress = progress.clone();
    let right_progress = progress.clone();
    let result = runtime.block_on(async {
        tokio::try_join!(
            async {
                let result = collect(&args.left, password.as_deref()).await;
                if result.is_ok() {
                    left_progress.set_message(format!(
                        "Collected {} · waiting for {}",
                        args.left, args.right
                    ));
                }
                result
            },
            async {
                let result = collect(&args.right, password.as_deref()).await;
                if result.is_ok() {
                    right_progress.set_message(format!(
                        "Collected {} · waiting for {}",
                        args.right, args.left
                    ));
                }
                result
            }
        )
    });
    progress.finish();
    let (left, right) = result?;
    let left = filter(left, &args.section);
    let right = filter(right, &args.section);
    let differences = compare(&left, &right, args.all);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "left": left.host, "right": right.host, "differences": differences,
                "raw_values": { "left": left.raw_values, "right": right.raw_values }
            }))
            .map_err(io::Error::other)?
        );
    } else {
        let color = colors_enabled();
        println!(
            "{}: {} {} {}\n",
            paint(color, "1;36", "Environment comparison"),
            paint(color, "1;31", &left.host),
            paint(color, "2", "<->"),
            paint(color, "1;32", &right.host)
        );
        let rows = differences
            .iter()
            .map(|item| {
                vec![
                    paint(color, "36", &item.section),
                    paint(color, "1", &item.field),
                    paint(color, "31", item.left.as_deref().unwrap_or("-")),
                    paint(color, "32", item.right.as_deref().unwrap_or("-")),
                ]
            })
            .collect::<Vec<_>>();
        if rows.is_empty() {
            println!("No differences found");
        } else {
            print!(
                "{}",
                table::render(
                    &[
                        &paint(color, "1", "SECTION"),
                        &paint(color, "1", "FIELD"),
                        &paint(color, "1;31", &left.host),
                        &paint(color, "1;32", &right.host),
                    ],
                    &rows,
                )
            );
            let changed = differences.iter().filter(|item| !item.equal).count();
            println!(
                "\n{}: {} difference{}",
                paint(color, "1;33", "Summary"),
                paint(color, "1;33", &changed.to_string()),
                if changed == 1 { "" } else { "s" }
            );
        }
    }
    Ok(0)
}

async fn collect(target: &str, password: Option<&str>) -> io::Result<EnvironmentSnapshot> {
    let (status, stdout, stderr) = capture_remote(target, PROBE.to_owned(), password).await?;
    if status != 0 {
        return Err(io::Error::other(format!(
            "environment probe failed on {target}: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    parse(target, &String::from_utf8_lossy(&stdout))
}

fn parse(host: &str, output: &str) -> io::Result<EnvironmentSnapshot> {
    let mut values: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut raw_values: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for line in output.lines() {
        let Some((key, value)) = line.split_once('\t') else {
            continue;
        };
        let Some((section, field)) = key.split_once('.') else {
            continue;
        };
        let (field, display, raw) = normalize_value(field, value.trim());
        values
            .entry(section.to_owned())
            .or_default()
            .insert(field.clone(), display);
        if let Some(raw) = raw {
            raw_values
                .entry(section.to_owned())
                .or_default()
                .insert(field, raw);
        }
    }
    if values.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "environment probe returned no data",
        ));
    }
    Ok(EnvironmentSnapshot {
        host: host.to_owned(),
        values,
        raw_values,
    })
}

fn filter(mut snapshot: EnvironmentSnapshot, sections: &[String]) -> EnvironmentSnapshot {
    if !sections.is_empty() {
        snapshot
            .values
            .retain(|key, _| sections.iter().any(|section| section == key));
        snapshot
            .raw_values
            .retain(|key, _| sections.iter().any(|section| section == key));
    }
    snapshot
}

fn normalize_value(field: &str, value: &str) -> (String, String, Option<u64>) {
    let (name, multiplier, suffix) = match field {
        "memory_kib" => ("memory", 1024_u64, None),
        "memory_available_kib" => ("memory_available", 1024, None),
        "swap_kib" => ("swap", 1024, None),
        "disk_free_kib" => ("disk_free", 1024, None),
        "shm_kib" => ("shm", 1024, None),
        "cgroup_memory_limit_bytes" if value == "max" => {
            return ("cgroup_memory_limit".into(), "unlimited".into(), None);
        }
        "cgroup_memory_limit_bytes" => ("cgroup_memory_limit", 1, None),
        "cgroup_cpu_quota" if value.starts_with("max ") => {
            return (field.to_owned(), "unlimited".into(), None);
        }
        "cgroup_cpu_quota" if value.starts_with("-1/") => {
            return (field.to_owned(), "unlimited".into(), None);
        }
        "moore_threads_vram_mib" => ("moore_threads_vram", 1024 * 1024, Some(" per GPU")),
        "hygon_dcu_vram_mib" => ("hygon_dcu_vram", 1024 * 1024, Some(" per DCU")),
        "speed_mbps" => {
            let display = value.parse::<u64>().map_or_else(
                |_| "unavailable".to_owned(),
                |speed| {
                    if speed >= 1000 {
                        format!("{:.2} Gbps", speed as f64 / 1000.0)
                    } else {
                        format!("{speed} Mbps")
                    }
                },
            );
            return ("speed".into(), display, None);
        }
        _ => {
            return (
                field.to_owned(),
                if value.is_empty() {
                    "unavailable".to_owned()
                } else {
                    value.to_owned()
                },
                None,
            );
        }
    };
    if value.is_empty() {
        return (name.to_owned(), "unavailable".to_owned(), None);
    }
    let Ok(raw) = value
        .parse::<u64>()
        .map(|value| value.saturating_mul(multiplier))
    else {
        return (name.to_owned(), "unavailable".to_owned(), None);
    };
    if field == "cgroup_memory_limit_bytes" && raw >= (1_u64 << 60) {
        return (name.to_owned(), "unlimited".into(), None);
    }
    (
        name.to_owned(),
        format!("{}{}", human_bytes(raw), suffix.unwrap_or_default()),
        Some(raw),
    )
}

fn human_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    const TIB: f64 = GIB * 1024.0;
    let value = bytes as f64;
    if value >= TIB {
        format!("{:.2} TiB", value / TIB)
    } else if value >= GIB {
        format!("{:.2} GiB", value / GIB)
    } else if value >= MIB {
        format!("{:.2} MiB", value / MIB)
    } else if value >= KIB {
        format!("{:.2} KiB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn compare(left: &EnvironmentSnapshot, right: &EnvironmentSnapshot, all: bool) -> Vec<Difference> {
    let sections = left
        .values
        .keys()
        .chain(right.values.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut output = Vec::new();
    for section in sections {
        let empty = BTreeMap::new();
        let left_fields = left.values.get(&section).unwrap_or(&empty);
        let right_fields = right.values.get(&section).unwrap_or(&empty);
        let fields = left_fields
            .keys()
            .chain(right_fields.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        for field in fields {
            let l = left_fields.get(&field).cloned();
            let r = right_fields.get(&field).cloned();
            let equal = l == r;
            if all || !equal {
                output.push(Difference {
                    section: section.clone(),
                    field,
                    left: l,
                    right: r,
                    equal,
                });
            }
        }
    }
    output
}

fn snapshot_table(snapshot: &EnvironmentSnapshot, color: bool) -> String {
    let rows = snapshot
        .values
        .iter()
        .flat_map(|(section, fields)| {
            fields.iter().map(move |(field, value)| {
                let value = if value == "unavailable" {
                    paint(color, "2;33", value)
                } else {
                    value.clone()
                };
                vec![paint(color, "36", section), paint(color, "1", field), value]
            })
        })
        .collect::<Vec<_>>();
    table::render(
        &[
            &paint(color, "1", "SECTION"),
            &paint(color, "1", "FIELD"),
            &paint(color, "1", "VALUE"),
        ],
        &rows,
    )
}

fn colors_enabled() -> bool {
    io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map_or(true, |term| term != "dumb")
}

fn paint(enabled: bool, code: &str, value: &str) -> String {
    if enabled {
        format!("\u{1b}[{code}m{value}\u{1b}[0m")
    } else {
        value.to_owned()
    }
}

fn prompt_password(enabled: bool) -> io::Result<Option<String>> {
    enabled
        .then(|| rpassword::prompt_password("SSH password: "))
        .transpose()
}

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Runtime::new().map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_compares_snapshots() {
        let a = parse("a", "system.os\tLinux A\nsystem.arch\tx86_64\n").unwrap();
        let b = parse("b", "system.os\tLinux B\nsystem.arch\tx86_64\n").unwrap();
        let differences = compare(&a, &b, false);
        assert_eq!(differences.len(), 1);
        assert_eq!(differences[0].field, "os");
    }

    #[test]
    fn filters_sections() {
        let snapshot = parse("a", "system.os\tLinux\nruntime.python\t3.12\n").unwrap();
        let snapshot = filter(snapshot, &["runtime".into()]);
        assert_eq!(snapshot.values.keys().collect::<Vec<_>>(), [&"runtime"]);
    }

    #[test]
    fn formats_sizes_and_preserves_raw_bytes() {
        let snapshot = parse(
            "a",
            "resources.memory_kib\t1048576\naccelerator.moore_threads_vram_mib\t81920\n",
        )
        .unwrap();
        assert_eq!(snapshot.values["resources"]["memory"], "1.00 GiB");
        assert_eq!(
            snapshot.values["accelerator"]["moore_threads_vram"],
            "80.00 GiB per GPU"
        );
        assert_eq!(
            snapshot.raw_values["resources"]["memory"],
            1024 * 1024 * 1024
        );
    }

    #[test]
    fn formats_hygon_dcu_memory_and_compares_hygon_fields() {
        let a = parse(
            "a",
            "accelerator.hygon_dcu_products\tBW1102\naccelerator.hygon_dcu_count\t8\naccelerator.hygon_dcu_vram_mib\t147440\naccelerator.dtk\t26.04\n",
        )
        .unwrap();
        let b = parse(
            "b",
            "accelerator.hygon_dcu_products\tBW1102\naccelerator.hygon_dcu_count\t4\naccelerator.hygon_dcu_vram_mib\t147440\naccelerator.dtk\t25.04\n",
        )
        .unwrap();
        assert_eq!(
            a.values["accelerator"]["hygon_dcu_vram"],
            "143.98 GiB per DCU"
        );
        let differences = compare(&a, &b, false);
        assert!(
            differences
                .iter()
                .any(|item| item.field == "hygon_dcu_count")
        );
        assert!(differences.iter().any(|item| item.field == "dtk"));
    }

    #[test]
    fn normalizes_unlimited_cgroup_quotas() {
        let snapshot = parse(
            "a",
            "configuration.cgroup_cpu_quota\t-1/100000\nconfiguration.cgroup_memory_limit_bytes\tmax\n",
        )
        .unwrap();
        assert_eq!(
            snapshot.values["configuration"]["cgroup_cpu_quota"],
            "unlimited"
        );
        assert_eq!(
            snapshot.values["configuration"]["cgroup_memory_limit"],
            "unlimited"
        );
    }

    #[test]
    fn classifies_peer_connectivity_and_training_risks() {
        let report = parse_peer_report(
            "worker-a",
            "worker-b",
            "10.0.0.2",
            22,
            "tcp\tok\npacket_loss_pct\t0\nlatency_avg_ms\t2.5\ncontrol_path_mtu\t1500\nrdma_devices\t2\nrdma_active_ports\t0\n",
        );
        assert_eq!(report.status, "connected");
        assert!(
            report
                .observations
                .iter()
                .any(|item| item.contains("latency"))
        );
        assert!(report.observations.iter().any(|item| item.contains("MTU")));
        assert!(
            report
                .observations
                .iter()
                .any(|item| item.contains("no active RDMA"))
        );
    }

    #[test]
    fn marks_failed_tcp_as_unreachable() {
        let report = parse_peer_report(
            "worker-a",
            "worker-b",
            "10.0.0.2",
            22,
            "tcp\tfailed\npacket_loss_pct\t100\n",
        );
        assert_eq!(report.status, "unreachable");
    }

    #[test]
    fn preserves_roce_transport_and_fabric_signals() {
        let report = parse_peer_report(
            "worker-a",
            "worker-b",
            "198.18.0.2",
            22,
            "tcp\tok\npacket_loss_pct\t0\nrdma_transport\tRoCE\nrdma_gid_types\tRoCE v1,RoCE v2\nroce_version\tv1,v2\nroce_pfc\tconfigured\nroce_ecn\tnot detected\n",
        );
        assert_eq!(report.metrics["rdma_transport"], "RoCE");
        assert_eq!(report.metrics["roce_version"], "v1,v2");
        assert_eq!(report.metrics["roce_pfc"], "configured");
        assert_eq!(report.metrics["roce_ecn"], "not detected");
    }
}
