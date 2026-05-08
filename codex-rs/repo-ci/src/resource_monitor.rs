use codex_cicd_artifacts::ContainerAttribution;
use codex_cicd_artifacts::ContainerResourceUsage;
use codex_cicd_artifacts::ContainerRuntime;
use codex_cicd_artifacts::HostResourceSnapshot;
use codex_cicd_artifacts::ResourceFeasibility;
use codex_cicd_artifacts::ResourceFeasibilityStatus;
use codex_cicd_artifacts::ResourceUsageTotals;
use codex_cicd_artifacts::RunResourceUsage;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::HashSet;
#[cfg(target_os = "linux")]
use std::fs;
use std::io::BufRead;
use std::io::BufReader;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const CONTAINER_POLL_INTERVAL: Duration = Duration::from_secs(1);
const DEFAULT_CLOCK_TICKS_PER_SECOND: u64 = 100;
const DEFAULT_PAGE_SIZE_BYTES: u64 = 4096;
const DOCKER_LABEL_RUN_ID: &str = "codex.repo_ci.run_id";
const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";
const COMPOSE_SERVICE_LABEL: &str = "com.docker.compose.service";
const COMPOSE_CONTAINER_NUMBER_LABEL: &str = "com.docker.compose.container-number";

pub(crate) struct ResourceMonitor {
    run_id: String,
    host: HostResourceSnapshot,
    process: ProcessGroupMonitor,
    containers: ContainerMonitor,
    aggregate_peak_memory_bytes: u64,
}

impl ResourceMonitor {
    pub(crate) fn start(
        run_id: String,
        compose_project_name: Option<String>,
        runner_pid: u32,
    ) -> Self {
        Self {
            host: host_snapshot(),
            process: ProcessGroupMonitor::new(runner_pid),
            containers: ContainerMonitor::new(&run_id, compose_project_name.as_deref()),
            run_id,
            aggregate_peak_memory_bytes: 0,
        }
    }

    pub(crate) fn poll(&mut self) {
        self.process.poll();
        self.containers.poll();
        let current_memory_bytes = self
            .process
            .current_memory_bytes()
            .saturating_add(self.containers.current_memory_bytes());
        self.aggregate_peak_memory_bytes =
            self.aggregate_peak_memory_bytes.max(current_memory_bytes);
    }

    pub(crate) fn finish(mut self) -> RunResourceUsage {
        self.poll();
        let process = self.process.finish();
        let containers = self.containers.finish();
        let totals = aggregate_totals(&process, &containers, self.aggregate_peak_memory_bytes);
        let feasibility = estimate_feasibility(&self.host, &totals);

        RunResourceUsage {
            run_id: self.run_id,
            host: self.host,
            process,
            containers,
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
    containers: &[ContainerResourceUsage],
    aggregate_peak_memory_bytes: u64,
) -> ResourceUsageTotals {
    let container_cpu_time_ms = containers.iter().fold(0_u64, |total, container| {
        total.saturating_add(container.resources.cpu_time_ms.unwrap_or(0))
    });
    let cpu_time_ms = process
        .cpu_time_ms
        .unwrap_or(0)
        .saturating_add(container_cpu_time_ms);
    ResourceUsageTotals {
        cpu_time_ms: (cpu_time_ms > 0).then_some(cpu_time_ms),
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

struct ContainerMonitor {
    runtimes: Vec<ContainerRuntimeMonitor>,
}

impl ContainerMonitor {
    fn new(run_id: &str, compose_project_name: Option<&str>) -> Self {
        Self::new_with_commands(
            run_id,
            compose_project_name,
            &[
                (ContainerRuntime::Docker, "docker"),
                (ContainerRuntime::Podman, "podman"),
            ],
        )
    }

    fn new_with_commands(
        run_id: &str,
        compose_project_name: Option<&str>,
        commands: &[(ContainerRuntime, &'static str)],
    ) -> Self {
        Self {
            runtimes: commands
                .iter()
                .filter_map(|(runtime, command)| {
                    ContainerRuntimeMonitor::new(*runtime, command, run_id, compose_project_name)
                })
                .collect(),
        }
    }

    fn poll(&mut self) {
        for runtime in &mut self.runtimes {
            runtime.poll();
        }
    }

    fn current_memory_bytes(&self) -> u64 {
        self.runtimes.iter().fold(0_u64, |total, runtime| {
            total.saturating_add(runtime.current_memory_bytes())
        })
    }

    fn finish(self) -> Vec<ContainerResourceUsage> {
        let mut containers = self
            .runtimes
            .into_iter()
            .flat_map(ContainerRuntimeMonitor::finish)
            .collect::<Vec<_>>();
        containers.sort_by(|left, right| {
            container_runtime_name(left.runtime)
                .cmp(container_runtime_name(right.runtime))
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.id.cmp(&right.id))
        });
        containers
    }
}

struct ContainerRuntimeMonitor {
    runtime: ContainerRuntime,
    command: &'static str,
    run_id: String,
    compose_project_name: Option<String>,
    existing_container_ids: HashSet<String>,
    tracked: BTreeMap<String, TrackedContainer>,
    events: Option<EventStream>,
    last_poll: Instant,
}

impl ContainerRuntimeMonitor {
    fn new(
        runtime: ContainerRuntime,
        command: &'static str,
        run_id: &str,
        compose_project_name: Option<&str>,
    ) -> Option<Self> {
        let existing_container_ids = list_container_ids(command).ok()?;
        Some(Self {
            runtime,
            command,
            run_id: run_id.to_string(),
            compose_project_name: compose_project_name.map(str::to_string),
            existing_container_ids,
            tracked: BTreeMap::new(),
            events: EventStream::start(command),
            last_poll: Instant::now()
                .checked_sub(CONTAINER_POLL_INTERVAL)
                .unwrap_or_else(Instant::now),
        })
    }

    fn poll(&mut self) {
        self.drain_events();
        if self.last_poll.elapsed() < CONTAINER_POLL_INTERVAL {
            return;
        }
        self.discover_current_containers();
        self.poll_stats();
        self.last_poll = Instant::now();
    }

    fn drain_events(&mut self) {
        let Some(events) = &mut self.events else {
            return;
        };
        let records = events.drain().collect::<Vec<_>>();
        for record in records {
            if record.container_id.is_empty() {
                continue;
            }
            let metadata = ContainerMetadata {
                id: record.container_id,
                name: record.name,
                image: record.image,
                labels: record.labels,
            };
            self.track_container(metadata);
        }
    }

    fn discover_current_containers(&mut self) {
        let Ok(container_ids) = list_container_ids(self.command) else {
            return;
        };
        for container_id in container_ids {
            if self.tracked.contains_key(&container_id) {
                continue;
            }
            let Some(metadata) = inspect_container(self.command, &container_id) else {
                continue;
            };
            self.track_container(metadata);
        }
    }

    fn track_container(&mut self, metadata: ContainerMetadata) {
        let attribution = container_attribution(
            &metadata.labels,
            &self.run_id,
            self.compose_project_name.as_deref(),
            !self.existing_container_ids.contains(&metadata.id),
        );
        let Some(attribution) = attribution else {
            return;
        };
        let labels = filtered_labels(&metadata.labels);
        let compose_project = metadata.labels.get(COMPOSE_PROJECT_LABEL).cloned();
        self.tracked
            .entry(metadata.id.clone())
            .and_modify(|tracked| {
                tracked.name = tracked.name.clone().or_else(|| metadata.name.clone());
                tracked.image = tracked.image.clone().or_else(|| metadata.image.clone());
                tracked.attribution = attribution;
                tracked.compose_project = tracked
                    .compose_project
                    .clone()
                    .or_else(|| compose_project.clone());
                tracked.labels.extend(labels.clone());
            })
            .or_insert_with(|| TrackedContainer {
                runtime: self.runtime,
                id: metadata.id,
                name: metadata.name,
                image: metadata.image,
                attribution,
                compose_project,
                labels,
                peak_memory_bytes: 0,
                current_memory_bytes: 0,
                estimated_cpu_time_ms: 0,
                last_stats_at: None,
                saw_cpu_sample: false,
            });
    }

    fn poll_stats(&mut self) {
        let container_ids = self.tracked.keys().cloned().collect::<Vec<_>>();
        for container_id in container_ids {
            let Some(stats) = container_stats(self.command, &container_id) else {
                continue;
            };
            let Some(container) = self.tracked.get_mut(&container_id) else {
                continue;
            };
            container.apply_stats(stats);
        }
    }

    fn current_memory_bytes(&self) -> u64 {
        self.tracked.values().fold(0_u64, |total, container| {
            total.saturating_add(container.current_memory_bytes)
        })
    }

    fn finish(self) -> Vec<ContainerResourceUsage> {
        self.tracked
            .into_values()
            .map(TrackedContainer::finish)
            .collect()
    }
}

struct TrackedContainer {
    runtime: ContainerRuntime,
    id: String,
    name: Option<String>,
    image: Option<String>,
    attribution: ContainerAttribution,
    compose_project: Option<String>,
    labels: BTreeMap<String, String>,
    peak_memory_bytes: u64,
    current_memory_bytes: u64,
    estimated_cpu_time_ms: u64,
    last_stats_at: Option<Instant>,
    saw_cpu_sample: bool,
}

impl TrackedContainer {
    fn apply_stats(&mut self, stats: ContainerStats) {
        let now = Instant::now();
        if let Some(memory_bytes) = stats.memory_bytes {
            self.current_memory_bytes = memory_bytes;
            self.peak_memory_bytes = self.peak_memory_bytes.max(memory_bytes);
        }
        if let Some(cpu_percent) = stats.cpu_percent {
            if let Some(last_stats_at) = self.last_stats_at {
                let elapsed_ms = u64::try_from(now.duration_since(last_stats_at).as_millis())
                    .unwrap_or(u64::MAX);
                let cpu_ms = ((elapsed_ms as f64) * cpu_percent / 100.0).max(0.0) as u64;
                self.estimated_cpu_time_ms = self.estimated_cpu_time_ms.saturating_add(cpu_ms);
                self.saw_cpu_sample = true;
            }
            self.last_stats_at = Some(now);
        }
    }

    fn finish(self) -> ContainerResourceUsage {
        ContainerResourceUsage {
            runtime: self.runtime,
            id: self.id,
            name: self.name,
            image: self.image,
            attribution: self.attribution,
            compose_project: self.compose_project,
            labels: self.labels,
            resources: ResourceUsageTotals {
                cpu_time_ms: self.saw_cpu_sample.then_some(self.estimated_cpu_time_ms),
                peak_memory_bytes: (self.peak_memory_bytes > 0).then_some(self.peak_memory_bytes),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContainerMetadata {
    id: String,
    name: Option<String>,
    image: Option<String>,
    labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
struct ContainerStats {
    memory_bytes: Option<u64>,
    cpu_percent: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeEventRecord {
    container_id: String,
    name: Option<String>,
    image: Option<String>,
    labels: BTreeMap<String, String>,
}

struct EventStream {
    child: Child,
    receiver: Receiver<RuntimeEventRecord>,
    reader: Option<JoinHandle<()>>,
}

impl EventStream {
    fn start(command: &str) -> Option<Self> {
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs()
            .to_string();
        let mut child = Command::new(command)
            .args([
                "events",
                "--format",
                "{{json .}}",
                "--filter",
                "type=container",
                "--since",
                &since,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(record) = parse_runtime_event(&line) {
                    let _ = sender.send(record);
                }
            }
        });
        Some(Self {
            child,
            receiver,
            reader: Some(reader),
        })
    }

    fn drain(&mut self) -> impl Iterator<Item = RuntimeEventRecord> + '_ {
        self.receiver.try_iter()
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn container_runtime_name(runtime: ContainerRuntime) -> &'static str {
    match runtime {
        ContainerRuntime::Docker => "docker",
        ContainerRuntime::Podman => "podman",
    }
}

fn container_attribution(
    labels: &BTreeMap<String, String>,
    run_id: &str,
    compose_project_name: Option<&str>,
    created_during_run: bool,
) -> Option<ContainerAttribution> {
    if labels
        .get(DOCKER_LABEL_RUN_ID)
        .is_some_and(|value| value == run_id)
    {
        Some(ContainerAttribution::Labeled)
    } else if labels
        .get(COMPOSE_PROJECT_LABEL)
        .is_some_and(|value| Some(value.as_str()) == compose_project_name)
    {
        Some(ContainerAttribution::ComposeProject)
    } else if created_during_run {
        Some(ContainerAttribution::CreatedDuringRun)
    } else {
        None
    }
}

fn filtered_labels(labels: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    labels
        .iter()
        .filter(|(key, _)| {
            matches!(
                key.as_str(),
                DOCKER_LABEL_RUN_ID
                    | COMPOSE_PROJECT_LABEL
                    | COMPOSE_SERVICE_LABEL
                    | COMPOSE_CONTAINER_NUMBER_LABEL
            )
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn list_container_ids(command: &str) -> std::io::Result<HashSet<String>> {
    let output = Command::new(command)
        .args(["ps", "-a", "--no-trunc", "--format", "{{.ID}}"])
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other("container list command failed"));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn inspect_container(command: &str, container_id: &str) -> Option<ContainerMetadata> {
    let output = Command::new(command)
        .args(["inspect", "--format", "{{json .}}", container_id])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_container_inspect(&String::from_utf8_lossy(&output.stdout))
}

fn parse_container_inspect(value: &str) -> Option<ContainerMetadata> {
    let value = serde_json::from_str::<Value>(value.trim()).ok()?;
    let id = value
        .get("Id")
        .or_else(|| value.get("ID"))
        .and_then(Value::as_str)?
        .to_string();
    let name = value
        .get("Name")
        .and_then(Value::as_str)
        .map(|name| name.trim_start_matches('/').to_string())
        .filter(|name| !name.is_empty());
    let config = value.get("Config");
    let image = config
        .and_then(|config| config.get("Image"))
        .or_else(|| value.get("ImageName"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let labels = config
        .and_then(|config| config.get("Labels"))
        .and_then(json_object_to_string_map)
        .unwrap_or_default();
    Some(ContainerMetadata {
        id,
        name,
        image,
        labels,
    })
}

fn container_stats(command: &str, container_id: &str) -> Option<ContainerStats> {
    let output = Command::new(command)
        .args([
            "stats",
            "--no-stream",
            "--format",
            "{{json .}}",
            container_id,
        ])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_container_stats(&String::from_utf8_lossy(&output.stdout))
}

fn parse_container_stats(value: &str) -> Option<ContainerStats> {
    let value = serde_json::from_str::<Value>(value.lines().next()?.trim()).ok()?;
    let memory_bytes = value
        .get("MemUsage")
        .or_else(|| value.get("MemUsageBytes"))
        .and_then(parse_memory_bytes_value);
    let cpu_percent = value
        .get("CPUPerc")
        .or_else(|| value.get("CPU"))
        .and_then(parse_percent_value);
    Some(ContainerStats {
        memory_bytes,
        cpu_percent,
    })
}

fn parse_runtime_event(line: &str) -> Option<RuntimeEventRecord> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    let actor = value.get("Actor");
    let container_id = actor
        .and_then(|actor| actor.get("ID"))
        .or_else(|| value.get("id"))
        .or_else(|| value.get("ID"))
        .and_then(Value::as_str)?
        .to_string();
    let attributes = actor
        .and_then(|actor| actor.get("Attributes"))
        .or_else(|| value.get("Attributes"))
        .and_then(json_object_to_string_map)
        .unwrap_or_default();
    let name = attributes
        .get("name")
        .or_else(|| attributes.get("container_name"))
        .cloned();
    let image = attributes.get("image").cloned();
    Some(RuntimeEventRecord {
        container_id,
        name,
        image,
        labels: filtered_labels(&attributes),
    })
}

fn json_object_to_string_map(value: &Value) -> Option<BTreeMap<String, String>> {
    Some(
        value
            .as_object()?
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
            .collect(),
    )
}

fn parse_memory_usage_bytes(value: &str) -> Option<u64> {
    let used = value.split('/').next()?.trim();
    parse_quantity_bytes(used)
}

fn parse_memory_bytes_value(value: &Value) -> Option<u64> {
    if let Some(bytes) = value.as_u64() {
        return Some(bytes);
    }
    value.as_str().and_then(parse_memory_usage_bytes)
}

fn parse_quantity_bytes(value: &str) -> Option<u64> {
    let value = value.trim();
    let number_end = value
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit() && *ch != '.')
        .map(|(index, _)| index)
        .unwrap_or(value.len());
    let number = value.get(..number_end)?.trim().parse::<f64>().ok()?;
    let unit = value.get(number_end..)?.trim().to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "" | "b" => 1.0,
        "kb" => 1000.0,
        "kib" => 1024.0,
        "mb" => 1000.0_f64.powi(2),
        "mib" => 1024.0_f64.powi(2),
        "gb" => 1000.0_f64.powi(3),
        "gib" => 1024.0_f64.powi(3),
        "tb" => 1000.0_f64.powi(4),
        "tib" => 1024.0_f64.powi(4),
        _ => return None,
    };
    Some((number * multiplier).max(0.0) as u64)
}

fn parse_percent(value: &str) -> Option<f64> {
    value.trim().trim_end_matches('%').trim().parse().ok()
}

fn parse_percent_value(value: &Value) -> Option<f64> {
    if let Some(percent) = value.as_f64() {
        return Some(percent);
    }
    value.as_str().and_then(parse_percent)
}

fn getconf_u64(name: &str, fallback: u64) -> u64 {
    let Ok(output) = Command::new("getconf")
        .arg(name)
        .stdin(Stdio::null())
        .output()
    else {
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
    (limit < (1_u64 << 60)).then_some(limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_container_memory_units() {
        assert_eq!(parse_memory_usage_bytes("1.5MiB / 8GiB"), Some(1_572_864));
        assert_eq!(parse_memory_usage_bytes("2MB / 8GB"), Some(2_000_000));
    }

    #[test]
    fn filters_container_labels_to_attribution_labels() {
        let labels = BTreeMap::from([
            (DOCKER_LABEL_RUN_ID.to_string(), "run".to_string()),
            (COMPOSE_PROJECT_LABEL.to_string(), "project".to_string()),
            ("secret.label".to_string(), "hidden".to_string()),
        ]);

        assert_eq!(
            filtered_labels(&labels),
            BTreeMap::from([
                (DOCKER_LABEL_RUN_ID.to_string(), "run".to_string()),
                (COMPOSE_PROJECT_LABEL.to_string(), "project".to_string()),
            ])
        );
    }

    #[test]
    fn missing_container_runtimes_leave_empty_container_usage() {
        let monitor = ContainerMonitor::new_with_commands(
            "run",
            /*compose_project_name*/ Some("run"),
            &[
                (ContainerRuntime::Docker, "__codex_missing_docker__"),
                (ContainerRuntime::Podman, "__codex_missing_podman__"),
            ],
        );

        assert_eq!(monitor.finish(), Vec::<ContainerResourceUsage>::new());
    }

    #[test]
    fn parses_docker_event_and_inspect_metadata() {
        let labels = BTreeMap::from([
            (DOCKER_LABEL_RUN_ID.to_string(), "run-1".to_string()),
            (COMPOSE_PROJECT_LABEL.to_string(), "project-1".to_string()),
            (COMPOSE_SERVICE_LABEL.to_string(), "db".to_string()),
            (COMPOSE_CONTAINER_NUMBER_LABEL.to_string(), "1".to_string()),
        ]);

        assert_eq!(
            parse_runtime_event(
                r#"{"status":"create","Actor":{"ID":"abc123","Attributes":{"name":"project-db-1","image":"postgres:16","codex.repo_ci.run_id":"run-1","com.docker.compose.project":"project-1","com.docker.compose.service":"db","com.docker.compose.container-number":"1"}}}"#,
            ),
            Some(RuntimeEventRecord {
                container_id: "abc123".to_string(),
                name: Some("project-db-1".to_string()),
                image: Some("postgres:16".to_string()),
                labels: labels.clone(),
            })
        );
        assert_eq!(
            parse_container_inspect(
                r#"{"Id":"abc123","Name":"/project-db-1","Config":{"Image":"postgres:16","Labels":{"codex.repo_ci.run_id":"run-1","com.docker.compose.project":"project-1","com.docker.compose.service":"db","com.docker.compose.container-number":"1"}}}"#,
            ),
            Some(ContainerMetadata {
                id: "abc123".to_string(),
                name: Some("project-db-1".to_string()),
                image: Some("postgres:16".to_string()),
                labels,
            })
        );
    }

    #[test]
    fn parses_podman_event_and_stats_metadata() {
        assert_eq!(
            parse_runtime_event(
                r#"{"id":"def456","Attributes":{"container_name":"svc-1","image":"redis:7","com.docker.compose.project":"project-2"}}"#,
            ),
            Some(RuntimeEventRecord {
                container_id: "def456".to_string(),
                name: Some("svc-1".to_string()),
                image: Some("redis:7".to_string()),
                labels: BTreeMap::from([(
                    COMPOSE_PROJECT_LABEL.to_string(),
                    "project-2".to_string()
                )]),
            })
        );
        assert_eq!(
            parse_container_stats(r#"{"CPUPerc":"12.5%","MemUsage":"1.5MiB / 8GiB"}"#,),
            Some(ContainerStats {
                memory_bytes: Some(1_572_864),
                cpu_percent: Some(12.5),
            })
        );
        assert_eq!(
            parse_container_stats(r#"{"CPU":7.25,"MemUsageBytes":4096}"#),
            Some(ContainerStats {
                memory_bytes: Some(4096),
                cpu_percent: Some(7.25),
            })
        );
    }

    #[test]
    fn attributes_labeled_compose_and_created_containers() {
        let run_labels = BTreeMap::from([(DOCKER_LABEL_RUN_ID.to_string(), "run".to_string())]);
        let compose_labels =
            BTreeMap::from([(COMPOSE_PROJECT_LABEL.to_string(), "project".to_string())]);

        assert_eq!(
            container_attribution(
                &run_labels,
                "run",
                /*compose_project_name*/ Some("project"),
                /*created_during_run*/ false,
            ),
            Some(ContainerAttribution::Labeled)
        );
        assert_eq!(
            container_attribution(
                &compose_labels,
                "run",
                /*compose_project_name*/ Some("project"),
                /*created_during_run*/ false,
            ),
            Some(ContainerAttribution::ComposeProject)
        );
        assert_eq!(
            container_attribution(
                &BTreeMap::new(),
                "run",
                /*compose_project_name*/ Some("project"),
                /*created_during_run*/ true,
            ),
            Some(ContainerAttribution::CreatedDuringRun)
        );
        assert_eq!(
            container_attribution(
                &compose_labels,
                "run",
                /*compose_project_name*/ Some("other"),
                /*created_during_run*/ false,
            ),
            None
        );
    }

    #[test]
    fn estimates_memory_feasibility_with_headroom() {
        let host = HostResourceSnapshot {
            cpu_count: Some(4),
            memory_total_bytes: Some(1_000),
            memory_available_bytes: Some(800),
            memory_limit_bytes: Some(1_000),
        };
        let totals = ResourceUsageTotals {
            cpu_time_ms: None,
            peak_memory_bytes: Some(900),
        };

        assert_eq!(
            estimate_feasibility(&host, &totals).status,
            ResourceFeasibilityStatus::Risky
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_proc_stat_with_spaces_in_command() {
        let stat = "123 (cmd with spaces) S 1 456 456 0 -1 0 0 0 0 0 12 34 0 0 20 0 1 0 789 0 42 0";

        assert_eq!(
            parse_proc_stat(stat).map(|stat| (
                stat.process_group_id,
                stat.cpu_jiffies,
                stat.start_time_jiffies,
                stat.rss_pages
            )),
            Some((456, 46, 789, 42))
        );
    }
}
