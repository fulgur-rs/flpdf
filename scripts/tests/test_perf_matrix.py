"""Contracts for the diagnostic benchmark harness (no timing thresholds in CI)."""
import importlib.util
import json
from pathlib import Path
import signal
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "perf-matrix.py"
spec = importlib.util.spec_from_file_location("perf_matrix", SCRIPT)
perf = importlib.util.module_from_spec(spec)
spec.loader.exec_module(perf)


def pinned_qpdf_available():
    return (shutil.which("qpdf") is not None and
            subprocess.run(["qpdf", "--version"], capture_output=True, text=True)
            .stdout.startswith("qpdf version 11.9.0\n"))


def worktree_artifact_temporary_directory():
    root = perf.ROOT / "target" / "perf-artifacts"
    root.mkdir(parents=True, exist_ok=True)
    return tempfile.TemporaryDirectory(dir=root)


class MeasurementContracts(unittest.TestCase):
    def test_default_artifact_directory_is_worktree_local(self):
        output = perf.resolve_artifact_directory(None)
        try:
            artifact_root = (perf.ROOT / "target" / "perf-artifacts").resolve()
            self.assertTrue(output.is_relative_to(artifact_root), output)
        finally:
            shutil.rmtree(output)

    def test_explicit_worktree_artifact_directory_is_accepted(self):
        artifact_root = perf.ROOT / "target" / "perf-artifacts"
        artifact_root.mkdir(parents=True, exist_ok=True)
        output = artifact_root / "contract-accepted"
        self.assertFalse(output.exists())
        self.assertEqual(perf.resolve_artifact_directory(output), output.resolve())

    def test_arbitrary_source_tree_artifact_directory_is_rejected(self):
        output = perf.ROOT / "scripts" / "contract-rejected"
        with self.assertRaisesRegex(ValueError, "target/perf-artifacts"):
            perf.resolve_artifact_directory(output)

    def test_statistics_keep_spread_and_do_not_divide_by_zero(self):
        self.assertEqual(perf.statistics_for([10, 12, 11]),
                         {"median": 11, "min": 10, "max": 12, "relative_range": 2 / 11})
        self.assertIsNone(perf.statistics_for([0, 0])["relative_range"])
        self.assertIsNone(perf.ratio(1, 0))

    def test_incomplete_and_variable_samples_cannot_pass(self):
        self.assertEqual(perf.verdict([100] * 5, [105] * 5), "within-target")
        self.assertEqual(perf.verdict([100] * 5, [120] * 5), "above-target")
        self.assertEqual(perf.verdict([100], [105]), "insufficient-samples")
        self.assertEqual(perf.verdict([100] * 5, [100, 100, 100, 100, 150]), "variable")

    def test_histogram_total_is_allocation_volume_not_peak(self):
        self.assertEqual(perf.histogram_totals("# size count\n8\t3\n32\t2\n"),
                         {"allocation_count": 5, "allocated_bytes": 88})
        with self.assertRaises(ValueError):
            perf.histogram_totals("not a histogram")

    def test_scaling_compares_same_family_and_operation(self):
        report = {"inputs": [], "cases": []}
        for size, qpdf, flpdf in [(100, 1000, 2000), (1000, 1900, 4700)]:
            identity = f"pages-{size}"
            report["inputs"].append({"id": identity, "family": "pages", "scale": size, "unit": "count"})
            report["cases"].append({"input": identity, "operation": "rewrite", "statistics": {
                name: {"max_rss_kib": {"median": value}} for name, value in (("qpdf", qpdf), ("flpdf", flpdf))},
                "ratios": {"max_rss_kib": flpdf / qpdf}, "memory_verdict": "above-target"})
        rows = perf.scaling_rows(report)
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["rss_kib_per_unit"], {"qpdf": 1, "flpdf": 3})
        self.assertGreater(rows[0]["rss_ratio_change"], 0)

    @unittest.skipUnless(Path("/usr/bin/time").exists(), "GNU time required")
    def test_measure_records_failure_and_separates_streams(self):
        with tempfile.TemporaryDirectory() as tmp:
            result = perf.measure(
                [sys.executable, "-c", "import sys; print('out'); print('err', file=sys.stderr); sys.exit(7)"],
                Path(tmp), "failed", 10)
            self.assertEqual(result["exit_status"], 7)
            self.assertEqual(Path(result["stdout"]).read_text(), "out\n")
            self.assertEqual(Path(result["stderr"]).read_text(), "err\n")
            self.assertGreater(result["max_rss_kib"], 0)
            self.assertGreater(result["wall_seconds"], 0)

    @unittest.skipUnless(Path("/usr/bin/time").exists(), "GNU time required")
    def test_timeout_is_invalid_even_without_metrics(self):
        with tempfile.TemporaryDirectory() as tmp:
            sample = perf.measure([sys.executable, "-c", "import time; time.sleep(10)"],
                                  Path(tmp), "timeout", 0.05)
            self.assertTrue(sample["timeout"])
            self.assertNotEqual(sample["exit_status"], 0)
            self.assertLess(sample["wall_seconds"], 5)
            self.assertFalse(perf.validate_sample(sample, "check", None, None, 1, "pages", 10)["ok"])

    @unittest.skipUnless(Path("/usr/bin/time").exists(), "GNU time required")
    def test_measure_kills_process_group_when_wait_is_interrupted(self):
        class InterruptedProcess:
            pid = 12345

            def __init__(self):
                self.wait_calls = 0

            def wait(self):
                self.wait_calls += 1
                if self.wait_calls == 1:
                    raise KeyboardInterrupt
                return -signal.SIGKILL

        process = InterruptedProcess()
        timer = mock.Mock()
        with tempfile.TemporaryDirectory() as tmp:
            with mock.patch.object(perf.subprocess, "Popen", return_value=process):
                with mock.patch.object(perf.threading, "Timer", return_value=timer):
                    with mock.patch.object(perf.os, "killpg") as killpg:
                        with self.assertRaises(KeyboardInterrupt):
                            perf.measure([sys.executable, "-c", "pass"],
                                         Path(tmp), "interrupt", 10)
        killpg.assert_called_once_with(process.pid, perf.signal.SIGKILL)
        self.assertEqual(process.wait_calls, 2)
        timer.start.assert_called_once_with()
        timer.cancel.assert_called_once_with()
        timer.join.assert_called_once_with()

    def test_successful_exit_with_invalid_json_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "output"
            sample = {"exit_status": 0, "max_rss_kib": 100, "stdout": str(output)}
            for body in ("not json", "{}", "[]"):
                output.write_text(body)
                self.assertFalse(perf.validate_sample(sample, "json", None, None, 1, "pages", 10)["ok"])

    def test_json_payload_must_contain_the_benchmarked_objects(self):
        """A constant `{"qpdf": []}` must not pass as a serialized document."""
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "output"
            sample = {"exit_status": 0, "max_rss_kib": 100, "stdout": str(output)}
            output.write_text('{"qpdf": []}')
            self.assertFalse(
                perf.validate_sample(sample, "json", None, None, 1, "pages", 10)["ok"])
            for thin in ('{"qpdf": [{}]}', '{"qpdf": [{}, {}]}',
                         '{"qpdf": [{}, {"obj:1 0 R": {}}]}'):
                output.write_text(thin)
                self.assertFalse(
                    perf.validate_sample(sample, "json", None, None, 1, "qtest", 10)["ok"],
                    thin)
            output.write_text(
                '{"qpdf": [{}, {"obj:1 0 R": {"value": {"/Bench": true}}, "obj:2 0 R": {}}]}')
            self.assertTrue(
                perf.validate_sample(sample, "json", None, None, 1, "pages", 10)["ok"])
            self.assertTrue(
                perf.validate_sample(sample, "json", None, None, 1, "qtest", 10)["ok"])

    @unittest.skipUnless(pinned_qpdf_available(), "qpdf 11.9.0 required")
    def test_linearize_requires_a_linearized_output(self):
        """An ordinary rewrite is a valid PDF; the operation must still be checked."""
        qpdf = shutil.which("qpdf")
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "input.pdf"
            perf.generate_pdf(source, "pages", 2)
            sample = {"exit_status": 0, "max_rss_kib": 100, "stdout": str(Path(tmp) / "unused")}
            for operation, expected in (("linearize", False), ("rewrite", True)):
                output = Path(tmp) / f"{operation}.pdf"
                subprocess.run([qpdf, str(source), "--static-id", str(output)],
                               capture_output=True, check=True)
                result = perf.validate_sample(sample, operation, output, qpdf, 2, "pages", 60)
                self.assertEqual(result["ok"], expected, f"{operation}: {result}")

    def test_marker_check_requires_the_embedded_file_tree_for_streams(self):
        perf.require_markers("/Bench /EmbeddedFiles", "stream")
        perf.require_markers("/Bench", "pages")
        for serialized, family in (("/Bench", "stream"), ("", "pages"), ("/EmbeddedFiles", "objects")):
            with self.assertRaises(ValueError):
                perf.require_markers(serialized, family)
        # A page-level `/Bench` must not stand in for the objects family's
        # Catalog array: dropping the array has to fail even when pages survive.
        perf.require_markers('"/Bench": {} "/Bench": [', "objects")
        with self.assertRaises(ValueError):
            perf.require_markers('"/Bench": {"/Flag": true}', "objects")
        # The pinned qtest fixture is read unchanged and has no generated marker.
        perf.require_markers("", "qtest")
        self.assertNotIn("qtest", perf.GENERATED_FAMILIES)

    def test_zero_rss_cannot_be_reported_as_memory_improvement(self):
        sample = {"exit_status": 0, "max_rss_kib": 0}
        self.assertFalse(perf.validate_sample(sample, "check", None, None, 1, "pages", 10)["ok"])

    def test_check_requires_qpdf_check_summary_output(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "check.stdout"
            sample = {"exit_status": 0, "max_rss_kib": 100, "stdout": str(output)}
            output.write_text("fake checker succeeded\\n")
            self.assertFalse(
                perf.validate_sample(sample, "check", None, None, 1, "pages", 10)["ok"]
            )
            output.write_text(
                "PDF Version: 1.7\\n"
                "File is not encrypted\\n"
                "File is not linearized\\n"
            )
            self.assertTrue(
                perf.validate_sample(sample, "check", None, None, 1, "pages", 10)["ok"]
            )

    def test_pdf_recipe_offsets_and_reachability(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "input.pdf"
            perf.generate_pdf(path, "objects", 4)
            data = path.read_bytes()
            xref = int(data.split(b"startxref\n")[1].splitlines()[0])
            self.assertEqual(data[xref:xref + 4], b"xref")
            entries = data[xref:].splitlines()[3:]
            for index in range(1, 8):
                offset = int(entries[index - 1].split()[0])
                self.assertTrue(data[offset:].startswith(f"{index} 0 obj\n".encode()))
            self.assertIn(b"/Bench [4 0 R 5 0 R 6 0 R 7 0 R]", data)


@unittest.skipUnless(pinned_qpdf_available(), "qpdf 11.9.0 required for live harness contracts")
class LiveContracts(unittest.TestCase):
    def test_stateful_noop_writer_cannot_reuse_a_previous_output(self):
        with worktree_artifact_temporary_directory() as tmp:
            root = Path(tmp)
            fake = root / "fake-flpdf"
            fake.write_text(
                "#!/usr/bin/env python3\n"
                "from pathlib import Path\n"
                "import shutil\n"
                "import sys\n"
                "if len(sys.argv) == 2 and sys.argv[1] == '--version':\n"
                "    print('qpdf version 11.9.0')\n"
                "    raise SystemExit\n"
                "source = Path(sys.argv[1])\n"
                "output = Path(sys.argv[-1])\n"
                "seen = source.with_suffix('.seen')\n"
                "if not seen.exists():\n"
                "    seen.write_text('seen')\n"
                "    shutil.copyfile(source, output)\n"
            )
            fake.chmod(0o755)
            out = root / "perf"
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--output", str(out),
                 "--flpdf", str(fake), "--sizes", "2", "--stream-mib", "1",
                 "--operations", "rewrite", "--runs", "1", "--warmups", "0",
                 "--skip-qtest"],
                capture_output=True, text=True)
            self.assertEqual(result.returncode, 1, result.stderr)
            report = json.loads((out / "results.json").read_text())
            self.assertEqual(report["status"], "validation-failed")
            self.assertTrue(any(not case["validation"]["ok"] for case in report["cases"]))

    def test_validated_phase_outputs_are_not_retained(self):
        with worktree_artifact_temporary_directory() as tmp:
            root = Path(tmp)
            out = root / "perf"
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--output", str(out),
                 "--sizes", "2", "--stream-mib", "1",
                 "--operations", "rewrite", "--runs", "2", "--warmups", "1",
                 "--skip-qtest"],
                capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads((out / "results.json").read_text())
            self.assertTrue(all(case["validation"]["ok"] for case in report["cases"]))
            self.assertNotEqual(report["cases"], [])
            # Each case runs four phases per tool. Keeping every artifact would
            # leave those PDFs behind; validation already stored size and digest.
            # Generated inputs live outside `cases/` and are still expected.
            retained = sorted(str(path.relative_to(out))
                              for path in (out / "cases").rglob("*.pdf"))
            self.assertEqual(retained, [], "validated phase outputs were retained")

    def test_failed_validation_preserves_report_and_exits_nonzero(self):
        with worktree_artifact_temporary_directory() as tmp:
            out = Path(tmp) / "invalid"
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--output", str(out),
                 "--flpdf", "/usr/bin/true", "--sizes", "2", "--stream-mib", "1",
                 "--operations", "npages", "--runs", "1", "--warmups", "0", "--skip-qtest"],
                capture_output=True, text=True)
            self.assertEqual(result.returncode, 1, result.stderr)
            report = json.loads((out / "results.json").read_text())
            self.assertEqual(report["status"], "validation-failed")
            self.assertEqual(len(report["cases"]), 4)
            for case in report["cases"]:
                self.assertEqual(case["memory_verdict"], "invalid")
                self.assertNotIn("statistics", case)
                self.assertFalse(case["validation"]["ok"])

    def test_generated_families_are_valid(self):
        with tempfile.TemporaryDirectory() as tmp:
            for family in ("pages", "content", "objects", "stream"):
                with self.subTest(family=family):
                    path = Path(tmp) / f"{family}.pdf"
                    perf.generate_pdf(path, family, 3 if family != "stream" else 1024)
                    checked = subprocess.run(["qpdf", "--check", str(path)], capture_output=True)
                    self.assertEqual(checked.returncode, 0, checked.stderr)

    def test_end_to_end_incomplete_matrix_cannot_claim_epic_success(self):
        with worktree_artifact_temporary_directory() as tmp:
            out = Path(tmp) / "results with spaces"
            command = [sys.executable, str(SCRIPT), "--output", str(out),
                       "--flpdf", shutil.which("qpdf"), "--sizes", "2", "--stream-mib", "1",
                       "--operations", "rewrite,json,npages", "--runs", "1", "--warmups", "0",
                       "--skip-qtest"]
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads((out / "results.json").read_text())
            self.assertEqual(len(report["cases"]), 12)
            self.assertFalse(report["primary_matrix_complete"])
            self.assertEqual(report["tools"]["flpdf"]["provenance"], "external-unverified")
            for case in report["cases"]:
                self.assertTrue(case["validation"]["ok"], case)
                self.assertEqual(case["memory_verdict"], "insufficient-samples")
                self.assertEqual(len(case["samples"]["qpdf"]["steady"]), 1)
                self.assertIn("first", case["samples"]["qpdf"])
            # Existing artifacts must never be overwritten by a new invocation.
            again = subprocess.run(command, capture_output=True, text=True)
            self.assertNotEqual(again.returncode, 0)
            self.assertIn("already exists", again.stderr)


if __name__ == "__main__":
    unittest.main()
