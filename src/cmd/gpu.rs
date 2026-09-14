use super::native_exec::capture_remote;
use std::collections::BTreeMap;
use std::io;

const GPU_P2P_BENCHMARK: &str = r#"import ctypes, ctypes.util, shutil, subprocess, time
if shutil.which("nvidia-smi"):
    vendor, prefix, runtime_api = "NVIDIA", "cu", False
    library = ctypes.util.find_library("cuda") or "libcuda.so.1"
    topology_command = ["nvidia-smi", "topo", "-m"]
elif shutil.which("mthreads-gmi"):
    vendor, prefix, runtime_api = "Moore Threads", "mu", False
    library = ctypes.util.find_library("musa") or "libmusa.so.1"
    topology_command = ["mthreads-gmi", "topo", "-mg"]
elif shutil.which("hy-smi"):
    vendor, prefix, runtime_api = "Hygon DCU", "hip", True
    library = ctypes.util.find_library("amdhip64") or "libamdhip64.so"
    topology_command = []
else:
    raise RuntimeError("no supported NVIDIA, Moore Threads, or Hygon accelerator was detected")
driver = ctypes.CDLL(library)

def call(name, *args):
    code = getattr(driver, prefix + name)(*args)
    if code != 0:
        raise RuntimeError(f"{prefix + name} failed with driver error {code}")

count = ctypes.c_int()
if runtime_api:
    call("Init", 0)
    call("GetDeviceCount", ctypes.byref(count))
else:
    call("Init", 0)
    call("DeviceGetCount", ctypes.byref(count))
if count.value < 2:
    raise RuntimeError("at least two accelerator devices are required")

contexts = []
devices = []
for gpu in range(count.value):
    if runtime_api:
        devices.append(gpu)
        contexts.append(gpu)
    else:
        device = ctypes.c_int()
        context = ctypes.c_void_p()
        call("DeviceGet", ctypes.byref(device), gpu)
        call("DevicePrimaryCtxRetain", ctypes.byref(context), device)
        devices.append(device)
        contexts.append(context)

paths = {}
try:
    topo = subprocess.check_output(topology_command, text=True, stderr=subprocess.DEVNULL) if topology_command else ""
    for line in topo.splitlines():
        columns = line.split()
        if columns and columns[0].startswith("GPU") and columns[0][3:].isdigit():
            left = int(columns[0][3:])
            for right in range(count.value):
                if right + 1 < len(columns) and columns[right + 1] != "X":
                    paths[(left, right)] = columns[right + 1]
except Exception:
    pass

size = 256 * 1024 * 1024
iterations = 8

def select(context):
    call("SetDevice" if runtime_api else "CtxSetCurrent", context)

def allocate(context):
    select(context)
    pointer = ctypes.c_void_p() if runtime_api else ctypes.c_uint64()
    call("Malloc" if runtime_api else "MemAlloc_v2", ctypes.byref(pointer), ctypes.c_size_t(size))
    return pointer

def free(context, pointer):
    select(context)
    call("Free" if runtime_api else "MemFree_v2", pointer)

def bandwidth(source, destination):
    can_access = ctypes.c_int()
    call("DeviceCanAccessPeer", ctypes.byref(can_access), devices[destination], devices[source])
    if not can_access.value:
        return None
    src_ctx, dst_ctx = contexts[source], contexts[destination]
    src = allocate(src_ctx)
    dst = allocate(dst_ctx)
    try:
        select(dst_ctx)
        enable = "DeviceEnablePeerAccess" if runtime_api else "CtxEnablePeerAccess"
        code = getattr(driver, prefix + enable)(src_ctx, 0)
        if code not in (0, 704):
            raise RuntimeError(f"{prefix + enable} failed with driver error {code}")
        for _ in range(2):
            call("MemcpyPeer", dst, destination, src, source, ctypes.c_size_t(size)) if runtime_api else call("MemcpyPeer", dst, dst_ctx, src, src_ctx, ctypes.c_size_t(size))
        call("DeviceSynchronize" if runtime_api else "CtxSynchronize")
        started = time.perf_counter()
        for _ in range(iterations):
            call("MemcpyPeer", dst, destination, src, source, ctypes.c_size_t(size)) if runtime_api else call("MemcpyPeer", dst, dst_ctx, src, src_ctx, ctypes.c_size_t(size))
        call("DeviceSynchronize" if runtime_api else "CtxSynchronize")
        elapsed = time.perf_counter() - started
        return size * iterations / elapsed / 1e9
    finally:
        free(dst_ctx, dst)
        free(src_ctx, src)

values = []
print(f"driver_api\t{vendor} {prefix}* Driver API")
for left in range(count.value):
    for right in range(left + 1, count.value):
        forward = bandwidth(left, right)
        reverse = bandwidth(right, left)
        if forward is None or reverse is None:
            print(f"pair_{left}_{right}\tunavailable (P2P disabled)")
        else:
            values.extend((forward, reverse))
            path = paths.get((left, right), "P2P path undetermined")
            print(f"pair_{left}_{right}\t{path} · {forward:.2f} / {reverse:.2f} GB/s (forward / reverse)")
if values:
    print(f"summary\t{len(values)//2} pairs · min {min(values):.2f} · avg {sum(values)/len(values):.2f} · max {max(values):.2f} GB/s")
"#;

pub async fn measure_gpu_p2p(
    target: &str,
    password: Option<&str>,
) -> io::Result<BTreeMap<String, String>> {
    let command = binport::execute_command("python3", &["-c".into(), GPU_P2P_BENCHMARK.into()])?;
    let (status, stdout, stderr) = tokio::time::timeout(
        std::time::Duration::from_secs(180),
        capture_remote(target, command, password),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "GPU bandwidth test timed out"))??;
    if status != 0 {
        return Err(io::Error::other(format!(
            "GPU bandwidth test failed on {target}: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    let metrics = parse_metrics(&String::from_utf8_lossy(&stdout));
    if metrics.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "GPU bandwidth test returned no measurements",
        ));
    }
    Ok(metrics)
}

fn parse_metrics(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pair_paths_and_summary() {
        let metrics = parse_metrics(
            "pair_0_1\tNV18 · 390.90 / 390.41 GB/s (forward / reverse)\nsummary\t1 pairs · min 390.41 GB/s\n",
        );
        assert!(metrics["pair_0_1"].starts_with("NV18"));
        assert!(metrics["summary"].contains("390.41"));
    }
}
