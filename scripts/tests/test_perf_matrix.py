"""Contracts for the diagnostic benchmark harness (no timing thresholds in CI)."""
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "perf-matrix.py"
spec = importlib.util.spec_from_file_location("perf_matrix", SCRIPT)
perf = importlib.util.module_from_spec(spec)
spec.loader.exec_module(perf)


def pinned_qpdf_available():
    return (shutil.which("qpdf") is not None and
            subprocess.run(["qpdf", "--version"], capture_output=True, text=True)
            .stdout.startswith("qpdf version 11.9.0\n"))


class MeasurementContracts(unittest.TestCase):
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
            self.assertFalse(perf.validate_sample(sample, "check", None, None, 1, 10)["ok"])

    def test_successful_exit_with_invalid_json_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "output"
            sample = {"exit_status": 0, "max_rss_kib": 100, "stdout": str(output)}
            for body in ("not json", "{}", "[]"):
                output.write_text(body)
                self.assertFalse(perf.validate_sample(sample, "json", None, None, 1, 10)["ok"])

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
    def test_failed_validation_preserves_report_and_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as tmp:
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
        with tempfile.TemporaryDirectory() as tmp:
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
