use codex_cicd_artifacts::HostResourceSnapshot;
use codex_cicd_artifacts::ResourceFeasibility;
use codex_cicd_artifacts::ResourceFeasibilityStatus;
use codex_cicd_artifacts::ResourceUsageTotals;
use codex_cicd_artifacts::RunResourceUsage;
use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::Path;

const DEFAULT_CLOCK_TICKS_PER_SECOND: u64 = 100;
const DEFAULT_PAGE_SIZE_BYTES: u64 = 4096;

pub(crate) struct ResourceMonitor {
    run_id: String,
    host: HostResourceSnapshot,
    process: ProcessGroupMonitor,
    aggregate_peak_memory_bytes: u64,
}

impl ResourceMonitor {
    pub(crate) fn start(run_id: String, runner_pid: u32) -> Self {
        Self {
            host: host_snapshot(),
            process: ProcessGroupMonitor::new(runner_pid),
            run_id,
            aggregate_peak_memory_bytes: 0,
        }
    }

    pub(crate) fn poll(&mut self) {
        self.process.poll();
        self.aggregate_peak_memory_bytes = self
            .aggregate_peak_memory_bytes
            .max(self.process.current_memory_bytes());
    }

    pub(crate) fn finish(mut self) -> RunResourceUsage {
        self.poll();
        let process = self.process.finish();
        let totals = aggregate_totals(&process, self.aggregate_peak_memory_bytes);
        let feasibility = estimate_feasibility(&self.host, &totals);

        RunResourceUsage {
            run_id: self.run_id,
            host: self.host,
            process,
            containers: Vec::new(),
            totals,
            feasibility,
        }
    }
}

pub(crate) fn run_id_for_capture(arg: &str, now_micros: u128) -> String {
    let sanitized_arg = arg
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!(
        "codex-repo-ci-{sanitized_arg}-{}-{now_micros}",
        std::process::id()
    )
}

fn aggregate_totals(
    process: &ResourceUsageTotals,
    aggregate_peak_memory_bytes: u64,
) -> ResourceUsageTotals {
    ResourceUsageTotals {
        cpu_time_ms: process.cpu_time_ms,
        peak_memory_bytes: (aggregate_peak_memory_bytes > 0).then_some(aggregate_peak_memory_bytes),
    }
}

fn estimate_feasibility(
    host: &HostResourceSnapshot,
    totals: &ResourceUsageTotals,
) -> ResourceFeasibility {
    let Some(peak_memory_bytes) = totals.peak_memory_bytes else {
        return ResourceFeasibility {
            status: ResourceFeasibilityStatus::Unknown,
            reason: "resource monitor did not observe memory usage".to_string(),
            required_memory_bytes: None,
            memory_limit_bytes: host.memory_limit_bytes,
        };
    };
    let Some(memory_limit_bytes) = host.memory_limit_bytes else {
        return ResourceFeasibility {
            status: ResourceFeasibilityStatus::Unknown,
            reason: "memory limit was not available on this host".to_string(),
            required_memory_bytes: None,
            memory_limit_bytes: None,
        };
    };

    let required_memory_bytes = peak_memory_bytes.saturating_add(peak_memory_bytes / 4);
    if peak_memory_bytes > memory_limit_bytes {
        ResourceFeasibility {
            status: ResourceFeasibilityStatus::Insufficient,
            reason: "observed peak memory exceeded the current machine limit".to_string(),
            required_memory_bytes: Some(required_memory_bytes),
            memory_limit_bytes: Some(memory_limit_bytes),
        }
    } else if required_memory_bytes > memory_limit_bytes {
        ResourceFeasibility {
            status: ResourceFeasibilityStatus::Risky,
            reason: "observed peak memory fit, but a 25% headroom estimate does not".to_string(),
            required_memory_bytes: Some(required_memory_bytes),
            memory_limit_bytes: Some(memory_limit_bytes),
        }
    } else {
        ResourceFeasibility {
            status: ResourceFeasibilityStatus::LikelyRunnable,
            reason: "observed peak memory plus 25% headroom fits within the current machine limit"
                .to_string(),
            required_memory_bytes: Some(required_memory_bytes),
            memory_limit_bytes: Some(memory_limit_bytes),
        }
    }
}

struct ProcessGroupMonitor {
    process_group_id: u32,
    page_size_bytes: u64,
    clock_ticks_per_second: u64,
    max_cpu_jiffies_by_process: BTreeMap<ProcessKey, u64>,
    peak_memory_bytes: u64,
    current_memory_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ProcessKey {
    pid: u32,
    start_time_jiffies: u64,
}

impl ProcessGroupMonitor {
    fn new(process_group_id: u32) -> Self {
        Self {
            process_group_id,
            page_size_bytes: getconf_u64("PAGESIZE", DEFAULT_PAGE_SIZE_BYTES),
            clock_ticks_per_second: getconf_u64("CLK_TCK", DEFAULT_CLOCK_TICKS_PER_SECOND),
            max_cpu_jiffies_by_process: BTreeMap::new(),
            peak_memory_bytes: 0,
            current_memory_bytes: 0,
        }
    }

    fn poll(&mut self) {
        #[cfg(target_os = "linux")]
        self.poll_linux();
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (self.process_group_id, self.page_size_bytes);
        }
    }

    #[cfg(target_os = "linux")]
    fn poll_linux(&mut self) {
        let Ok(entries) = fs::read_dir("/proc") else {
            return;
        };
        let mut current_memory_bytes = 0_u64;
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(pid) = file_name
                .to_str()
                .and_then(|value| value.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            let Some(stat) = parse_proc_stat(&stat) else {
                continue;
            };
            if stat.process_group_id != self.process_group_id {
                continue;
            }
            let key = ProcessKey {
                pid,
                start_time_jiffies: stat.start_time_jiffies,
            };
            self.max_cpu_jiffies_by_process
                .entry(key)
                .and_modify(|max_cpu| *max_cpu = (*max_cpu).max(stat.cpu_jiffies))
                .or_insert(stat.cpu_jiffies);
            let rss_bytes = stat.rss_pages.saturating_mul(self.page_size_bytes);
            current_memory_bytes = current_memory_bytes.saturating_add(rss_bytes);
        }
        self.current_memory_bytes = current_memory_bytes;
        self.peak_memory_bytes = self.peak_memory_bytes.max(current_memory_bytes);
    }

    fn current_memory_bytes(&self) -> u64 {
        self.current_memory_bytes
    }

    fn finish(self) -> ResourceUsageTotals {
        let cpu_jiffies = self
            .max_cpu_jiffies_by_process
            .values()
            .copied()
            .fold(0_u64, u64::saturating_add);
        let cpu_time_ms = cpu_jiffies
            .saturating_mul(1000)
            .checked_div(self.clock_ticks_per_second.max(1))
            .unwrap_or(0);
        ResourceUsageTotals {
            cpu_time_ms: (cpu_time_ms > 0).then_some(cpu_time_ms),
            peak_memory_bytes: (self.peak_memory_bytes > 0).then_some(self.peak_memory_bytes),
        }
    }
}

#[cfg(target_os = "linux")]
struct ProcStat {
    process_group_id: u32,
    cpu_jiffies: u64,
    start_time_jiffies: u64,
    rss_pages: u64,
}

#[cfg(target_os = "linux")]
fn parse_proc_stat(value: &str) -> Option<ProcStat> {
    let right_paren = value.rfind(')')?;
    let fields = value
        .get(right_paren + 2..)?
        .split_whitespace()
        .collect::<Vec<_>>();
    let process_group_id = fields.get(2)?.parse().ok()?;
    let user_jiffies = fields.get(11)?.parse::<u64>().ok()?;
    let system_jiffies = fields.get(12)?.parse::<u64>().ok()?;
    let start_time_jiffies = fields.get(19)?.parse().ok()?;
    let rss_pages = fields.get(21)?.parse::<i64>().ok()?.max(0) as u64;
    Some(ProcStat {
        process_group_id,
        cpu_jiffies: user_jiffies.saturating_add(system_jiffies),
        start_time_jiffies,
        rss_pages,
    })
}

fn getconf_u64(name: &str, fallback: u64) -> u64 {
    let Ok(output) = std::process::Command::new("getconf").arg(name).output() else {
        return fallback;
    };
    if !output.status.success() {
        return fallback;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(fallback)
}

fn host_snapshot() -> HostResourceSnapshot {
    let cpu_count = std::thread::available_parallelism()
        .ok()
        .and_then(|count| u64::try_from(count.get()).ok());
    let (memory_total_bytes, memory_available_bytes) = read_meminfo();
    let cgroup_limit = cgroup_memory_limit_bytes();
    let memory_limit_bytes = match (memory_total_bytes, cgroup_limit) {
        (Some(total), Some(limit)) => Some(total.min(limit)),
        (Some(total), None) => Some(total),
        (None, Some(limit)) => Some(limit),
        (None, None) => None,
    };
    HostResourceSnapshot {
        cpu_count,
        memory_total_bytes,
        memory_available_bytes,
        memory_limit_bytes,
    }
}

#[cfg(target_os = "linux")]
fn read_meminfo() -> (Option<u64>, Option<u64>) {
    let Ok(meminfo) = fs::read_to_string("/proc/meminfo") else {
        return (None, None);
    };
    let mut total = None;
    let mut available = None;
    for line in meminfo.lines() {
        if let Some(value) = line.strip_prefix("MemTotal:") {
            total = parse_meminfo_kib(value);
        } else if let Some(value) = line.strip_prefix("MemAvailable:") {
            available = parse_meminfo_kib(value);
        }
    }
    (total, available)
}

#[cfg(not(target_os = "linux"))]
fn read_meminfo() -> (Option<u64>, Option<u64>) {
    (None, None)
}

#[cfg(target_os = "linux")]
fn parse_meminfo_kib(value: &str) -> Option<u64> {
    let kib = value.split_whitespace().next()?.parse::<u64>().ok()?;
    kib.checked_mul(1024)
}

#[cfg(target_os = "linux")]
fn cgroup_memory_limit_bytes() -> Option<u64> {
    cgroup_v2_memory_limit_bytes().or_else(cgroup_v1_memory_limit_bytes)
}

#[cfg(not(target_os = "linux"))]
fn cgroup_memory_limit_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn cgroup_v2_memory_limit_bytes() -> Option<u64> {
    for line in fs::read_to_string("/proc/self/cgroup").ok()?.lines() {
        let mut parts = line.splitn(3, ':');
        let hierarchy = parts.next()?;
        let controllers = parts.next()?;
        let cgroup_path = parts.next()?;
        if hierarchy == "0" && controllers.is_empty() {
            return read_cgroup_limit(Path::new("/sys/fs/cgroup"), cgroup_path, "memory.max");
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn cgroup_v1_memory_limit_bytes() -> Option<u64> {
    for line in fs::read_to_string("/proc/self/cgroup").ok()?.lines() {
        let mut parts = line.splitn(3, ':');
        let _hierarchy = parts.next()?;
        let controllers = parts.next()?;
        let cgroup_path = parts.next()?;
        if controllers
            .split(',')
            .any(|controller| controller == "memory")
        {
            return read_cgroup_limit(
                Path::new("/sys/fs/cgroup/memory"),
                cgroup_path,
                "memory.limit_in_bytes",
            )
            .or_else(|| {
                read_cgroup_limit(
                    Path::new("/sys/fs/cgroup"),
                    cgroup_path,
                    "memory.limit_in_bytes",
                )
            });
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn read_cgroup_limit(root: &Path, cgroup_path: &str, file_name: &str) -> Option<u64> {
    let relative = cgroup_path.trim_start_matches('/');
    let path = if relative.is_empty() {
        root.join(file_name)
    } else {
        root.join(relative).join(file_name)
    };
    let value = fs::read_to_string(path).ok()?;
    parse_cgroup_limit(&value)
}

#[cfg(target_os = "linux")]
fn parse_cgroup_limit(value: &str) -> Option<u64> {
    let value = value.trim();
    if value == "max" {
        return None;
    }
    let limit = value.parse::<u64>().ok()?;
    (limit < i64::MAX as u64).then_some(limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn run_id_for_capture_sanitizes_mode() {
        let run_id = run_id_for_capture("Fast / Debug", /*now_micros*/ 123);

        assert!(run_id.starts_with("codex-repo-ci-fast---debug-"));
        assert!(run_id.ends_with("-123"));
    }

    #[test]
    fn estimate_feasibility_reports_memory_risk() {
        let host = HostResourceSnapshot {
            cpu_count: Some(4),
            memory_total_bytes: Some(1_000),
            memory_available_bytes: Some(500),
            memory_limit_bytes: Some(1_000),
        };

        assert_eq!(
            estimate_feasibility(
                &host,
                &ResourceUsageTotals {
                    cpu_time_ms: None,
                    peak_memory_bytes: Some(900),
                },
            ),
            ResourceFeasibility {
                status: ResourceFeasibilityStatus::Risky,
                reason: "observed peak memory fit, but a 25% headroom estimate does not"
                    .to_string(),
                required_memory_bytes: Some(1_125),
                memory_limit_bytes: Some(1_000),
            }
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_cgroup_limit_handles_max_and_huge_values() {
        assert_eq!(parse_cgroup_limit("max\n"), None);
        assert_eq!(parse_cgroup_limit("9223372036854775807"), None);
        assert_eq!(parse_cgroup_limit("1024"), Some(1024));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_proc_stat_reads_process_group_cpu_and_memory() {
        let stat = "123 (bash) S 1 456 456 0 -1 4194560 1 2 3 4 10 20 0 0 20 0 1 0 99 1000 25";

        let parsed = parse_proc_stat(stat).expect("proc stat");

        assert_eq!(parsed.process_group_id, 456);
        assert_eq!(parsed.cpu_jiffies, 30);
        assert_eq!(parsed.start_time_jiffies, 99);
        assert_eq!(parsed.rss_pages, 25);
    }
}
