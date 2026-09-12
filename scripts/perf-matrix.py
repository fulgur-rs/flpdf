#!/usr/bin/env python3
"""Reproducible, memory-first qpdf/flpdf diagnostics. See docs/performance.md."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from datetime import datetime, timezone


ROOT = Path(__file__).resolve().parents[1]
QPDF_COMMIT = "3b97c9bd266b7c32ea36d3536e22dab77412886d"
GENERATED_FAMILIES = ("pages", "content", "objects", "stream")
SIZES = [100, 1000, 5000]
STREAM_MIB = [1, 8, 32]
OPERATIONS = {
    "check": ["--check"],
    "npages": ["--show-npages"],
    "json": ["--json-output=2"],
    "rewrite": [],
    "qdf": ["--qdf"],
    "linearize": ["--linearize"],
    "objects-preserve": ["--object-streams=preserve"],
    "objects-generate": ["--object-streams=generate"],
    "objects-disable": ["--object-streams=disable"],
    "streams-preserve": ["--stream-data=preserve"],
    "streams-compress": ["--stream-data=compress"],
    "streams-uncompress": ["--stream-data=uncompress"],
}
ENV = {**os.environ, "LC_ALL": "C", "TZ": "UTC"}


def run(command, **kwargs):
    return subprocess.run([str(x) for x in command], env=ENV, check=True, **kwargs)


def capture(command):
    return run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True).stdout.strip()


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def statistics_for(values):
    median = statistics.median(values)
    return {"median": median, "min": min(values), "max": max(values),
            "relative_range": (max(values) - min(values)) / median if median else None}


def ratio(numerator, denominator):
    return numerator / denominator if denominator else None


def verdict(qpdf, flpdf):
    if min(len(qpdf), len(flpdf)) < 5:
        return "insufficient-samples"
    stats = [statistics_for(v) for v in (qpdf, flpdf)]
    if any(s["relative_range"] is None or s["relative_range"] > 0.10 for s in stats):
        return "variable"
    return "within-target" if stats[1]["median"] <= 1.10 * stats[0]["median"] else "above-target"


def generate_pdf(path, family, size):
    """Original fixtures: all measured objects reachable from Catalog; valid xref."""
    count = size if family in ("pages", "content") else 1
    page_ids = [3 + i * (2 if family == "content" else 1) for i in range(count)]
    catalog = b"<< /Type /Catalog /Pages 2 0 R"
    if family == "objects":
        catalog += b" /Bench [" + " ".join(f"{4+i} 0 R" for i in range(size)).encode() + b"]"
    if family == "stream":
        catalog += b" /Names << /EmbeddedFiles << /Names [(payload) 4 0 R] >> >>"
    bodies = [catalog + b" >>",
              f"<< /Type /Pages /Count {count} /Kids [".encode() +
              " ".join(f"{n} 0 R" for n in page_ids).encode() + b"] >>"]
    for page_id in page_ids:
        page = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >>"
        page += b" /Bench << /Flag true /Name /Sample /Values [1 2 3 4.5 (scalar) null] >>"
        if family == "content":
            page += f" /Contents {page_id + 1} 0 R".encode()
        bodies.append(page + b" >>")
        if family == "content":
            data = b"q 0 0 100 100 re S Q\n" * 16
            bodies.append(f"<< /Length {len(data)} >>\nstream\n".encode() + data + b"endstream")
    if family == "objects":
        bodies.extend(f"<< /Index {i} /Values [1 2 3 4 5 6 7 8] /Label (object-{i}) /Flag true >>".encode()
                      for i in range(size))
    if family == "stream":
        bodies.append(b"<< /Type /Filespec /F (payload) /EF << /F 5 0 R >> >>")
        bodies.append(f"<< /Type /EmbeddedFile /Length {size} >>\nstream\n".encode()
                      + b"\0" * size + b"\nendstream")
    with Path(path).open("wb") as out:
        out.write(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n")
        offsets = [0]
        for index, body in enumerate(bodies, 1):
            offsets.append(out.tell())
            out.write(f"{index} 0 obj\n".encode() + body + b"\nendobj\n")
        xref = out.tell()
        out.write(f"xref\n0 {len(offsets)}\n0000000000 65535 f \n".encode())
        for offset in offsets[1:]:
            out.write(f"{offset:010d} 00000 n \n".encode())
        out.write(f"trailer\n<< /Root 1 0 R /Size {len(offsets)} >>\nstartxref\n{xref}\n%%EOF\n".encode())


def measure(command, directory, name, timeout):
    """GNU time measures the exec child, avoiding inherited Python ru_maxrss."""
    directory = Path(directory)
    stdout, stderr, metrics = [directory / f"{name}.{suffix}" for suffix in ("stdout", "stderr", "time")]
    timed = ["/usr/bin/time", "-f", "%U\t%S\t%M", "-o", str(metrics), "--", *map(str, command)]
    with stdout.open("wb") as out, stderr.open("wb") as err:
        started = time.perf_counter()
        process = subprocess.Popen(timed, stdout=out, stderr=err, env=ENV, start_new_session=True)
        expired = threading.Event()

        def kill_timed_out():
            expired.set()
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                # The process group exited between the timer firing and this
                # kill, so the timeout is recorded but there is nothing to
                # signal. `expired` is already set, so the caller still treats
                # the sample as invalid.
                pass

        timer = threading.Timer(timeout, kill_timed_out)
        timer.start()
        try:
            # wait(timeout=...) polls in Python and can add tens of milliseconds.
            status = process.wait()
        finally:
            timer.cancel()
            timer.join()
        wall = time.perf_counter() - started
    row = {"command": list(map(str, command)), "exit_status": status, "timeout": expired.is_set(),
           "wall_seconds": wall, "stdout": str(stdout), "stderr": str(stderr),
           "user_seconds": None, "system_seconds": None, "max_rss_kib": None}
    # GNU time prefixes abnormal exits with a diagnostic. Never parse stderr as metrics.
    matches = re.findall(r"^([0-9.]+)\t([0-9.]+)\t([0-9]+)$",
                         metrics.read_text() if metrics.exists() else "", re.MULTILINE)
    if matches:
        user, system, rss = matches[-1]
        row.update(user_seconds=float(user), system_seconds=float(system), max_rss_kib=int(rss))
    return row


def histogram_totals(text):
    count = volume = 0
    for line in text.splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        size, calls = map(int, line.split())
        count += calls
        volume += size * calls
    if count == 0:
        raise ValueError("heaptrack histogram contains no allocations")
    return {"allocation_count": count, "allocated_bytes": volume}


def profile(command, directory, timeout):
    directory.mkdir()
    measured = measure(["heaptrack", "-o", str(directory / "trace"), *command], directory, "profile", timeout)
    traces = list(directory.glob("trace*.gz")) + list(directory.glob("trace*.zst"))
    if measured["exit_status"] or len(traces) != 1:
        raise RuntimeError(f"heaptrack failed: {directory}")
    histogram, stacks, report = [directory / n for n in ("histogram.txt", "peak-stacks.txt", "report.txt")]
    with report.open("w") as out:
        run(["heaptrack_print", "-f", traces[0], "--merge-backtraces", "0", "-H", histogram,
             "--flamegraph-cost-type", "peak", "-F", stacks], stdout=out, stderr=subprocess.STDOUT,
            timeout=timeout)
    return {**histogram_totals(histogram.read_text()), "trace": str(traces[0]),
            "report": str(report), "peak_stacks": str(stacks),
            "peak_live_bytes": sum(int(line.rsplit(" ", 1)[1]) for line in stacks.read_text().splitlines()),
            "note": "Instrumented allocations only; never use profiler RSS/time as baseline metrics."}


def command_for(binary, operation, source, output):
    args = [str(binary), str(source), *OPERATIONS[operation]]
    if operation not in ("check", "npages", "json"):
        args += ["--static-id", str(output)]
    return args


def require_markers(serialized, family):
    """The measured objects must survive, or a writer that drops them scores well.

    `generate_pdf` reaches every benchmarked object from the Catalog: each page
    carries `/Bench`, `objects` adds a Catalog `/Bench` array, and `stream` adds
    an `/EmbeddedFiles` name tree. Readability and page count alone accept an
    output that silently discarded them. The pinned qtest fixture is read
    unchanged from the qpdf source tree and carries none of these markers, so it
    has no generated invariant to check here.
    """
    if family not in GENERATED_FAMILIES:
        return
    # Every family puts a `/Bench` dictionary on each page, so that name alone
    # does not prove the family's own objects survived: an `objects` output
    # could drop the Catalog array and all its dictionaries and still match.
    # Require the shape each family actually contributes.
    markers = ["/Bench"]
    if family == "objects":
        markers.append('"/Bench": [')
    if family == "stream":
        markers.append("/EmbeddedFiles")
    missing = [marker for marker in markers if marker not in serialized]
    if missing:
        raise ValueError("output dropped benchmark objects: " + ",".join(missing))


def validate_sample(sample, operation, output, qpdf, expected_pages, family, timeout):
    if sample["exit_status"] or sample.get("timeout") or not sample["max_rss_kib"]:
        return {"ok": False, "reason": "command failed or metrics missing"}
    try:
        artifact = Path(sample["stdout"]) if operation in ("check", "npages", "json") else output
        if operation == "npages":
            if int(artifact.read_text().strip()) != expected_pages:
                raise ValueError("page count mismatch")
        elif operation == "json":
            with artifact.open() as stream:
                obj = json.load(stream)
            # A constant `{"qpdf": []}` must not pass as a serialized document,
            # so require the object map qpdf always emits as the second element
            # and at least one object beside the trailer. This holds for the
            # pinned fixture too, which has no generated marker to check.
            payload = obj.get("qpdf") if isinstance(obj, dict) else None
            if not isinstance(payload, list) or len(payload) < 2 or not isinstance(payload[1], dict):
                raise ValueError("missing qpdf JSON payload")
            if len(payload[1]) < 2:
                raise ValueError(f"qpdf JSON payload holds {len(payload[1])} objects")
            require_markers(artifact.read_text(errors="replace"), family)
        elif operation != "check":
            checked = subprocess.run([str(qpdf), "--check", str(output)], capture_output=True,
                                     env=ENV, timeout=timeout)
            if checked.returncode:
                raise ValueError("output qpdf --check: " + checked.stderr.decode(errors="replace"))
            if int(capture([qpdf, "--show-npages", output])) != expected_pages:
                raise ValueError("output page count mismatch")
            require_markers(capture([qpdf, "--json=2", output]), family)
        return {"ok": True, "bytes": artifact.stat().st_size, "sha256": digest(artifact)}
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        return {"ok": False, "reason": str(error)}


def benchmark_case(source, operation, binaries, args, out):
    case_id = source["id"] + "/" + operation
    directory = out / "cases" / source["id"] / operation
    directory.mkdir(parents=True)
    row = {"id": case_id, "input": source["id"], "operation": operation,
           "samples": {name: {"first": None, "warmup": [], "steady": []} for name in binaries}}
    row["validation"] = {"ok": True}
    phases = [("first", 0)] + [("warmup", n) for n in range(args.warmups)] + [("steady", n) for n in range(args.runs)]
    for round_number, (phase, index) in enumerate(phases):
        # Alternate pair order; no concurrent measurements compete for RAM/CPU.
        names = list(binaries) if round_number % 2 == 0 else list(reversed(binaries))
        for name in names:
            output = directory / f"{name}.pdf"
            sample = measure(command_for(binaries[name], operation, source["path"], output),
                             directory, f"{name}-{phase}-{index}", args.timeout)
            sample["validation"] = validate_sample(sample, operation, output, binaries["qpdf"],
                                                   source["pages"], source["family"], args.timeout)
            if phase == "first":
                row["samples"][name][phase] = sample
            else:
                row["samples"][name][phase].append(sample)
            if not sample["validation"]["ok"]:
                row["validation"]["ok"] = False
    if row["validation"]["ok"]:
        row["statistics"] = {}
        for name in binaries:
            row["statistics"][name] = {
                field: statistics_for([s[field] for s in row["samples"][name]["steady"]])
                for field in ("wall_seconds", "user_seconds", "system_seconds", "max_rss_kib")}
        row["ratios"] = {
            field: ratio(row["statistics"]["flpdf"][field]["median"], row["statistics"]["qpdf"][field]["median"])
            for field in row["statistics"]["qpdf"]}
        row["memory_verdict"] = verdict(*[[s["max_rss_kib"] for s in row["samples"][n]["steady"]]
                                         for n in ("qpdf", "flpdf")])
        row["time_verdict"] = verdict(*[[s["wall_seconds"] for s in row["samples"][n]["steady"]]
                                       for n in ("qpdf", "flpdf")])
        # Inspection diagnostics contain tool/path names; preserve raw output, no normalization.
        if operation != "check":
            hashes = [row["samples"][n]["steady"][-1]["validation"]["sha256"] for n in binaries]
            row["validation"]["byte_identical"] = hashes[0] == hashes[1]
    else:
        row["memory_verdict"] = row["time_verdict"] = "invalid"
    if case_id in args.heaptrack_case:
        row["heaptrack"] = {name: profile(command_for(binary, operation, source["path"], directory / f"{name}-heap.pdf"),
                                          directory / f"heaptrack-{name}", args.timeout)
                            for name, binary in binaries.items()}
    return row


def positive_csv(value):
    result = [int(x) for x in value.split(",")]
    if not result or min(result) <= 0 or len(result) != len(set(result)):
        raise argparse.ArgumentTypeError("expected distinct positive integers")
    return sorted(result)


def prepare_inputs(args, out, qpdf, source):
    directory = out / "inputs"
    directory.mkdir()
    inputs = []
    for family in GENERATED_FAMILIES:
        sizes = args.stream_mib if family == "stream" else args.sizes
        for size in sizes:
            identity = f"{family}-{size}"
            raw, path = directory / f"{identity}-raw.pdf", directory / f"{identity}.pdf"
            generate_pdf(raw, family, size * 1024 * 1024 if family == "stream" else size)
            # Generated ObjStms ensure preserve/disable modes exercise actual compressed objects.
            object_mode = "generate" if family == "objects" else "disable"
            run([qpdf, raw, "--static-id", f"--object-streams={object_mode}", path], capture_output=True)
            inputs.append({"id": identity, "family": family, "scale": size,
                           "unit": "MiB" if family == "stream" else "count", "path": str(path)})
    if not args.skip_qtest:
        path = source / "qpdf/qtest/qpdf/inline-images.pdf"
        inputs.append({"id": "qtest-inline-images", "family": "qtest", "scale": None,
                       "unit": None, "path": str(path)})
    for entry in inputs:
        run([qpdf, "--check", entry["path"]], capture_output=True)
        entry.update(bytes=Path(entry["path"]).stat().st_size, sha256=digest(entry["path"]),
                     pages=int(capture([qpdf, "--show-npages", entry["path"]])))
    return inputs


def scaling_rows(report):
    inputs = {entry["id"]: entry for entry in report["inputs"]}
    groups = {}
    for case in report["cases"]:
        entry = inputs[case["input"]]
        if entry["scale"] is not None and "statistics" in case:
            groups.setdefault((entry["family"], case["operation"]), []).append((entry, case))
    rows = []
    for (family, operation), cases in sorted(groups.items()):
        cases.sort(key=lambda pair: pair[0]["scale"])
        for (left, before), (right, after) in zip(cases, cases[1:]):
            rows.append({"family": family, "operation": operation, "from": left["scale"],
                         "to": right["scale"], "unit": left["unit"],
                         "rss_kib_per_unit": {
                             name: (after["statistics"][name]["max_rss_kib"]["median"] -
                                    before["statistics"][name]["max_rss_kib"]["median"]) /
                             (right["scale"] - left["scale"]) for name in ("qpdf", "flpdf")},
                         "rss_ratio_change": after["ratios"]["max_rss_kib"] - before["ratios"]["max_rss_kib"],
                         "endpoint_verdicts": [before["memory_verdict"], after["memory_verdict"]]})
    return rows


def write_report(report, out):
    report["scaling"] = scaling_rows(report)
    (out / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    lines = ["# qpdf/flpdf memory comparison", "",
             f"Status: {report['status']}. Full primary matrix: {report['primary_matrix_complete']}.",
             "Ratios are flpdf/qpdf. First invocation and warmups are excluded. No performance CI gate.", "",
             "| Case | qpdf RSS KiB | flpdf RSS KiB | RSS ratio | Time ratio | Memory verdict | Bytes equal |",
             "|---|---:|---:|---:|---:|---|---|"]
    for row in report["cases"]:
        if "statistics" not in row:
            lines.append(f"| {row['id']} | — | — | — | — | invalid | — |")
            continue
        rss = [row["statistics"][n]["max_rss_kib"]["median"] for n in ("qpdf", "flpdf")]
        lines.append(f"| {row['id']} | {rss[0]:.0f} | {rss[1]:.0f} | {row['ratios']['max_rss_kib']:.3f} | "
                     f"{row['ratios']['wall_seconds']:.3f} | {row['memory_verdict']} | "
                     f"{row['validation'].get('byte_identical', 'n/a')} |")
    if report["scaling"]:
        lines += ["", "## RSS growth between input sizes", "",
                  "Descriptive slopes only; consult endpoint sample spread before interpreting changes.", "",
                  "| Family / operation | Scale | qpdf KiB/unit | flpdf KiB/unit | RSS ratio change |",
                  "|---|---|---:|---:|---:|"]
        for row in report["scaling"]:
            lines.append(f"| {row['family']}/{row['operation']} | {row['from']} → {row['to']} {row['unit']} | "
                         f"{row['rss_kib_per_unit']['qpdf']:.3f} | {row['rss_kib_per_unit']['flpdf']:.3f} | "
                         f"{row['rss_ratio_change']:+.3f} |")
    (out / "summary.md").write_text("\n".join(lines) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="new artifact directory (must not exist)")
    parser.add_argument("--qpdf", default="/usr/bin/qpdf", type=Path)
    parser.add_argument("--flpdf", type=Path, help="external binary; provenance explicitly unverified")
    parser.add_argument("--flavor", choices=("default", "qpdf-zlib-compat"), default="default")
    parser.add_argument("--sizes", type=positive_csv, default=SIZES)
    parser.add_argument("--stream-mib", type=positive_csv, default=STREAM_MIB)
    parser.add_argument("--operations", default=",".join(OPERATIONS))
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--timeout", type=float, default=300)
    parser.add_argument("--skip-qtest", action="store_true", help="partial diagnostic matrix only")
    parser.add_argument("--heaptrack-case", action="append", default=[], metavar="INPUT/OPERATION")
    args = parser.parse_args()
    args.operations = args.operations.split(",")
    if args.runs < 1 or args.warmups < 0 or args.timeout <= 0:
        parser.error("runs/timeout must be positive; warmups must be nonnegative")
    if len(set(args.operations)) != len(args.operations) or set(args.operations) - OPERATIONS.keys():
        parser.error("operations must be distinct names from " + ",".join(OPERATIONS))
    qpdf = args.qpdf.resolve()
    version = capture([qpdf, "--version"])
    if not version.startswith("qpdf version 11.9.0\n"):
        parser.error("qpdf 11.9.0 is required")
    time_version = capture(["/usr/bin/time", "--version"])
    if "GNU" not in time_version:
        parser.error("Linux GNU time is required")
    if args.heaptrack_case and not all(shutil.which(t) for t in ("heaptrack", "heaptrack_print")):
        parser.error("heaptrack and heaptrack_print are required for --heaptrack-case")
    if args.output:
        out = args.output.resolve()
        if out.exists():
            parser.error(f"output already exists: {out}")
    else:
        out = Path(tempfile.mkdtemp(prefix="flpdf-perf-"))
    # Keep upstream fixtures AND generated outputs outside the repository.
    if out.is_relative_to(ROOT):
        parser.error("artifact directory must be outside the repository (qtest license isolation)")
    ancestor = out
    while not ancestor.exists():
        ancestor = ancestor.parent
    if subprocess.run(["git", "-C", str(ancestor), "rev-parse", "--show-toplevel"],
                      capture_output=True, env=ENV).returncode == 0:
        parser.error("artifact directory must be outside all Git worktrees (qtest license isolation)")
    out.mkdir(parents=True, exist_ok=True)
    print(f"Artifacts: {out}", flush=True)
    source = None
    if not args.skip_qtest:
        source = Path(capture([ROOT / "scripts/fetch-qpdf-source.sh", "--print-path"]))
        if capture(["git", "-C", source, "rev-parse", "HEAD"]) != QPDF_COMMIT:
            raise RuntimeError("qpdf source pin mismatch")
    commit = capture(["git", "-C", ROOT, "rev-parse", "HEAD"])
    dirty = capture(["git", "-C", ROOT, "status", "--porcelain"])
    build = None
    if args.flpdf:
        flpdf = args.flpdf.resolve()
    else:
        if os.environ.get("CARGO_BUILD_TARGET"):
            parser.error("unset CARGO_BUILD_TARGET for a native benchmark build")
        target = ROOT / "target" / f"perf-{args.flavor}"
        build = ["cargo", "build", "--locked", "--release", "-p", "flpdf-cli", "--bin", "flpdf",
                 "--target-dir", str(target)]
        if args.flavor != "default":
            build += ["--features", "qpdf-zlib-compat"]
        print("Building " + args.flavor, flush=True)
        with (out / "build.log").open("w") as log:
            run(build, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT)
        flpdf = target / "release/flpdf"
    report = {"schema_version": 1, "status": "running", "created_at": datetime.now(timezone.utc).isoformat(),
              "harness_commit": commit, "worktree_status": dirty, "harness_sha256": digest(__file__),
              "environment": {"platform": platform.platform(), "machine": platform.machine(),
                              "cpu_count": os.cpu_count(), "python": sys.version,
                              "gnu_time": time_version, "rustc": capture(["rustc", "--version"]),
                              "loadavg": os.getloadavg(), "locale": "C", "timezone": "UTC",
                              "cpu_affinity": sorted(os.sched_getaffinity(0)),
                              "cpu_model": next((line.split(":", 1)[1].strip() for line in
                                                 Path("/proc/cpuinfo").read_text().splitlines()
                                                 if line.startswith("model name")), None),
                              "memory_total": next((line for line in Path("/proc/meminfo").read_text().splitlines()
                                                    if line.startswith("MemTotal:")), None),
                              "allocator_environment": {k: os.environ[k] for k in
                                                        ("LD_PRELOAD", "GLIBC_TUNABLES", "MALLOC_ARENA_MAX",
                                                         "MALLOC_MMAP_THRESHOLD_") if k in os.environ},
                              "build_overrides": {k: os.environ[k] for k in
                                                  ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET") if k in os.environ}},
              "tools": {"qpdf": {"path": str(qpdf), "version": version, "sha256": digest(qpdf)},
                        "flpdf": {"path": str(flpdf), "version": capture([flpdf, "--version"]),
                                  "sha256": digest(flpdf), "flavor": args.flavor if build else "external-unverified",
                                  "source_commit": commit if build else None, "build_command": build,
                                  "provenance": "built-from-checkout" if build else "external-unverified"}},
              "qpdf_source": str(source) if source else None, "qpdf_source_commit": QPDF_COMMIT if source else None,
              "policy": {"runs": args.runs, "warmups": args.warmups, "ratio_target": 1.10,
                         "minimum_samples": 5, "max_relative_range": 0.10,
                         "first_run": "fresh process; filesystem cache NOT flushed; not cold-cache evidence",
                         "wall_clock": "perf_counter includes GNU time launch/wait; user/system resolution 0.01 s"},
              "primary_matrix_complete": (args.sizes == SIZES and args.stream_mib == STREAM_MIB
                                          and set(args.operations) == set(OPERATIONS) and not args.skip_qtest),
              "cases": []}
    if args.heaptrack_case:
        report["environment"]["heaptrack"] = capture(["heaptrack", "--version"])
        report["environment"]["heaptrack_print"] = capture(["heaptrack_print", "--version"])
    report["inputs"] = prepare_inputs(args, out, qpdf, source)
    identifiers = {i["id"] + "/" + op for i in report["inputs"] for op in args.operations}
    if set(args.heaptrack_case) - identifiers:
        parser.error("unknown heaptrack case: " + str(set(args.heaptrack_case) - identifiers))
    write_report(report, out)
    for entry in report["inputs"]:
        for operation in args.operations:
            print(f"{len(report['cases']) + 1}/{len(identifiers)} {entry['id']}/{operation}", flush=True)
            report["cases"].append(benchmark_case(entry, operation, {"qpdf": qpdf, "flpdf": flpdf}, args, out))
            write_report(report, out)
    report["status"] = "complete" if all(r["validation"]["ok"] for r in report["cases"]) else "validation-failed"
    write_report(report, out)
    print(f"{report['status']}: {out / 'summary.md'}", flush=True)
    return 0 if report["status"] == "complete" else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"perf-matrix: {error}", file=sys.stderr)
        sys.exit(1)
