# Real-GPU Reranker Release Gate

CUDA artifacts must not be published from compilation evidence alone. The
release gate runs the packaged `hold` binary on a protected Linux NVIDIA runner
and records real inference, ranking parity, performance, and resource evidence.

## Runner isolation

The GPU workflow is intentionally unavailable to pull requests. Run it only
by manual dispatch from a protected `main` or tag ref, or as the CUDA job of
the release workflow for an annotated tag whose commit was validated as part
of `main`. The runner must carry all of these labels:

```text
self-hosted, linux, x64, localhold-gpu-release
```

Attach the job to a protected `cuda-release` environment. Restrict environment
approval and workflow dispatch to release maintainers, use an ephemeral runner
when possible, and never register a general-purpose repository runner with the
`localhold-gpu-release` label. The runner needs NVIDIA driver 570.26 or newer,
standard archive/inspection tools, GitHub artifact access, and a pre-populated
hash-verified model cache. It does not compile the binary or fetch upstream
runtime inputs, and it does not use a runner-installed ONNX Runtime, CUDA
toolkit, cuDNN, Python ML environment, or dynamic-loader path. It must not
expose database credentials, agent configuration, signing keys, or unrelated
services.

The workflow uploads only sanitized gate JSON. Reports contain provider, precision,
aggregate timing, throughput, RSS, VRAM, parity, thresholds, and failure text;
they omit hostnames, GPU UUIDs, usernames, credentials, configuration paths,
and model-cache paths. Negative-case evidence retains only the stable case,
status, exit code, failure category, and reranker check status; raw doctor
summaries and stderr remain ephemeral because they can contain configured paths.

## Preparing artifacts

Both managed artifacts must be fetched and verified before the offline gate:

```sh
LOCALHOLD_RERANKER_PRECISION=fp32 hold models fetch --yes --json
LOCALHOLD_RERANKER_PRECISION=fp16 hold models fetch --yes --json
```

Fetch is a separate, audited preparation step. The release workflow uses
`models verify` and fails when either artifact is absent or has the wrong
SHA-256; the benchmark never downloads or repairs artifacts.

The reusable workflow first builds the exact trusted release commit on an
Ubuntu 22.04 GitHub-hosted runner. That pins the published binary to the
documented glibc 2.35 floor; the workflow inspects its ELF version requirements
and fails if it references a newer glibc. It then materializes, validates, and
packages the pinned runtime:

```sh
python3 script/prepare-cuda-runtime.py \
  --cache-dir "$RUNNER_TEMP/localhold-cuda-source-cache" \
  --output-dir "$RUNNER_TEMP/localhold-cuda-runtime"
python3 script/validate-cuda-runtime.py "$RUNNER_TEMP/localhold-cuda-runtime"
cargo build --release --locked --features reranker-cuda \
  --target x86_64-unknown-linux-gnu
python3 script/package-release.py \
  --tag "$GITHUB_REF_NAME" \
  --target x86_64-unknown-linux-gnu-cuda12 \
  --binary target/x86_64-unknown-linux-gnu/release/hold \
  --cuda-runtime-dir "$RUNNER_TEMP/localhold-cuda-runtime" \
  --format tar.zst \
  --output-dir dist
```

Packaging streams the deterministic tar directly through single-threaded zstd
compression, avoiding a second 2.9 GiB uncompressed tar on the hosted runner.
The hosted job uploads the exact archive. The protected GPU job downloads that
same artifact, rechecks its manifest and glibc ceiling, and certifies the
extracted binary; it never rebuilds the release candidate.

The release specification pins ONNX Runtime 1.23.2, CUDA 12.8 components, cuDNN
9.8, every source URL, and every source SHA-256. Materialization extracts an
allowlisted library/notice inventory without installing the upstream Python
wheels. The package discovers its private sibling `lib/` directory and preloads
those exact libraries. An empty environment startup plus `/proc/PID/maps`
validation proves it did not inherit another CUDA or cuDNN installation.

## Methodology

`hold reranker gate` performs these checks in one machine-readable operation:

1. Loads the fused FP32 artifact with explicit required CPU, performs health
   inference, and scores a fixed six-query, sixteen-document corpus.
2. Measures CPU request p50, p95, and document-pair throughput with one, four,
   and eight concurrent clients after warmup.
3. Loads the configured CUDA precision with explicit required CUDA, proves both
   selected and active providers are CUDA, scores the same corpus, and measures
   the same concurrency matrix.
4. Compares top-k membership and per-pair scores against the FP32 CPU baseline.
5. Records process high-water RSS from `/proc/self/status` and process VRAM from
   `nvidia-smi` outside timed regions.
6. Creates an `auto` session and requires real health inference to keep CUDA
   selected and active.

The session mutex deliberately remains part of the measurement: the reported
numbers describe the packaged LocalHold behavior seen by concurrent callers,
not an isolated ONNX kernel microbenchmark. Tokenization, queueing, inference,
and result extraction are included. Model loading, warmup, resource sampling,
and `nvidia-smi` execution are excluded from request latency.

All elapsed-time measurement uses LocalHold's injectable monotonic clock. Unit
tests therefore control time without wall-clock sleeps; release runs use the
system implementation.

## Thresholds

The command has conservative portable defaults and exposes every release
threshold as a CLI option:

| Threshold | Default | Release use |
| --- | ---: | --- |
| Minimum top-10 overlap | 0.90 | FP32 should normally require 1.00; FP16 may use 0.90 while quality work continues. |
| Maximum absolute score delta | 0.03 | Use 0.001 for FP32 and 0.03 for FP16. |
| Maximum CUDA p95 | 1000 ms | Enforced independently at 1, 4, and 8 clients. Tighten per protected runner baseline. |
| Minimum CUDA throughput | 50 pairs/s | Enforced independently at 1, 4, and 8 clients. Tighten per protected runner baseline. |
| Maximum peak RSS | 3072 MiB | Covers the complete gate process, including model sessions. |
| Maximum process VRAM | 2048 MiB | Requires an actual `nvidia-smi` process measurement. |

Example FP32 release invocation:

```sh
LOCALHOLD_RERANKER_PRECISION=fp32 hold reranker gate \
  --iterations 10 \
  --warmup 3 \
  --min-overlap 1.0 \
  --max-score-delta 0.001 \
  --max-p95-ms 1000 \
  --min-throughput 50 \
  --max-rss-mib 3072 \
  --max-vram-mib 2048 \
  --json
```

Run FP16 separately with `LOCALHOLD_RERANKER_PRECISION=fp16`, a 0.90 minimum
overlap, and a 0.03 maximum score delta. FP16 evidence is a performance and
parity guard, not proof of ranking-quality equivalence. Preserve the FP32
report as its baseline and keep FP32 as the rollback artifact.

Any unavailable metric is a gate failure. A threshold should change only in a
reviewed commit with benchmark evidence and an explanation of the regression
or intentional workload change; do not raise thresholds inside a release run.

## Negative cases

The protected workflow also verifies that:

- hiding all CUDA devices makes the explicit required-CUDA gate fail;
- an incompatible or missing ONNX Runtime path produces structured doctor JSON
  without panic text on stderr;
- removing one manifest-owned CUDA dependency produces structured doctor JSON;
- model verification remains offline and rejects missing or modified files.

Preserve the successful workflow run and uploaded JSON artifacts with the
release record. The reusable gate uploads the exact CUDA archive, and the
release publish job depends on that job and downloads that artifact directly.
