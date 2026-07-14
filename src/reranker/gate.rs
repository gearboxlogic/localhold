//! Real-hardware reranker parity and performance release gate.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use serde::Serialize;

use super::{RerankerProvider, RerankerScore, runtime};
use crate::{
    clock::{Clock, SystemClock},
    config::{RerankerConfig, RerankerExecutionProvider, RerankerPrecision},
};

/// Stable schema version for machine-readable GPU gate reports.
pub const SCHEMA_VERSION: u32 = 1;

/// Default concurrency levels required by the release methodology.
pub const CONCURRENCY_LEVELS: [usize; 3] = [1, 4, 8];

/// Configurable pass/fail thresholds for the real-GPU release gate.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct GateOptions {
    /// Untimed inference iterations per client after warmup.
    pub iterations_per_client: usize,
    /// Warmup inference iterations for each concrete provider.
    pub warmup_iterations: usize,
    /// Number of highest-ranked documents compared for parity.
    pub parity_top_k: usize,
    /// Minimum allowed top-k membership overlap, in `[0, 1]`.
    pub minimum_top_k_overlap: f64,
    /// Maximum allowed absolute CPU/CUDA score delta.
    pub maximum_score_delta: f64,
    /// Maximum allowed CUDA p95 request latency at any concurrency.
    pub maximum_cuda_p95: Duration,
    /// Minimum allowed CUDA throughput at any concurrency, in document pairs/second.
    pub minimum_cuda_pairs_per_second: f64,
    /// Maximum observed process high-water RSS.
    pub maximum_rss_bytes: u64,
    /// Maximum observed VRAM attributed to the gate process.
    pub maximum_vram_bytes: u64,
}

impl Default for GateOptions {
    fn default() -> Self {
        Self {
            iterations_per_client: 10,
            warmup_iterations: 3,
            parity_top_k: 10,
            minimum_top_k_overlap: 0.9_f64,
            maximum_score_delta: 0.03_f64,
            maximum_cuda_p95: Duration::from_secs(1),
            minimum_cuda_pairs_per_second: 50.0_f64,
            maximum_rss_bytes: 3 * 1024 * 1024 * 1024,
            maximum_vram_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}

/// One process resource sample collected outside timed inference regions.
#[derive(Clone, Copy, Debug, Default, Serialize)]
#[non_exhaustive]
pub struct ResourceSnapshot {
    /// Current resident set size, when supported.
    pub rss_bytes: Option<u64>,
    /// Process high-water resident set size, when supported.
    pub peak_rss_bytes: Option<u64>,
    /// VRAM attributed to this process by `nvidia-smi`, when available.
    pub vram_bytes: Option<u64>,
}

/// Production resource sampler for Linux NVIDIA release runners.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct SystemResourceSampler;

trait ResourceSampler: Send + Sync {
    fn sample(&self) -> ResourceSnapshot;
}

impl ResourceSampler for SystemResourceSampler {
    fn sample(&self) -> ResourceSnapshot {
        let (rss_bytes, peak_rss_bytes) = linux_rss_bytes();
        ResourceSnapshot {
            rss_bytes,
            peak_rss_bytes,
            vram_bytes: nvidia_process_vram_bytes(),
        }
    }
}

/// Latency, throughput, and resource observations for one concurrency level.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct ConcurrencyMeasurement {
    /// Number of concurrent clients issuing rerank calls.
    pub clients: usize,
    /// Number of measured rerank requests.
    pub requests: usize,
    /// Number of document pairs scored.
    pub document_pairs: usize,
    /// Median end-to-end request latency in milliseconds.
    pub p50_ms: f64,
    /// 95th percentile end-to-end request latency in milliseconds.
    pub p95_ms: f64,
    /// Aggregate document pairs scored per second.
    pub pairs_per_second: f64,
    /// Resource sample collected immediately after this measurement.
    pub resources: ResourceSnapshot,
}

/// Concrete execution-provider evidence and performance results.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct ProviderEvidence {
    /// Provider explicitly requested for this session.
    pub requested: RerankerExecutionProvider,
    /// Provider selected while constructing the session.
    pub selected: Option<RerankerExecutionProvider>,
    /// Provider active after real health inference.
    pub active: Option<RerankerExecutionProvider>,
    /// Precision of the model artifact used by this provider.
    pub precision: RerankerPrecision,
    /// Resource sample after model load and health inference.
    pub loaded_resources: ResourceSnapshot,
    /// Measurements for one, four, and eight concurrent clients.
    pub concurrency: Vec<ConcurrencyMeasurement>,
}

/// CPU/CUDA ranking parity evidence over the fixed release corpus.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct ParityEvidence {
    /// Number of deterministic query cases compared.
    pub query_count: usize,
    /// Requested top-k membership boundary.
    pub top_k: usize,
    /// Lowest observed top-k membership overlap.
    pub minimum_observed_overlap: f64,
    /// Largest absolute score delta for a matching query/document pair.
    pub maximum_observed_score_delta: f64,
}

/// Policy-mode evidence required before CUDA artifact publication.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct PolicyEvidence {
    /// Explicit CPU selected and activated CPU.
    pub explicit_cpu: bool,
    /// Explicit required CUDA selected and activated CUDA.
    pub explicit_required_cuda: bool,
    /// Auto policy selected and activated CUDA on the real GPU runner.
    pub auto_selected_cuda: bool,
}

/// Machine-readable real-GPU release-gate report.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct GateReport {
    /// Report schema version.
    pub schema_version: u32,
    /// `passed` or `failed`.
    pub status: &'static str,
    /// Process exit code (`0` passed, `1` failed).
    pub exit_code: i32,
    /// CUDA artifact precision under evaluation.
    pub cuda_precision: RerankerPrecision,
    /// Thresholds applied to this run.
    pub thresholds: GateThresholdReport,
    /// Concrete provider evidence, when initialization succeeded.
    pub cpu: Option<ProviderEvidence>,
    /// Concrete CUDA evidence, when initialization succeeded.
    pub cuda: Option<ProviderEvidence>,
    /// CPU/CUDA ranking parity evidence, when both providers ran.
    pub parity: Option<ParityEvidence>,
    /// Provider policy evidence.
    pub policies: PolicyEvidence,
    /// Explicit reasons the gate failed.
    pub failures: Vec<String>,
}

/// Serialized threshold values included with every gate artifact.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct GateThresholdReport {
    /// Minimum allowed top-k overlap.
    pub minimum_top_k_overlap: f64,
    /// Maximum allowed absolute score delta.
    pub maximum_score_delta: f64,
    /// Maximum allowed CUDA p95 latency in milliseconds.
    pub maximum_cuda_p95_ms: f64,
    /// Minimum allowed CUDA document-pair throughput.
    pub minimum_cuda_pairs_per_second: f64,
    /// Maximum allowed process high-water RSS.
    pub maximum_rss_bytes: u64,
    /// Maximum allowed process VRAM.
    pub maximum_vram_bytes: u64,
}

impl GateReport {
    /// Serialize the report as pretty JSON.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the stable report cannot be encoded.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self).map(|mut json| {
            json.push('\n');
            json
        })
    }
}

#[derive(Clone, Copy)]
struct QueryCase {
    query: &'static str,
    documents: &'static [&'static str],
}

const DOCUMENTS: &[&str] = &[
    "PostgreSQL uses a server process and supports concurrent network clients.",
    "SQLite stores a database in a local file and needs no separate server.",
    "CUDA accelerates tensor inference on supported NVIDIA GPUs.",
    "A CPU executes general-purpose instructions and remains the portable fallback.",
    "Vector embeddings encode semantic similarity as numeric coordinates.",
    "Keyword search rewards exact token matches in indexed text.",
    "A reranker scores query and document pairs after initial retrieval.",
    "HTTP transport lets several agents share one long-running memory service.",
    "Standard input transport starts one server process for each client.",
    "Database backups should be tested by restoring them into an isolated environment.",
    "FP16 reduces numeric precision and can change the order of nearly tied results.",
    "FP32 is the portable default for CPU and CUDA reranking.",
    "A health probe should demonstrate real inference rather than configuration alone.",
    "Latency percentiles describe the distribution better than a single average.",
    "Throughput measures completed work per unit of elapsed time.",
    "Release evidence must avoid credentials, hostnames, and persistent runner paths.",
];

const CASES: &[QueryCase] = &[
    QueryCase {
        query: "How can multiple AI clients share memory without one process per client?",
        documents: DOCUMENTS,
    },
    QueryCase {
        query: "What is the portable reranker precision and what is the faster CUDA option?",
        documents: DOCUMENTS,
    },
    QueryCase {
        query: "How do we prove that GPU inference is actually active?",
        documents: DOCUMENTS,
    },
    QueryCase {
        query: "Which metrics capture request speed and capacity?",
        documents: DOCUMENTS,
    },
    QueryCase {
        query: "What storage backend works well for concurrent network users?",
        documents: DOCUMENTS,
    },
    QueryCase {
        query: "What information must be excluded from release evidence?",
        documents: DOCUMENTS,
    },
];

/// Run the real-GPU gate using production time and resource sampling.
pub async fn run(config: &RerankerConfig, options: &GateOptions) -> GateReport {
    run_with(config, options, Arc::new(SystemClock::new()), &SystemResourceSampler).await
}

#[expect(clippy::too_many_lines, reason = "linear gate orchestration keeps partial evidence and phase-specific failures explicit")]
async fn run_with(config: &RerankerConfig, options: &GateOptions, clock: Arc<dyn Clock>, sampler: &dyn ResourceSampler) -> GateReport {
    let thresholds = GateThresholdReport {
        minimum_top_k_overlap: options.minimum_top_k_overlap,
        maximum_score_delta: options.maximum_score_delta,
        maximum_cuda_p95_ms: duration_ms(options.maximum_cuda_p95),
        minimum_cuda_pairs_per_second: options.minimum_cuda_pairs_per_second,
        maximum_rss_bytes: options.maximum_rss_bytes,
        maximum_vram_bytes: options.maximum_vram_bytes,
    };
    let mut report = GateReport {
        schema_version: SCHEMA_VERSION,
        status: "failed",
        exit_code: 1,
        cuda_precision: config.precision,
        thresholds,
        cpu: None,
        cuda: None,
        parity: None,
        policies: PolicyEvidence {
            explicit_cpu: false,
            explicit_required_cuda: false,
            auto_selected_cuda: false,
        },
        failures: validate_options(options),
    };
    if !report.failures.is_empty() {
        return report;
    }

    let mut cpu_config = config.clone();
    cpu_config.enabled = true;
    cpu_config.required = true;
    cpu_config.execution_provider = RerankerExecutionProvider::Cpu;
    cpu_config.precision = RerankerPrecision::Fp32;
    if config.precision == RerankerPrecision::Fp16 && !config.model_path.is_empty() {
        report.failures.push("FP16 direct-file gates require a separately configured FP32 CPU baseline".into());
        return report;
    }

    let cpu_initialized = match runtime::initialize_for_diagnostics(&cpu_config, false).await {
        Ok(initialized) => initialized,
        Err(_error) => {
            report.failures.push("explicit CPU initialization failed".into());
            return report;
        }
    };
    let cpu_selected = cpu_initialized.selected_execution_provider();
    let cpu_active = cpu_initialized.active_execution_provider();
    report.policies.explicit_cpu = cpu_selected == Some(RerankerExecutionProvider::Cpu) && cpu_active == Some(RerankerExecutionProvider::Cpu);
    let cpu_provider = cpu_initialized.into_provider();
    let cpu_loaded_resources = sampler.sample();
    let cpu_scores = match score_corpus(&cpu_provider).await {
        Ok(scores) => scores,
        Err(_error) => {
            report.failures.push("CPU parity corpus inference failed".into());
            return report;
        }
    };
    let cpu_concurrency = match measure_provider(&cpu_provider, options, Arc::clone(&clock), sampler).await {
        Ok(measurements) => measurements,
        Err(_error) => {
            report.failures.push("CPU performance measurement failed".into());
            return report;
        }
    };
    report.cpu = Some(ProviderEvidence {
        requested: RerankerExecutionProvider::Cpu,
        selected: cpu_selected,
        active: cpu_active,
        precision: RerankerPrecision::Fp32,
        loaded_resources: cpu_loaded_resources,
        concurrency: cpu_concurrency,
    });
    drop(cpu_provider);

    let mut cuda_config = config.clone();
    cuda_config.enabled = true;
    cuda_config.required = true;
    cuda_config.execution_provider = RerankerExecutionProvider::Cuda;
    let cuda_initialized = match runtime::initialize_for_diagnostics(&cuda_config, false).await {
        Ok(initialized) => initialized,
        Err(_error) => {
            report.failures.push("explicit required CUDA initialization failed".into());
            return report;
        }
    };
    let cuda_selected = cuda_initialized.selected_execution_provider();
    let cuda_active = cuda_initialized.active_execution_provider();
    report.policies.explicit_required_cuda = cuda_selected == Some(RerankerExecutionProvider::Cuda) && cuda_active == Some(RerankerExecutionProvider::Cuda);
    let cuda_provider = cuda_initialized.into_provider();
    let cuda_loaded_resources = sampler.sample();
    let cuda_scores = match score_corpus(&cuda_provider).await {
        Ok(scores) => scores,
        Err(_error) => {
            report.failures.push("CUDA parity corpus inference failed".into());
            return report;
        }
    };
    let cuda_concurrency = match measure_provider(&cuda_provider, options, clock, sampler).await {
        Ok(measurements) => measurements,
        Err(_error) => {
            report.failures.push("CUDA performance measurement failed".into());
            return report;
        }
    };
    report.cuda = Some(ProviderEvidence {
        requested: RerankerExecutionProvider::Cuda,
        selected: cuda_selected,
        active: cuda_active,
        precision: cuda_config.precision,
        loaded_resources: cuda_loaded_resources,
        concurrency: cuda_concurrency,
    });
    report.parity = Some(compare_scores(&cpu_scores, &cuda_scores, options.parity_top_k));
    drop(cuda_provider);

    let mut auto_config = cuda_config;
    auto_config.execution_provider = RerankerExecutionProvider::Auto;
    // FP16 intentionally forbids auto fallback, so prove auto-provider CUDA
    // selection with the portable FP32 artifact instead.
    if auto_config.precision == RerankerPrecision::Fp16 {
        auto_config.precision = RerankerPrecision::Fp32;
    }
    match runtime::initialize_for_diagnostics(&auto_config, false).await {
        Ok(initialized) => {
            report.policies.auto_selected_cuda = initialized.selected_execution_provider() == Some(RerankerExecutionProvider::Cuda)
                && initialized.active_execution_provider() == Some(RerankerExecutionProvider::Cuda);
        }
        Err(_error) => report.failures.push("auto provider validation failed".into()),
    }

    evaluate_thresholds(&mut report, options);
    if report.failures.is_empty() {
        report.status = "passed";
        report.exit_code = 0_i32;
    }
    report
}

fn validate_options(options: &GateOptions) -> Vec<String> {
    let mut failures = Vec::new();
    if options.iterations_per_client == 0 {
        failures.push("iterations per client must be greater than zero".into());
    } else if options.iterations_per_client > 10_000 {
        failures.push("iterations per client must not exceed 10000".into());
    }
    if options.warmup_iterations > 10_000 {
        failures.push("warmup iterations must not exceed 10000".into());
    }
    if options.parity_top_k == 0 || options.parity_top_k > DOCUMENTS.len() {
        failures.push(format!("parity top-k must be between 1 and {}", DOCUMENTS.len()));
    }
    if !(0.0_f64..=1.0_f64).contains(&options.minimum_top_k_overlap) || !options.minimum_top_k_overlap.is_finite() {
        failures.push("minimum top-k overlap must be finite and between zero and one".into());
    }
    if options.maximum_score_delta < 0.0_f64 || !options.maximum_score_delta.is_finite() {
        failures.push("maximum score delta must be finite and non-negative".into());
    }
    if options.minimum_cuda_pairs_per_second <= 0.0_f64 || !options.minimum_cuda_pairs_per_second.is_finite() {
        failures.push("minimum CUDA throughput must be finite and positive".into());
    }
    failures
}

async fn score_corpus(provider: &Arc<dyn RerankerProvider>) -> Result<Vec<Vec<RerankerScore>>, String> {
    let mut corpus = Vec::with_capacity(CASES.len());
    for case in CASES {
        corpus.push(provider.rerank(case.query, case.documents).await.map_err(|error| error.to_string())?);
    }
    Ok(corpus)
}

#[expect(clippy::arithmetic_side_effects, reason = "bounded benchmark corpus and request counts")]
#[expect(clippy::float_arithmetic, reason = "throughput is measured work divided by elapsed seconds")]
#[expect(clippy::integer_division_remainder_used, reason = "fixed corpus cases intentionally rotate with modulo")]
async fn measure_provider(
    provider: &Arc<dyn RerankerProvider>,
    options: &GateOptions,
    clock: Arc<dyn Clock>,
    sampler: &dyn ResourceSampler,
) -> Result<Vec<ConcurrencyMeasurement>, String> {
    for index in 0..options.warmup_iterations {
        let case = CASES[index % CASES.len()];
        let _scores = provider.rerank(case.query, case.documents).await.map_err(|error| error.to_string())?;
    }

    let mut measurements = Vec::with_capacity(CONCURRENCY_LEVELS.len());
    for clients in CONCURRENCY_LEVELS {
        let started = clock.monotonic();
        let mut tasks = Vec::with_capacity(clients);
        for client in 0..clients {
            let provider = Arc::clone(provider);
            let clock = Arc::clone(&clock);
            let iterations = options.iterations_per_client;
            tasks.push(tokio::spawn(measure_client(provider, clock, client, iterations)));
        }
        let mut latencies = Vec::with_capacity(clients.saturating_mul(options.iterations_per_client));
        for task in tasks {
            let mut client_latencies = task.await.map_err(|error| format!("measurement task failed: {error}"))??;
            latencies.append(&mut client_latencies);
        }
        let elapsed = clock.monotonic().saturating_sub(started);
        latencies.sort_unstable();
        let requests = latencies.len();
        let document_pairs = requests.saturating_mul(DOCUMENTS.len());
        let pairs_per_second = if elapsed.is_zero() {
            0.0_f64
        } else {
            usize_to_f64(document_pairs) / elapsed.as_secs_f64()
        };
        measurements.push(ConcurrencyMeasurement {
            clients,
            requests,
            document_pairs,
            p50_ms: percentile_ms(&latencies, 50),
            p95_ms: percentile_ms(&latencies, 95),
            pairs_per_second,
            resources: sampler.sample(),
        });
    }
    Ok(measurements)
}

#[expect(clippy::arithmetic_side_effects, reason = "bounded benchmark corpus indices")]
#[expect(clippy::integer_division_remainder_used, reason = "fixed corpus cases intentionally rotate with modulo")]
async fn measure_client(provider: Arc<dyn RerankerProvider>, clock: Arc<dyn Clock>, client: usize, iterations: usize) -> Result<Vec<Duration>, String> {
    let mut latencies = Vec::with_capacity(iterations);
    for iteration in 0..iterations {
        let case = CASES[(client + iteration) % CASES.len()];
        let request_started = clock.monotonic();
        let _scores = provider.rerank(case.query, case.documents).await.map_err(|error| error.to_string())?;
        latencies.push(clock.monotonic().saturating_sub(request_started));
    }
    Ok(latencies)
}

#[expect(clippy::float_arithmetic, reason = "ranking parity compares overlap ratios and score deltas")]
fn compare_scores(cpu: &[Vec<RerankerScore>], cuda: &[Vec<RerankerScore>], top_k: usize) -> ParityEvidence {
    let mut minimum_overlap = 1.0_f64;
    let mut maximum_delta = 0.0_f64;
    for (cpu_query, cuda_query) in cpu.iter().zip(cuda) {
        let cpu_top = top_indices(cpu_query, top_k);
        let cuda_top = top_indices(cuda_query, top_k);
        let overlap = usize_to_f64(cpu_top.intersection(&cuda_top).count()) / usize_to_f64(top_k);
        minimum_overlap = minimum_overlap.min(overlap);
        for cpu_score in cpu_query {
            if let Some(cuda_score) = cuda_query.iter().find(|score| score.index == cpu_score.index) {
                maximum_delta = maximum_delta.max((cpu_score.score - cuda_score.score).abs());
            }
        }
    }
    ParityEvidence {
        query_count: cpu.len().min(cuda.len()),
        top_k,
        minimum_observed_overlap: minimum_overlap,
        maximum_observed_score_delta: maximum_delta,
    }
}

fn top_indices(scores: &[RerankerScore], top_k: usize) -> BTreeSet<usize> {
    let mut ordered = scores.to_vec();
    ordered.sort_by(|left, right| right.score.total_cmp(&left.score).then_with(|| left.index.cmp(&right.index)));
    ordered.into_iter().take(top_k).map(|score| score.index).collect()
}

fn evaluate_thresholds(report: &mut GateReport, options: &GateOptions) {
    if !report.policies.explicit_cpu {
        report.failures.push("explicit CPU did not select and activate CPU".into());
    }
    if !report.policies.explicit_required_cuda {
        report.failures.push("explicit required CUDA did not select and activate CUDA".into());
    }
    if !report.policies.auto_selected_cuda {
        report.failures.push("auto policy did not select and activate CUDA on the GPU runner".into());
    }
    if let Some(parity) = &report.parity {
        if parity.minimum_observed_overlap < options.minimum_top_k_overlap {
            report.failures.push(format!(
                "minimum top-k overlap {:.6} is below threshold {:.6}",
                parity.minimum_observed_overlap, options.minimum_top_k_overlap
            ));
        }
        if parity.maximum_observed_score_delta > options.maximum_score_delta {
            report.failures.push(format!(
                "maximum score delta {:.6} exceeds threshold {:.6}",
                parity.maximum_observed_score_delta, options.maximum_score_delta
            ));
        }
    }
    let Some(cuda) = &report.cuda else {
        report.failures.push("CUDA evidence is missing".into());
        return;
    };
    for measurement in &cuda.concurrency {
        if measurement.p95_ms > duration_ms(options.maximum_cuda_p95) {
            report.failures.push(format!(
                "CUDA p95 {:.3}ms at {} clients exceeds {:.3}ms",
                measurement.p95_ms,
                measurement.clients,
                duration_ms(options.maximum_cuda_p95)
            ));
        }
        if measurement.pairs_per_second < options.minimum_cuda_pairs_per_second {
            report.failures.push(format!(
                "CUDA throughput {:.3} pairs/s at {} clients is below {:.3}",
                measurement.pairs_per_second, measurement.clients, options.minimum_cuda_pairs_per_second
            ));
        }
    }
    let snapshots = cuda
        .concurrency
        .iter()
        .map(|measurement| measurement.resources)
        .chain(std::iter::once(cuda.loaded_resources));
    let mut saw_rss = false;
    let mut saw_vram = false;
    for snapshot in snapshots {
        if let Some(rss) = snapshot.peak_rss_bytes {
            saw_rss = true;
            if rss > options.maximum_rss_bytes {
                report.failures.push(format!("peak RSS {rss} exceeds {} bytes", options.maximum_rss_bytes));
            }
        }
        if let Some(vram) = snapshot.vram_bytes {
            saw_vram = true;
            if vram > options.maximum_vram_bytes {
                report.failures.push(format!("process VRAM {vram} exceeds {} bytes", options.maximum_vram_bytes));
            }
        }
    }
    if !saw_rss {
        report.failures.push("peak RSS measurement is unavailable".into());
    }
    if !saw_vram {
        report.failures.push("process VRAM measurement is unavailable".into());
    }
}

fn percentile_ms(sorted: &[Duration], percentile: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0_f64;
    }
    let rank = sorted.len().saturating_mul(percentile).saturating_add(99).saturating_div(100).max(1);
    duration_ms(sorted[rank.saturating_sub(1).min(sorted.len().saturating_sub(1))])
}

#[expect(clippy::cast_precision_loss, reason = "benchmark counts fit exactly in practical f64 ranges")]
#[expect(clippy::as_conversions, reason = "benchmark counts fit exactly in practical f64 ranges")]
const fn usize_to_f64(value: usize) -> f64 {
    value as f64
}

#[expect(clippy::float_arithmetic, reason = "benchmark reporting converts seconds to milliseconds")]
fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0_f64
}

fn linux_rss_bytes() -> (Option<u64>, Option<u64>) {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return (None, None);
    };
    (proc_status_kib(&status, "VmRSS:"), proc_status_kib(&status, "VmHWM:"))
}

fn proc_status_kib(status: &str, key: &str) -> Option<u64> {
    let kib = status.lines().find(|line| line.starts_with(key))?.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    kib.checked_mul(1024)
}

fn nvidia_process_vram_bytes() -> Option<u64> {
    let output = std::process::Command::new("nvidia-smi")
        .args(["--query-compute-apps=pid,used_memory", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let pid = std::process::id();
    let stdout = String::from_utf8(output.stdout).ok()?;
    parse_nvidia_process_vram_bytes(&stdout, pid)
}

fn parse_nvidia_process_vram_bytes(stdout: &str, pid: u32) -> Option<u64> {
    let mut found = false;
    let mut mib = 0_u64;
    for line in stdout.lines() {
        let Some((raw_pid, raw_mib)) = line.split_once(',') else {
            continue;
        };
        if raw_pid.trim().parse::<u32>().ok() != Some(pid) {
            continue;
        }
        found = true;
        // A partial multi-GPU total could under-report VRAM and incorrectly
        // pass the release threshold, so any malformed matching row fails the
        // measurement closed.
        let parsed_mib = raw_mib.trim().parse::<u64>().ok()?;
        mib = mib.checked_add(parsed_mib)?;
    }
    if !found {
        return None;
    }
    mib.checked_mul(1024)?.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_with_cuda_resources(resources: ResourceSnapshot) -> GateReport {
        GateReport {
            schema_version: SCHEMA_VERSION,
            status: "failed",
            exit_code: 1,
            cuda_precision: RerankerPrecision::Fp32,
            thresholds: GateThresholdReport {
                minimum_top_k_overlap: 0.9_f64,
                maximum_score_delta: 0.03_f64,
                maximum_cuda_p95_ms: 1_000.0_f64,
                minimum_cuda_pairs_per_second: 50.0_f64,
                maximum_rss_bytes: 1,
                maximum_vram_bytes: 1,
            },
            cpu: None,
            cuda: Some(ProviderEvidence {
                requested: RerankerExecutionProvider::Cuda,
                selected: Some(RerankerExecutionProvider::Cuda),
                active: Some(RerankerExecutionProvider::Cuda),
                precision: RerankerPrecision::Fp32,
                loaded_resources: resources,
                concurrency: vec![ConcurrencyMeasurement {
                    clients: 1,
                    requests: 1,
                    document_pairs: DOCUMENTS.len(),
                    p50_ms: 1.0_f64,
                    p95_ms: 1.0_f64,
                    pairs_per_second: 100.0_f64,
                    resources,
                }],
            }),
            parity: Some(ParityEvidence {
                query_count: CASES.len(),
                top_k: 10,
                minimum_observed_overlap: 1.0_f64,
                maximum_observed_score_delta: 0.0_f64,
            }),
            policies: PolicyEvidence {
                explicit_cpu: true,
                explicit_required_cuda: true,
                auto_selected_cuda: true,
            },
            failures: Vec::new(),
        }
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        let samples = [1_u64, 2, 3, 4, 5, 6, 7, 8, 9, 10].map(Duration::from_millis);
        assert!((percentile_ms(&samples, 50) - 5.0_f64).abs() < f64::EPSILON);
        assert!((percentile_ms(&samples, 95) - 10.0_f64).abs() < f64::EPSILON);
    }

    #[test]
    fn parity_detects_membership_and_score_drift() {
        let cpu = vec![vec![RerankerScore::new(0, 0.9_f64), RerankerScore::new(1, 0.8_f64), RerankerScore::new(2, 0.1_f64)]];
        let cuda = vec![vec![RerankerScore::new(0, 0.89_f64), RerankerScore::new(1, 0.2_f64), RerankerScore::new(2, 0.81_f64)]];
        let parity = compare_scores(&cpu, &cuda, 2);
        assert!((parity.minimum_observed_overlap - 0.5_f64).abs() < f64::EPSILON);
        assert!((parity.maximum_observed_score_delta - 0.71_f64).abs() < f64::EPSILON);
    }

    #[test]
    fn proc_status_parser_converts_kibibytes() {
        let status = "Name:\thold\nVmHWM:\t2048 kB\nVmRSS:\t1024 kB\n";
        assert_eq!(proc_status_kib(status, "VmRSS:"), Some(1_048_576_u64));
        assert_eq!(proc_status_kib(status, "VmHWM:"), Some(2_097_152_u64));
    }

    #[test]
    fn nvidia_vram_parser_sums_matching_gpus_and_fails_closed() {
        let complete = "42, 100\n7, 999\n42, 200\n";
        assert_eq!(parse_nvidia_process_vram_bytes(complete, 42), Some(300_u64 * 1024 * 1024));

        let partial = "42, 100\n42, N/A\n";
        assert_eq!(parse_nvidia_process_vram_bytes(partial, 42), None);
    }

    #[test]
    fn resource_thresholds_fail_for_unavailable_and_excessive_samples() {
        let options = GateOptions {
            maximum_rss_bytes: 10,
            maximum_vram_bytes: 20,
            ..GateOptions::default()
        };
        let mut excessive = report_with_cuda_resources(ResourceSnapshot {
            rss_bytes: Some(1),
            peak_rss_bytes: Some(11),
            vram_bytes: Some(21),
        });
        evaluate_thresholds(&mut excessive, &options);
        assert!(excessive.failures.iter().any(|failure| failure.starts_with("peak RSS")));
        assert!(excessive.failures.iter().any(|failure| failure.starts_with("process VRAM")));

        let mut unavailable = report_with_cuda_resources(ResourceSnapshot::default());
        evaluate_thresholds(&mut unavailable, &options);
        assert!(unavailable.failures.iter().any(|failure| failure == "peak RSS measurement is unavailable"));
        assert!(unavailable.failures.iter().any(|failure| failure == "process VRAM measurement is unavailable"));
    }

    #[test]
    fn invalid_options_fail_before_hardware_access() {
        let options = GateOptions {
            iterations_per_client: 0,
            parity_top_k: 0,
            minimum_top_k_overlap: f64::NAN,
            maximum_score_delta: -1.0_f64,
            minimum_cuda_pairs_per_second: 0.0_f64,
            ..GateOptions::default()
        };
        assert_eq!(validate_options(&options).len(), 5);
    }
}
