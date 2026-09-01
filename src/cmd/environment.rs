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
emit runtime.java "$(java -version 2>&1 | head -n 1)"
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

pub fn inspect(args: InspectArgs, use_password: bool, json: bool) -> io::Result<u8> {
    let password = prompt_password(use_password)?;
    let snapshot = runtime()?.block_on(collect(&args.target, password.as_deref()))?;
    let snapshot = filter(snapshot, &args.section);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&snapshot).map_err(io::Error::other)?
        );
    } else {
        let color = colors_enabled();
        println!(
            "{}: {}\n",
            paint(color, "1;36", "Environment"),
            paint(color, "1", &snapshot.host)
        );
        print!("{}", snapshot_table(&snapshot, color));
    }
    Ok(0)
}

pub fn diff(args: DiffArgs, use_password: bool, json: bool) -> io::Result<u8> {
    let password = prompt_password(use_password)?;
    let runtime = runtime()?;
    let (left, right) = runtime.block_on(async {
        tokio::try_join!(
            collect(&args.left, password.as_deref()),
            collect(&args.right, password.as_deref())
        )
    })?;
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
        "moore_threads_vram_mib" => ("moore_threads_vram", 1024 * 1024, Some(" per GPU")),
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
}
