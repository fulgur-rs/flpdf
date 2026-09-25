from __future__ import annotations

import contextlib
import importlib.util
import io
import sys
import tempfile
from pathlib import Path
import unittest


SCRIPT_PATH = Path(__file__).resolve().parents[1] / "check-qpdf-deviation-markers.py"


def load_checker():
    spec = importlib.util.spec_from_file_location("check_qpdf_deviation_markers", SCRIPT_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"unable to load {SCRIPT_PATH}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class ScanSourceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.module = load_checker()

    def test_file_with_no_markers_has_no_errors(self):
        errors = self.module.scan_source("pub fn f() {}\n")
        self.assertEqual([], errors)

    def test_accepts_well_formed_single_line_marker(self):
        source = (
            "if legacy_redirect_chase(r) {\n"
            "    // qpdf-deviation: no qpdf counterpart, test-only bridge\n"
            "}\n"
        )
        errors = self.module.scan_source(source)
        self.assertEqual([], errors)

    def test_rejects_single_line_marker_without_reason(self):
        source = "// qpdf-deviation:\n"
        errors = self.module.scan_source(source)
        self.assertEqual([(1, "qpdf-deviation requires ': <reason>'")], errors)

    def test_rejects_marker_without_colon(self):
        source = "// qpdf-deviation no colon here\n"
        errors = self.module.scan_source(source)
        self.assertEqual(
            [(1, "qpdf-deviation requires ': <reason>'")],
            errors,
        )

    def test_accepts_well_formed_start_end_block(self):
        source = (
            "// qpdf-deviation-start: no qpdf counterpart\n"
            "fn legacy_only() {}\n"
            "// qpdf-deviation-end\n"
        )
        errors = self.module.scan_source(source)
        self.assertEqual([], errors)

    def test_rejects_start_without_reason(self):
        # A reason-less `-start` never opens a block (mirrors cov:ignore), so
        # the following `-end` also errors as unmatched.
        source = "// qpdf-deviation-start:\n// qpdf-deviation-end\n"
        errors = self.module.scan_source(source)
        self.assertEqual(
            [
                (1, "qpdf-deviation-start requires ': <reason>'"),
                (2, "qpdf-deviation-end without matching start"),
            ],
            errors,
        )

    def test_rejects_nested_start(self):
        # The inner `-start` errors as nested but still counts as "in block";
        # the first `-end` closes it, so the second `-end` is unmatched.
        source = (
            "// qpdf-deviation-start: outer\n"
            "// qpdf-deviation-start: inner\n"
            "// qpdf-deviation-end\n"
            "// qpdf-deviation-end\n"
        )
        errors = self.module.scan_source(source)
        self.assertEqual(
            [
                (2, "nested qpdf-deviation-start"),
                (4, "qpdf-deviation-end without matching start"),
            ],
            errors,
        )

    def test_rejects_end_without_matching_start(self):
        source = "// qpdf-deviation-end\n"
        errors = self.module.scan_source(source)
        self.assertEqual(
            [(1, "qpdf-deviation-end without matching start")], errors
        )

    def test_rejects_start_without_colon(self):
        # No colon at all -- still errors, and the block never opens so the
        # following `-end` also errors as unmatched (same shape as
        # test_rejects_start_without_reason).
        source = "// qpdf-deviation-start missing-colon\n// qpdf-deviation-end\n"
        errors = self.module.scan_source(source)
        self.assertEqual(
            [
                (1, "qpdf-deviation-start requires ': <reason>'"),
                (2, "qpdf-deviation-end without matching start"),
            ],
            errors,
        )

    def test_rejects_end_with_colon(self):
        # A colon-bearing `-end` is malformed and does not close the block,
        # so the still-open `-start` also errors as unterminated (same shape
        # as test_rejects_end_with_trailing_text).
        source = "// qpdf-deviation-start: reason\n// qpdf-deviation-end:\n"
        errors = self.module.scan_source(source)
        self.assertEqual(
            [
                (2, "qpdf-deviation-end takes no text"),
                (1, "qpdf-deviation-start without matching end"),
            ],
            errors,
        )

    def test_rejects_end_with_trailing_text(self):
        # A malformed `-end` (unexpected trailing text) does not close the
        # block, so the still-open `-start` also errors as unterminated.
        source = "// qpdf-deviation-start: reason\n// qpdf-deviation-end: extra\n"
        errors = self.module.scan_source(source)
        self.assertEqual(
            [
                (2, "qpdf-deviation-end takes no text"),
                (1, "qpdf-deviation-start without matching end"),
            ],
            errors,
        )

    def test_rejects_unterminated_block(self):
        source = "// qpdf-deviation-start: reason\nfn f() {}\n"
        errors = self.module.scan_source(source)
        self.assertEqual(
            [(1, "qpdf-deviation-start without matching end")], errors
        )

    def test_rejects_token_inside_string_literal_as_malformed(self):
        # No real `//` comment exists on this line (the token is inside a
        # string literal), so it is flagged rather than silently ignored --
        # a false negative here would let a stray mention of the token
        # masquerade as a marker undetected.
        source = 'let s = "see qpdf-deviation docs";\n'
        errors = self.module.scan_source(source)
        self.assertEqual(
            [(1, "qpdf-deviation must be a `// qpdf-deviation[-start|-end]` comment")],
            errors,
        )

    def test_char_literal_quote_masks_a_marker_on_the_same_line(self):
        # Documented limitation (module docstring): a `"` inside a char/byte
        # literal like `b'"'` is misread as opening a string, so a real `//`
        # marker later on the same line is never seen and the line is
        # flagged as malformed instead of accepted. This locks in the
        # documented workaround (put the marker on its own line) so a future
        # change to the shared scanning algorithm doesn't silently drift
        # from what the docstring promises.
        source = "        b'\"' => value, // qpdf-deviation: reason\n"
        errors = self.module.scan_source(source)
        self.assertEqual(
            [(1, "qpdf-deviation must be a `// qpdf-deviation[-start|-end]` comment")],
            errors,
        )

    def test_raw_string_quote_lets_a_later_comment_look_like_a_marker(self):
        # Documented limitation (module docstring): raw string delimiters
        # are not tracked, so an embedded `"` inside a raw string (as in
        # crates/flpdf-qtest-tools/src/main.rs) can close the scanner's
        # naive string state early. A `//` sequence that is still textually
        # inside the raw string is then misread as a real comment and its
        # `qpdf-deviation: ...` text is accepted as a well-formed marker --
        # the higher-severity direction of the char-literal gap above (a
        # false accept, not a false reject). This locks in the documented
        # behavior so a future change to the shared scanning algorithm
        # doesn't silently drift from what the docstring promises.
        source = 'let s = r#"text " // qpdf-deviation: bogus"#;\n'
        errors = self.module.scan_source(source)
        self.assertEqual([], errors)


class CheckTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.module = load_checker()

    def _write(self, root: Path, relpath: str, content: str) -> None:
        full = root / relpath
        full.parent.mkdir(parents=True, exist_ok=True)
        full.write_text(content, encoding="utf-8")

    def test_returns_zero_when_all_markers_well_formed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(
                root,
                "crates/flpdf/src/lib.rs",
                "// qpdf-deviation: no qpdf counterpart\npub fn f() {}\n",
            )
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                code = self.module.check(root)
            self.assertEqual(0, code)
            self.assertIn("OK", buf.getvalue())

    def test_returns_nonzero_and_reports_file_and_line_for_malformed_marker(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(
                root,
                "crates/flpdf/src/lib.rs",
                "pub fn f() {}\n// qpdf-deviation:\n",
            )
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                code = self.module.check(root)
            self.assertEqual(1, code)
            output = buf.getvalue()
            self.assertIn("crates/flpdf/src/lib.rs:2", output)
            self.assertIn("qpdf-deviation requires ': <reason>'", output)

    def test_ignores_files_outside_crates_src(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(
                root,
                "crates/flpdf/tests/some_test.rs",
                "// qpdf-deviation:\n",
            )
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                code = self.module.check(root)
            self.assertEqual(0, code)


class MaxParseDepthDeviationInventoryTests(unittest.TestCase):
    """Keep the qpdf-less depth-limit inventory machine checked."""

    @classmethod
    def setUpClass(cls):
        cls.repo_root = SCRIPT_PATH.parents[1]

    def source(self, relative_path):
        return (self.repo_root / relative_path).read_text()

    def function_chunk(self, relative_path, signature):
        source = self.source(relative_path)
        start = source.index(signature)
        endings = [
            position
            for token in ("\nfn ", "\n    fn ")
            if (position := source.find(token, start + len(signature))) >= 0
        ]
        end = min(endings) if endings else len(source)
        return source[start:end]

    def test_qpdf_less_depth_branches_have_single_line_deviation_markers(self):
        guards = [
            (
                "crates/flpdf/src/writer/rewrite_renumber.rs",
                "fn collect_canonical_children_with_linearized_omission<",
                "if depth > MAX_PARSE_DEPTH",
            ),
            (
                "crates/flpdf/src/writer/rewrite_renumber.rs",
                "fn walk_resurrectable_handle(",
                "if depth > MAX_PARSE_DEPTH",
            ),
            (
                "crates/flpdf/src/writer/plain/body.rs",
                "fn collect_live_seed_handles(",
                "if depth > crate::parser::MAX_PARSE_DEPTH",
            ),
            (
                "crates/flpdf/src/writer/plain/body.rs",
                "fn collect_live_child_handles(",
                "if depth > crate::parser::MAX_PARSE_DEPTH",
            ),
            (
                "crates/flpdf/src/optimization.rs",
                "fn update_object_maps<R, F>(",
                "if pending.inline_depth > MAX_PARSE_DEPTH",
            ),
            (
                "crates/flpdf/src/linearization/plan.rs",
                "fn collect_direct_handle_refs(",
                "if depth > MAX_PARSE_DEPTH",
            ),
            (
                "crates/flpdf/src/linearization/plan.rs",
                "fn collect_direct_handle_refs_with_context(",
                "if depth > MAX_PARSE_DEPTH",
            ),
            (
                "crates/flpdf/src/linearization/plan.rs",
                "fn collect_direct_handle_refs_with_stream_parameters_context(",
                "if depth > MAX_PARSE_DEPTH",
            ),
            (
                "crates/flpdf/src/object_handle.rs",
                "fn write_handle(\n        &mut self,",
                "if depth > crate::parser::MAX_PARSE_DEPTH",
            ),
        ]
        for path, signature, guard in guards:
            with self.subTest(path=path, signature=signature):
                chunk = self.function_chunk(path, signature)
                lines = chunk.splitlines()
                guard_index = next(
                    (index for index, line in enumerate(lines) if guard in line),
                    None,
                )
                self.assertIsNotNone(guard_index, f"missing guard {guard}")
                assert guard_index is not None
                self.assertIn(
                    "// qpdf-deviation:",
                    lines[guard_index - 1].strip(),
                    f"{path}: {guard} must carry a qpdf deviation marker",
                )

    def test_isolated_depth_guard_helpers_are_deprecated(self):
        helpers = [
            ("crates/flpdf/src/writer/object.rs", "UnparseWalkDepthGuard"),
            ("crates/flpdf/src/writer/plain/body.rs", "ContentEmitWalkDepthGuard"),
            ("crates/flpdf/src/object_handle.rs", "DirectGraphWalkDepthGuard"),
        ]
        for path, helper in helpers:
            with self.subTest(path=path, helper=helper):
                lines = self.source(path).splitlines()
                struct_line = next(
                    index
                    for index, line in enumerate(lines)
                    if line.strip() == f"struct {helper};"
                )
                enter_line = next(
                    index
                    for index in range(struct_line + 1, len(lines))
                    if "fn enter()" in lines[index]
                )
                prefix = "\n".join(lines[max(0, enter_line - 5) : enter_line])
                self.assertIn("#[deprecated(", prefix)
                self.assertIn("no qpdf counterpart", prefix)

    def test_pdf_parser_depth_limit_is_not_marked_as_a_deviation(self):
        chunk = self.function_chunk(
            "crates/flpdf/src/parser.rs",
            "fn push_frame(&mut self, frames: &mut Vec<LiveFrame>, token: LiveToken)",
        )
        self.assertIn("frames.len() >= MAX_PARSE_DEPTH", chunk)
        self.assertNotIn("qpdf-deviation", chunk)


if __name__ == "__main__":
    unittest.main()
