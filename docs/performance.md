# qpdf/flpdf memory and time benchmarks

`flpdf-ymuj.1` provides a diagnostic harness; it does not change production PDF
processing. The immediate goal of `flpdf-ymuj` is memory convergence through
qpdf-faithful data structures, ownership, algorithms, and lifetimes. Time is
recorded to detect regressions and remains a later convergence goal.

## Run from a clean checkout

Prerequisites: Linux, Python 3.11+, GNU `/usr/bin/time`, Rust/Cargo, the normal
flpdf native build dependencies, and `/usr/bin/qpdf` **11.9.0**. The harness checks
the qpdf version. Fetch the pinned source once with
`scripts/fetch-qpdf-source.sh`; subsequent runs resolve it with `--print-path`.
The source commit is checked against
`3b97c9bd266b7c32ea36d3536e22dab77412886d`.

```sh
python3 scripts/perf-matrix.py
```

This single command builds the release CLI with `--locked`, generates and checks
the inputs, runs the complete matrix, validates results, and writes `results.json`
and `summary.md` to a fresh `/tmp/flpdf-perf-*` directory printed at startup.
It uses a separate `target/perf-default` build directory to avoid feature mixing.
It does not run benchmarks concurrently. Avoid other builds, profiling sessions,
or benchmarks on the same machine during collection. Artifacts are retained for
inspection; the harness does not delete previous runs.

For a controlled zlib comparison, run separately:

```sh
python3 scripts/perf-matrix.py --flavor qpdf-zlib-compat
```

Record both flavors. The default build reflects shipped miniz_oxide performance;
`qpdf-zlib-compat` controls the DEFLATE implementation for byte comparisons.
Do not combine their samples. The JSON records source commit, dirty worktree
state, harness/binary/input SHA-256 hashes, tool versions, exact build and run
commands, platform, CPU count, starting load, and selected build environment
overrides. Run with a clean checkout and no build overrides for comparable
baselines. `--flpdf /absolute/path` skips building but explicitly records
`external-unverified` provenance and an unknown source commit/flavor; it is for
diagnostics, not evidence that a particular source revision was measured.

## Fixed primary matrix

All combinations below are primary cases, including each size separately:

| Input family | Sizes | Purpose |
|---|---|---|
| `pages` | 100, 1,000, 5,000 pages | No streams; direct scalar/array/dictionary state per page |
| `content` | 100, 1,000, 5,000 pages | Individual small content streams plus page state |
| `objects` | 100, 1,000, 5,000 extra indirect dictionaries | Reachable from Catalog; input contains generated object streams |
| `stream` | 1, 8, 32 MiB decoded payload | Zero-filled embedded stream; compressed by qpdf during preparation |
| `qtest-inline-images` | Pinned `qpdf/qtest/qpdf/inline-images.pdf` | Real upstream regression fixture, read from the external source tree |

The 12 operations are `check`, `npages`, `json`, `rewrite`, `qdf`, `linearize`,
`objects-preserve`, `objects-generate`, `objects-disable`, `streams-preserve`,
`streams-compress`, and `streams-uncompress`: **13 inputs × 12 = 156 cases**.
`check` exercises parsing/checking; `npages` exposes a relatively shallow read.
JSON uses the complete qpdf document representation, `--json-output=2`.
Writer cases use `--static-id` identically for both programs.

The generation recipe is original to flpdf, with explicit offsets and a valid
xref/trailer. All extra objects and embedded streams are reachable from the
Catalog. Preparation uses qpdf outside the measured region. Upstream fixtures
are never copied into flpdf. The artifact directory must be outside this
checkout because it also contains outputs derived from upstream fixtures.

The stream family intentionally separates input file size from decoded size.
It exercises retained decoded/output buffers; it is not a representative
compression-throughput corpus. Preserve/disable object-stream modes operate on
compressed input in the `objects` family, not merely on PDFs without ObjStms.

## Measurement and interpretation

Each case performs one separately recorded **first invocation**, one warmup,
then five measured steady-state invocations per program. Each invocation is a
fresh process. Pair order alternates qpdf/flpdf and flpdf/qpdf. The first run is
not a cold-cache claim: fixture generation/validation and prior cases may have
populated OS caches. The harness does not drop system caches. Only steady-state
samples contribute to the summary.

GNU time records each child process's maximum RSS (KiB), user time, and system
time. This avoids attributing the Python fixture generator's peak RSS to the
PDF process. High-resolution wall time includes launch/wait overhead of GNU
time, equally for both tools. GNU time's CPU times have 0.01-second resolution;
zero denominators produce JSON `null`, never fabricated ratios. Very small
workloads are useful for RSS baselines but weak evidence for CPU-time ratios.

Every metric includes median, min, max, and `(max-min)/median`. Ratios are
flpdf/qpdf medians. The immediate target is **RSS ≤ 1.10× qpdf for every primary
case**. The eventual time target is also ≤ 1.10×. At least five samples per tool
and relative range ≤ 10% for both tools are required for a per-case target
classification. Otherwise the result is `insufficient-samples` or `variable`;
repeat in a quieter environment with more samples. No case can be hidden by a
matrix average. Compare successive sizes within a family/operation to identify
increasing ratios and growth slopes. The summary's growth table and JSON
`scaling` rows report adjacent-size RSS growth in KiB/unit and the change in RSS
ratio. These are descriptive slopes, not significance tests: consult sample
spread at both endpoints. `inputs[].scale` and `unit` preserve the dimensions
for downstream analysis.

`complete` means the requested diagnostic run finished and its outputs passed
validation. It does **not** mean memory parity or Epic completion. Check
`primary_matrix_complete`, all per-case memory verdicts, source/build provenance,
and correctness evidence. A partial matrix cannot establish the Epic target.
Performance verdicts do not change the harness exit code and are not ordinary
CI timing gates.

For every invocation the harness records exit status, stdout/stderr, output size
and hash, and validation results. Nonzero exits (including warnings) or invalid
outputs mark the case invalid and make the overall harness exit nonzero while
preserving the report. Written PDFs must pass qpdf `--check` and preserve page
count. Page-count output is checked against the input; JSON must parse and
contain the qpdf payload. The last steady outputs' byte equality is reported
separately for all operations except `check`, whose diagnostics include program
and path names. Readability and byte equality are distinct: a readable but
different output remains visible as `byte_identical: false`. These diagnostics
do not replace the project's source-backed differential correctness tests.

## Focused runs and allocation attribution

```sh
# Small full-operation smoke; explicitly a partial matrix.
python3 scripts/perf-matrix.py --sizes 3 --stream-mib 1 --runs 1 --warmups 0

# A full-size memory case with separate allocation profiling.
python3 scripts/perf-matrix.py --operations rewrite --heaptrack-case pages-1000/rewrite
```

`--operations` accepts a comma-separated subset. `--sizes`, `--stream-mib`,
`--runs`, `--warmups`, and `--timeout` control diagnostic runs. `--skip-qtest`
allows local smoke tests without the external source, marking the matrix partial.
`--output` selects a new artifact directory and refuses existing directories.

`--heaptrack-case INPUT/OPERATION` is repeatable and requires `heaptrack` and
`heaptrack_print`. It profiles both programs in **additional** runs, keeping
instrumentation time and RSS out of the baseline. The report retains compressed
traces, the allocation-size histogram, allocation count/total allocated bytes,
the text report with peak allocation backtraces, and exact peak stack costs for
inspection. `peak_live_bytes` is the sum of those stack costs at the global heap
peak. `heaptrack_print --merge-backtraces 0` avoids its documented merged
peak inaccuracy. Total allocated bytes measure allocation traffic, not live heap
size or RSS; use the peak report/stacks for live retention. Optimized release
stacks may have limited symbols; any rebuild with debug symbols must be recorded
as a separate profiling configuration, not silently substituted in a baseline.

Use those stacks to connect excess allocations and live bytes to qpdf's
corresponding structures and lifetimes. Explain allocator/container differences
separately. An RSS reduction alone is not evidence of a faithful port.

Contract tests (no performance assertions):

```sh
python3 -m unittest scripts/tests/test_perf_matrix.py
```
