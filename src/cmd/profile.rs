use super::native_exec::capture_remote;
use super::table;
use clap::Args;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io;

#[derive(Debug, Args)]
pub struct ProfileArgs {
    /// SSH host configured in binport or ~/.ssh/config
    target: String,
    /// Sampling duration in seconds
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=3600))]
    duration: u64,
    /// Sampling interval in seconds
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u64).range(1..=300))]
    interval: u64,
}

#[derive(Debug, Serialize)]
struct ProfileReport {
    host: String,
    duration_seconds: u64,
    interval_seconds: u64,
    samples: usize,
    metrics: BTreeMap<String, MetricSummary>,
    observations: Vec<String>,
}

#[derive(Debug, Serialize)]
struct MetricSummary {
    average: f64,
    peak: f64,
    unit: &'static str,
}

pub fn run(args: ProfileArgs, use_password: bool, json: bool) -> io::Result<u8> {
    if args.interval > args.duration {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--interval cannot exceed --duration",
        ));
    }
    let password = use_password
        .then(|| rpassword::prompt_password("SSH password: "))
        .transpose()?;
    let samples = args.duration.div_ceil(args.interval);
    let script = probe_script(samples, args.interval);
    let progress = binport::progress::TaskProgress::new(
        format!(
            "Profiling {} · {} samples every {}s",
            args.target, samples, args.interval
        ),
        !json,
    );
    let result = tokio::runtime::Runtime::new()
        .map_err(io::Error::other)?
        .block_on(capture_remote(&args.target, script, password.as_deref()));
    progress.finish();
    let (status, stdout, stderr) = result?;
    if status != 0 {
        return Err(io::Error::other(format!(
            "profile failed on {}: {}",
            args.target,
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    let report = summarize(
        &args.target,
        args.duration,
        args.interval,
        &String::from_utf8_lossy(&stdout),
    )?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(io::Error::other)?
        );
    } else {
        print_report(&report);
    }
    Ok(0)
}

fn probe_script(samples: u64, interval: u64) -> String {
    format!(
        r#"read_cpu() {{ awk '/^cpu / {{idle=$5+$6; total=0; for(i=2;i<=NF;i++) total+=$i; print total, idle; exit}}' /proc/stat; }}
read_disk() {{ awk '$3 ~ /^(sd[a-z]+|vd[a-z]+|xvd[a-z]+|nvme[0-9]+n[0-9]+)$/ {{r+=$6; w+=$10}} END {{print r+0, w+0}}' /proc/diskstats; }}
read_net() {{ awk -F'[: ]+' '$1 !~ /lo/ && NF>10 {{rx+=$3; tx+=$11}} END {{print rx+0, tx+0}}' /proc/net/dev; }}
hy_smi="$(command -v hy-smi 2>/dev/null || true)"
if [ -z "$hy_smi" ]; then for p in /opt/hyhal/bin/hy-smi /opt/dtk/bin/hy-smi /opt/hygondtk/bin/hy-smi; do [ -x "$p" ] && {{ hy_smi="$p"; break; }}; done; fi
mt_gmi="$(command -v mthreads-gmi 2>/dev/null || true)"
if [ -z "$mt_gmi" ]; then for p in /usr/local/bin/mthreads-gmi /usr/bin/mthreads-gmi; do [ -x "$p" ] && {{ mt_gmi="$p"; break; }}; done; fi
set -- $(read_cpu); prev_cpu=$1; prev_idle=$2
set -- $(read_disk); prev_read=$1; prev_write=$2
set -- $(read_net); prev_rx=$1; prev_tx=$2
i=1
while [ "$i" -le {samples} ]; do
  sleep {interval}
  set -- $(read_cpu); cpu=$1; idle=$2; dt=$((cpu-prev_cpu)); di=$((idle-prev_idle)); cpu_pct=$(awk -v t="$dt" -v i="$di" 'BEGIN{{if(t>0) printf "%.2f", 100*(t-i)/t; else print 0}}'); prev_cpu=$cpu; prev_idle=$idle
  mem_pct=$(awk '/MemTotal:/{{t=$2}} /MemAvailable:/{{a=$2}} END{{if(t>0) printf "%.2f",100*(t-a)/t; else print 0}}' /proc/meminfo)
  load1=$(awk '{{print $1}}' /proc/loadavg)
  set -- $(read_disk); rd=$1; wr=$2; read_mib=$(awk -v n="$((rd-prev_read))" -v s="{interval}" 'BEGIN{{printf "%.2f",n*512/1048576/s}}'); write_mib=$(awk -v n="$((wr-prev_write))" -v s="{interval}" 'BEGIN{{printf "%.2f",n*512/1048576/s}}'); prev_read=$rd; prev_write=$wr
  set -- $(read_net); rx=$1; tx=$2; rx_mib=$(awk -v n="$((rx-prev_rx))" -v s="{interval}" 'BEGIN{{printf "%.2f",n/1048576/s}}'); tx_mib=$(awk -v n="$((tx-prev_tx))" -v s="{interval}" 'BEGIN{{printf "%.2f",n/1048576/s}}'); prev_rx=$rx; prev_tx=$tx
  accel=none; util=; vram=; temp=; power=
  if command -v nvidia-smi >/dev/null 2>&1; then
    accel=nvidia; set -- $(nvidia-smi --query-gpu=utilization.gpu,utilization.memory,temperature.gpu,power.draw --format=csv,noheader,nounits 2>/dev/null | awk -F, '{{u+=$1;m+=$2;t+=$3;p+=$4;n++}} END{{if(n) printf "%.2f %.2f %.2f %.2f",u/n,m/n,t/n,p/n}}'); util=$1; vram=$2; temp=$3; power=$4
  elif [ -n "$hy_smi" ]; then
    accel=hygon; set -- $($hy_smi 2>/dev/null | awk '/^[0-9]+[[:space:]]/ {{gsub(/[CW%]/,"",$2); gsub(/[CW%]/,"",$3); gsub(/%/,"",$6); gsub(/%/,"",$7); t+=$2;p+=$3;m+=$6;u+=$7;n++}} END{{if(n) printf "%.2f %.2f %.2f %.2f",u/n,m/n,t/n,p/n}}'); util=$1; vram=$2; temp=$3; power=$4
  elif [ -n "$mt_gmi" ]; then
    accel=moore_threads; set -- $($mt_gmi -cf 2>/dev/null | awk '/^[0-9]+[[:space:]]/ && /MiB\(/ {{line=$0; sub(/^.*\|/,"",line); gsub(/[(),]/," ",line); count=split(line,a,/[[:space:]]+/); pc=0; mc=0; for(j=1;j<=count;j++) {{x=a[j]; if(x ~ /^[0-9.]+%$/) {{gsub(/%/,"",x); pct[++pc]=x}} else if(x ~ /^[0-9.]+MiB$/) {{gsub(/MiB/,"",x); mem[++mc]=x}}}} if(pc) u+=pct[1]; if(mc>=2 && mem[2]>0) m+=100*mem[1]/mem[2]; n++; delete pct; delete mem}} /Physical/ {{line=$0; gsub(/[|]/," ",line); count=split(line,a,/[[:space:]]+/); for(j=1;j<=count;j++) if(a[j] ~ /^[0-9.]+C$/) {{gsub(/C/,"",a[j]); t+=a[j]; tn++; break}}}} END{{if(n) printf "%.2f %.2f %s -",u/n,m/n,(tn ? sprintf("%.2f",t/tn) : "-")}}'); util=$1; vram=$2; temp=$3; power=$4
  fi
  printf 'sample\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$i" "$cpu_pct" "$mem_pct" "$load1" "$read_mib" "$write_mib" "$rx_mib" "$tx_mib" "$accel" "${{util:--}}" "${{vram:--}}" "${{temp:--}}" "${{power:--}}"
  i=$((i+1))
done"#
    )
}

fn summarize(host: &str, duration: u64, interval: u64, output: &str) -> io::Result<ProfileReport> {
    let mut values: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
    let fields = [
        "cpu",
        "memory",
        "load1",
        "disk_read",
        "disk_write",
        "network_rx",
        "network_tx",
    ];
    let mut accelerator = None;
    let mut count = 0;
    for line in output.lines().filter(|line| line.starts_with("sample\t")) {
        let columns = line.split('\t').collect::<Vec<_>>();
        if columns.len() != 14 {
            continue;
        }
        count += 1;
        for (field, column) in fields.iter().zip(&columns[2..9]) {
            if let Ok(value) = column.parse() {
                values.entry(field).or_default().push(value);
            }
        }
        if columns[9] != "none" {
            accelerator = Some(columns[9].to_owned());
            for (field, column) in [
                "accelerator_util",
                "accelerator_memory",
                "accelerator_temp",
                "accelerator_power",
            ]
            .iter()
            .zip(&columns[10..14])
            {
                if let Ok(value) = column.parse() {
                    values.entry(field).or_default().push(value);
                }
            }
        }
    }
    if count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "profile returned no samples: {}",
                output.trim().chars().take(240).collect::<String>()
            ),
        ));
    }
    let mut metrics = BTreeMap::new();
    for (name, samples) in values {
        let unit = match name {
            "load1" => "",
            "disk_read" | "disk_write" | "network_rx" | "network_tx" => "MiB/s",
            "accelerator_temp" => "°C",
            "accelerator_power" => "W",
            _ => "%",
        };
        metrics.insert(
            name.to_owned(),
            MetricSummary {
                average: samples.iter().sum::<f64>() / samples.len() as f64,
                peak: samples.iter().copied().fold(0.0, f64::max),
                unit,
            },
        );
    }
    let observations = observations(&metrics, accelerator.as_deref());
    Ok(ProfileReport {
        host: host.to_owned(),
        duration_seconds: duration,
        interval_seconds: interval,
        samples: count,
        metrics,
        observations,
    })
}

fn observations(
    metrics: &BTreeMap<String, MetricSummary>,
    accelerator: Option<&str>,
) -> Vec<String> {
    let avg = |name: &str| metrics.get(name).map_or(0.0, |metric| metric.average);
    let peak = |name: &str| metrics.get(name).map_or(0.0, |metric| metric.peak);
    let mut output = Vec::new();
    if peak("memory") >= 90.0 {
        output.push("Host memory pressure is high (peak >= 90%).".into());
    }
    if let Some(kind) = accelerator {
        if metrics.contains_key("accelerator_util") && avg("accelerator_util") < 5.0 {
            output.push(format!(
                "{kind} accelerator was idle during this sampling window."
            ));
        } else if avg("accelerator_util") < 40.0 && avg("cpu") >= 70.0 {
            output.push(format!("Low {kind} utilization with high CPU usage; input processing or CPU work may be limiting throughput."));
        } else if avg("accelerator_util") >= 85.0 {
            output.push(format!("{kind} compute utilization is consistently high."));
        }
        if peak("accelerator_memory") >= 90.0 {
            output.push(format!("{kind} memory pressure is high (peak >= 90%)."));
        }
    }
    if output.is_empty() {
        output.push("No obvious saturation detected in this sampling window.".into());
    }
    output
}

fn print_report(report: &ProfileReport) {
    println!("Profile: {} · {} samples\n", report.host, report.samples);
    let rows = report
        .metrics
        .iter()
        .map(|(name, metric)| {
            vec![
                name.clone(),
                format!("{:.2} {}", metric.average, metric.unit),
                format!("{:.2} {}", metric.peak, metric.unit),
            ]
        })
        .collect::<Vec<_>>();
    print!("{}", table::render(&["METRIC", "AVERAGE", "PEAK"], &rows));
    println!("\nObservations:");
    for observation in &report.observations {
        println!("  • {observation}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_samples_and_detects_cpu_bound_accelerator_work() {
        let output = "sample\t1\t80\t50\t2\t1\t2\t3\t4\thygon\t20\t30\t40\t100\n\
                      sample\t2\t90\t60\t3\t2\t4\t6\t8\thygon\t30\t40\t50\t120\n";
        let report = summarize("hg", 2, 1, output).unwrap();
        assert_eq!(report.samples, 2);
        assert_eq!(report.metrics["cpu"].average, 85.0);
        assert!(report.observations[0].contains("Low hygon utilization"));
    }

    #[test]
    fn omits_unavailable_power_and_reports_idle_accelerator() {
        let output = "sample\t1\t1\t4\t8\t0\t1\t0\t0\tmoore_threads\t0\t0\t35\t-\n";
        let report = summarize("mtt", 1, 1, output).unwrap();
        assert!(!report.metrics.contains_key("accelerator_power"));
        assert_eq!(report.metrics["accelerator_temp"].average, 35.0);
        assert!(report.observations[0].contains("was idle"));
    }
}
