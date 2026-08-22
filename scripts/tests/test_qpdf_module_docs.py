from __future__ import annotations

import importlib.util
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT_PATH = Path(__file__).resolve().parents[1] / "qpdf-module-docs.py"


def load_checker():
    spec = importlib.util.spec_from_file_location("qpdf_module_docs", SCRIPT_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"unable to load {SCRIPT_PATH}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class ClassificationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.module = load_checker()

    def test_accepts_single_mirror(self):
        result = self.module.classify_source(
            Path("crates/flpdf/src/pdf_version.rs"),
            "//! Mirrors qpdf 11.9.0 libqpdf/PDFVersion.cc.\n\npub struct V;\n",
        )
        self.assertEqual(
            ("mirror", "libqpdf/PDFVersion.cc"),
            (result.kind, result.text),
        )

    def test_accepts_multiple_mirror_files(self):
        result = self.module.classify_source(
            Path("crates/flpdf/src/json.rs"),
            "//! Mirrors qpdf 11.9.0 libqpdf/JSON.cc, libqpdf/JSONHandler.cc.\n",
        )
        self.assertEqual(
            ("mirror", "libqpdf/JSON.cc, libqpdf/JSONHandler.cc"),
            (result.kind, result.text),
        )

    def test_accepts_multiple_mirror_files_joined_by_and(self):
        result = self.module.classify_source(
            Path("crates/flpdf/src/security/rc4.rs"),
            "//! Mirrors qpdf 11.9.0 libqpdf/RC4.cc and libqpdf/RC4_native.cc.\n",
        )
        self.assertEqual(
            ("mirror", "libqpdf/RC4.cc, libqpdf/RC4_native.cc"),
            (result.kind, result.text),
        )

    def test_accepts_non_mirror_reason(self):
        result = self.module.classify_source(
            Path("crates/flpdf/src/fonts.rs"),
            "//! qpdf correspondence: flpdf-only font inspection.\n",
        )
        self.assertEqual(
            ("correspondence", "flpdf-only font inspection"),
            (result.kind, result.text),
        )

    def test_accepts_classification_after_utf8_bom(self):
        try:
            result = self.module.classify_source(
                Path("crates/flpdf/src/example.rs"),
                "\ufeff//! qpdf correspondence: bom-prefixed module.\npub struct X;\n",
            )
        except ValueError as error:
            self.fail(str(error))

        self.assertEqual(
            ("correspondence", "bom-prefixed module"),
            (result.kind, result.text),
        )

    def test_accepts_classification_after_each_rust_whitespace_character(self):
        rust_whitespace = "\u0009\u000a\u000b\u000c\u000d\u0020\u0085\u200e\u200f\u2028\u2029"

        for whitespace in rust_whitespace:
            with self.subTest(codepoint=f"U+{ord(whitespace):04X}"):
                result = self.module.classify_source(
                    Path("crates/flpdf/src/example.rs"),
                    f"{whitespace}//! qpdf correspondence: whitespace-prefixed module.\n",
                )
                self.assertEqual(
                    ("correspondence", "whitespace-prefixed module"),
                    (result.kind, result.text),
                )

    def test_accepts_classification_after_shebang(self):
        try:
            result = self.module.classify_source(
                Path("crates/flpdf/src/example.rs"),
                "#!/usr/bin/env rustx\n"
                "//! qpdf correspondence: shebang-prefixed module.\n"
                "pub struct X;\n",
            )
        except ValueError as error:
            self.fail(str(error))

        self.assertEqual(
            ("correspondence", "shebang-prefixed module"),
            (result.kind, result.text),
        )

    def test_accepts_classification_after_inner_attribute(self):
        result = self.module.classify_source(
            Path("crates/flpdf/src/lib.rs"),
            '#![forbid(unsafe_code)]\n//! qpdf correspondence: crate root.\n\npub mod x;\n',
        )
        self.assertEqual(("correspondence", "crate root"), (result.kind, result.text))

    def test_accepts_classification_after_inner_attribute_on_same_line(self):
        result = self.module.classify_source(
            Path("crates/flpdf/src/lib.rs"),
            "#![allow(dead_code)] //! qpdf correspondence: crate root.\n"
            "pub mod x;\n",
        )

        self.assertEqual(("correspondence", "crate root"), (result.kind, result.text))

    def test_accepts_classification_after_whitespace_separated_inner_attribute(self):
        result = self.module.classify_source(
            Path("crates/flpdf/src/lib.rs"),
            "# ! [allow(dead_code)]\n"
            "//! qpdf correspondence: crate root.\n"
            "pub mod x;\n",
        )

        self.assertEqual(("correspondence", "crate root"), (result.kind, result.text))

    def test_accepts_classification_after_comment_separated_inner_attribute(self):
        result = self.module.classify_source(
            Path("crates/flpdf/src/lib.rs"),
            "# /* between */ ! /* between */ [allow(dead_code)]\n"
            "//! qpdf correspondence: crate root.\n"
            "pub mod x;\n",
        )

        self.assertEqual(("correspondence", "crate root"), (result.kind, result.text))

    def test_accepts_classification_after_line_comment_separated_inner_attribute(self):
        result = self.module.classify_source(
            Path("crates/flpdf/src/lib.rs"),
            "# // between\n"
            "! [allow(dead_code)]\n"
            "//! qpdf correspondence: crate root.\n"
            "pub mod x;\n",
        )

        self.assertEqual(("correspondence", "crate root"), (result.kind, result.text))

    def test_accepts_classification_after_multiline_comment_separated_inner_attribute(
        self,
    ):
        result = self.module.classify_source(
            Path("crates/flpdf/src/lib.rs"),
            "# /* between\n"
            "*/ ! [allow(dead_code)]\n"
            "//! qpdf correspondence: crate root.\n"
            "pub mod x;\n",
        )

        self.assertEqual(("correspondence", "crate root"), (result.kind, result.text))

    def test_accepts_classification_after_inner_attribute_with_lifetime(self):
        try:
            result = self.module.classify_source(
                Path("crates/flpdf/src/lib.rs"),
                "#![doc = stringify!('a)] // lifetime's spelling\n"
                "//! qpdf correspondence: crate root.\n\n"
                "pub mod x;\n",
            )
        except ValueError as error:
            self.fail(str(error))

        self.assertEqual(
            ("correspondence", "crate root"),
            (result.kind, result.text),
        )

    def test_accepts_classification_after_inner_attribute_with_newer_xid_lifetime(
        self,
    ):
        result = self.module.classify_source(
            Path("crates/flpdf/src/lib.rs"),
            "#![doc = stringify!('࢏)] // lifetime's spelling\n"
            "//! qpdf correspondence: crate root.\n\n"
            "pub mod x;\n",
        )

        self.assertEqual(
            ("correspondence", "crate root"),
            (result.kind, result.text),
        )

    def test_accepts_classification_after_inner_attribute_with_bracket_char_literal(
        self,
    ):
        result = self.module.classify_source(
            Path("crates/flpdf/src/lib.rs"),
            "#![doc = stringify!(']')]\n"
            "//! qpdf correspondence: crate root.\n\n"
            "pub mod x;\n",
        )

        self.assertEqual(
            ("correspondence", "crate root"),
            (result.kind, result.text),
        )

    def test_ignores_classification_like_text_inside_multiline_inner_attribute(self):
        self.assert_invalid(
            '#![doc = "ordinary\n'
            "//! qpdf correspondence: fake.\n"
            '"]\n'
            "pub struct X;\n",
            "missing",
        )

    def test_ignores_bracket_inside_multiline_inner_attribute_string(self):
        self.assert_invalid(
            '#![doc = "]\n'
            "//! qpdf correspondence: fake.\n"
            '"]\n'
            "pub struct X;\n",
            "missing",
        )

    def assert_invalid(self, source: str, message: str):
        with self.assertRaisesRegex(ValueError, message):
            self.module.classify_source(Path("crates/flpdf/src/example.rs"), source)

    def test_rejects_missing_classification(self):
        self.assert_invalid("//! Ordinary docs.\n\npub struct X;\n", "missing")

    def test_ignores_classification_after_unicode_line_separator_in_line_comment(self):
        self.assert_invalid(
            "// ordinary\u2028//! qpdf correspondence: fake.\n"
            "pub struct X;\n",
            "missing",
        )

    def test_ignores_classification_like_text_inside_block_comment(self):
        self.assert_invalid(
            "/*\n"
            "//! qpdf correspondence: old reason.\n"
            "*/\n"
            "pub struct X;\n",
            "missing",
        )

    def test_ignores_classification_like_text_inside_nested_block_comment(self):
        self.assert_invalid(
            "/* outer\n"
            "/* inner */\n"
            "//! qpdf correspondence: old reason.\n"
            "*/\n"
            "pub struct X;\n",
            "missing",
        )

    def test_accepts_real_classification_after_block_comment(self):
        result = self.module.classify_source(
            Path("crates/flpdf/src/example.rs"),
            "/*\n"
            "//! qpdf correspondence: old reason.\n"
            "*/\n"
            "//! qpdf correspondence: current reason.\n"
            "pub struct X;\n",
        )

        self.assertEqual(
            ("correspondence", "current reason"),
            (result.kind, result.text),
        )

    def test_accepts_classification_after_closed_inline_block_comment(self):
        try:
            result = self.module.classify_source(
                Path("crates/flpdf/src/example.rs"),
                "/* ordinary */ //! qpdf correspondence: current reason.\n"
                "pub struct X;\n",
            )
        except ValueError as error:
            self.fail(str(error))

        self.assertEqual(
            ("correspondence", "current reason"),
            (result.kind, result.text),
        )

    def test_rejects_duplicate_classification(self):
        self.assert_invalid(
            "//! Mirrors qpdf 11.9.0 libqpdf/QPDF.cc.\n"
            "//! qpdf correspondence: duplicate.\n",
            "multiple",
        )

    def test_rejects_empty_non_mirror_reason(self):
        self.assert_invalid("//! qpdf correspondence: .\n", "non-empty")

    def test_rejects_wrong_qpdf_version(self):
        self.assert_invalid(
            "//! Mirrors qpdf 12.0.0 libqpdf/QPDF.cc.\n",
            "11\\.9\\.0",
        )

    def test_rejects_absolute_qpdf_path(self):
        self.assert_invalid(
            "//! Mirrors qpdf 11.9.0 /libqpdf/QPDF.cc.\n",
            "invalid qpdf path",
        )

    def test_rejects_non_libqpdf_path(self):
        self.assert_invalid(
            "//! Mirrors qpdf 11.9.0 qpdf/fix-qdf.cc.\n",
            "invalid qpdf path",
        )

    def test_rejects_non_cc_path(self):
        self.assert_invalid(
            "//! Mirrors qpdf 11.9.0 libqpdf/QPDF.hh.\n",
            "invalid qpdf path",
        )

    def test_rejects_mirror_without_terminal_period(self):
        self.assert_invalid(
            "//! Mirrors qpdf 11.9.0 libqpdf/QPDF.cc\n",
            "terminal period",
        )

    def test_rejects_correspondence_without_terminal_period(self):
        self.assert_invalid(
            "//! qpdf correspondence: QPDF.cc responsibility\n",
            "terminal period",
        )

    def test_rejects_non_rust_whitespace_after_correspondence_period(self):
        self.assert_invalid(
            "//! qpdf correspondence: valid.\u001c\n",
            "terminal period",
        )

    def test_rejects_non_rust_whitespace_after_mirror_period(self):
        self.assert_invalid(
            "//! Mirrors qpdf 11.9.0 libqpdf/QPDF.cc.\u001c\n",
            "terminal period",
        )


class GeneratorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.module = load_checker()

    def test_module_path_code_span_uses_longer_backtick_delimiter(self):
        rendered = self.module.render_index(
            [
                (
                    Path("crates/flpdf/src/a`b.rs"),
                    self.module.Classification("correspondence", "test module"),
                )
            ]
        )

        self.assertIn(
            "| ``crates/flpdf/src/a`b.rs`` | correspondence | test module |",
            rendered,
        )

    def test_module_path_code_span_pads_edge_backticks(self):
        rendered = self.module.render_index(
            [
                (
                    Path("`leading.rs"),
                    self.module.Classification("correspondence", "leading"),
                ),
                (
                    Path("trailing.rs`"),
                    self.module.Classification("correspondence", "trailing"),
                ),
            ]
        )

        self.assertIn(
            "| `` `leading.rs `` | correspondence | leading |",
            rendered,
        )
        self.assertIn(
            "| `` trailing.rs` `` | correspondence | trailing |",
            rendered,
        )

    def test_module_path_code_span_escapes_backslash_before_pipe(self):
        rendered = self.module.render_index(
            [
                (
                    Path(r"crates/flpdf/src/a\|b.rs"),
                    self.module.Classification("correspondence", "test module"),
                )
            ]
        )

        self.assertIn(
            r"| `crates/flpdf/src/a\\\|b.rs` | correspondence | test module |",
            rendered,
        )

    def test_module_path_code_span_preserves_lone_backslash(self):
        rendered = self.module.render_index(
            [
                (
                    Path(r"crates/flpdf/src/a\b.rs"),
                    self.module.Classification("correspondence", "test module"),
                )
            ]
        )

        self.assertIn(
            r"| `crates/flpdf/src/a\b.rs` | correspondence | test module |",
            rendered,
        )

    def test_scan_rejects_line_breaking_module_paths(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source_root = root / "crates/flpdf/src"
            source_root.mkdir(parents=True)

            for line_break, escaped_name in (("\n", "\\n"), ("\r", "\\r")):
                with self.subTest(line_break=escaped_name):
                    module_path = source_root / f"line{line_break}break.rs"
                    module_path.write_text(
                        "//! qpdf correspondence: test module.\n",
                        encoding="utf-8",
                    )

                    with self.assertRaisesRegex(
                        ValueError,
                        rf"{re.escape(escaped_name)}.*line breaks are not allowed in module paths",
                    ) as error:
                        self.module.scan_modules(source_root, root)

                    self.assertNotIn("\n", str(error.exception))
                    self.assertNotIn("\r", str(error.exception))
                    module_path.unlink()

    def test_scan_rejects_non_utf8_module_paths(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source_root = root / "crates/flpdf/src"
            source_root.mkdir(parents=True)
            (source_root / "lib.rs").write_text(
                "//! qpdf correspondence: crate root.\n",
                encoding="utf-8",
            )
            # `pathlib` surrogate-escapes undecodable filename bytes, which
            # would otherwise reach `render_index(...).encode("utf-8")` and
            # abort with an uncaught UnicodeEncodeError.
            undecodable_name = b"bad\xff.rs".decode("utf-8", "surrogateescape")
            try:
                (source_root / undecodable_name).write_text(
                    "//! qpdf correspondence: undecodable name.\n",
                    encoding="utf-8",
                )
            except (OSError, UnicodeEncodeError) as error:
                self.skipTest(f"undecodable filenames unavailable: {error}")

            with self.assertRaisesRegex(
                ValueError, "module paths must be valid UTF-8"
            ):
                self.module.scan_modules(source_root, root)

    def test_scan_order_and_markdown_escaping_are_deterministic(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source_root = root / "crates/flpdf/src"
            (source_root / "nested").mkdir(parents=True)
            (source_root / "z.rs").write_text(
                "//! qpdf correspondence: reason with `code` and | pipe.\n",
                encoding="utf-8",
            )
            (source_root / "nested/a.rs").write_text(
                "//! Mirrors qpdf 11.9.0 libqpdf/QPDF.cc.\n",
                encoding="utf-8",
            )

            entries = self.module.scan_modules(source_root, root)
            self.assertEqual(
                [
                    Path("crates/flpdf/src/nested/a.rs"),
                    Path("crates/flpdf/src/z.rs"),
                ],
                [path for path, _classification in entries],
            )

            rendered = self.module.render_index(entries)
            self.assertLess(rendered.index("nested/a.rs"), rendered.index("z.rs"))
            self.assertIn(r"reason with \`code\` and \| pipe", rendered)
            self.assertIn(
                r"| correspondence | reason with \`code\` and \| pipe |",
                rendered,
            )
            self.assertTrue(rendered.endswith("\n"))

    def test_scan_preserves_bare_cr_inside_line_comment(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source_root = root / "crates/flpdf/src"
            source_root.mkdir(parents=True)
            (source_root / "lib.rs").write_bytes(
                b"// ordinary\r//! qpdf correspondence: fake.\n"
                b"pub struct X;\n"
            )

            with self.assertRaisesRegex(ValueError, "missing"):
                self.module.scan_modules(source_root, root)

    def test_check_rejects_stale_generated_index(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source_root = root / "crates/flpdf/src"
            source_root.mkdir(parents=True)
            (source_root / "lib.rs").write_text(
                "//! qpdf correspondence: crate root.\n",
                encoding="utf-8",
            )
            index = root / "docs/qpdf-module-doc-index.md"
            common = [
                sys.executable,
                str(SCRIPT_PATH),
                "--root",
                str(root),
                "--source-root",
                "crates/flpdf/src",
                "--index",
                "docs/qpdf-module-doc-index.md",
            ]

            subprocess.run([*common, "--write"], check=True, capture_output=True, text=True)
            index.write_text("stale\n", encoding="utf-8")
            checked = subprocess.run(
                [*common, "--check"],
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertNotEqual(0, checked.returncode)
            self.assertIn("stale", checked.stderr)
            self.assertIn("--write", checked.stderr)

    def test_check_rejects_crlf_generated_index_as_byte_drift(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source_root = root / "crates/flpdf/src"
            source_root.mkdir(parents=True)
            (source_root / "lib.rs").write_text(
                "//! qpdf correspondence: crate root.\n",
                encoding="utf-8",
            )
            index = root / "docs/qpdf-module-doc-index.md"
            common = [
                sys.executable,
                str(SCRIPT_PATH),
                "--root",
                str(root),
                "--source-root",
                "crates/flpdf/src",
                "--index",
                "docs/qpdf-module-doc-index.md",
            ]

            subprocess.run([*common, "--write"], check=True, capture_output=True, text=True)
            index.write_bytes(index.read_bytes().replace(b"\n", b"\r\n"))
            checked = subprocess.run(
                [*common, "--check"],
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertNotEqual(0, checked.returncode)
            self.assertIn("stale", checked.stderr)

    def test_write_rejects_relative_index_escape(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            root = base / "repo"
            source_root = root / "crates/flpdf/src"
            source_root.mkdir(parents=True)
            (source_root / "lib.rs").write_text(
                "//! qpdf correspondence: crate root.\n",
                encoding="utf-8",
            )
            escaped_index = base / "escaped.md"

            written = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT_PATH),
                    "--root",
                    str(root),
                    "--index",
                    "../escaped.md",
                    "--write",
                ],
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertNotEqual(0, written.returncode)
            self.assertIn("outside --root", written.stderr)
            self.assertFalse(escaped_index.exists())

    def test_write_rejects_symlinked_index(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source_root = root / "crates/flpdf/src"
            source_root.mkdir(parents=True)
            (source_root / "lib.rs").write_text(
                "//! qpdf correspondence: crate root.\n",
                encoding="utf-8",
            )
            readme = root / "README.md"
            readme.write_text("project readme\n", encoding="utf-8")
            index = root / "docs/qpdf-module-doc-index.md"
            index.parent.mkdir()
            try:
                index.symlink_to(Path("../README.md"))
            except OSError as error:
                self.skipTest(f"symlink creation unavailable: {error}")

            written = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT_PATH),
                    "--root",
                    str(root),
                    "--write",
                ],
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertNotEqual(0, written.returncode)
            self.assertIn("symlink", written.stderr)
            self.assertEqual("project readme\n", readme.read_text(encoding="utf-8"))

    def test_write_rejects_symlinked_index_parent(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source_root = root / "crates/flpdf/src"
            source_root.mkdir(parents=True)
            (source_root / "lib.rs").write_text(
                "//! qpdf correspondence: crate root.\n",
                encoding="utf-8",
            )
            real_docs = root / "real-docs"
            real_docs.mkdir()
            protected = real_docs / "target.md"
            protected.write_text("protected\n", encoding="utf-8")
            try:
                (root / "docs").symlink_to(real_docs, target_is_directory=True)
            except OSError as error:
                self.skipTest(f"symlink creation unavailable: {error}")

            written = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT_PATH),
                    "--root",
                    str(root),
                    "--index",
                    "docs/target.md",
                    "--write",
                ],
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertNotEqual(0, written.returncode)
            self.assertIn("symlink", written.stderr)
            self.assertEqual("protected\n", protected.read_text(encoding="utf-8"))

    def test_write_rejects_absolute_source_root_escape(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            root = base / "repo"
            external_source = base / "external"
            external_source.mkdir(parents=True)
            (external_source / "lib.rs").write_text(
                "//! qpdf correspondence: external.\n",
                encoding="utf-8",
            )

            written = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT_PATH),
                    "--root",
                    str(root),
                    "--source-root",
                    str(external_source),
                    "--write",
                ],
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertNotEqual(0, written.returncode)
            self.assertIn("outside --root", written.stderr)

    def test_write_rejects_source_root_symlink_escape(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            root = base / "repo"
            external_source = base / "external"
            external_source.mkdir(parents=True)
            (external_source / "lib.rs").write_text(
                "//! qpdf correspondence: external.\n",
                encoding="utf-8",
            )
            source_parent = root / "crates/flpdf"
            source_parent.mkdir(parents=True)
            try:
                (source_parent / "src").symlink_to(external_source, target_is_directory=True)
            except OSError as error:
                self.skipTest(f"symlink creation unavailable: {error}")

            written = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT_PATH),
                    "--root",
                    str(root),
                    "--write",
                ],
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertNotEqual(0, written.returncode)
            self.assertIn("outside --root", written.stderr)

    def test_write_rejects_source_file_symlink_escape(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            root = base / "repo"
            source_root = root / "crates/flpdf/src"
            source_root.mkdir(parents=True)
            (source_root / "lib.rs").write_text(
                "//! qpdf correspondence: crate root.\n",
                encoding="utf-8",
            )
            external_source = base / "external.rs"
            external_source.write_text(
                "//! qpdf correspondence: external.\n",
                encoding="utf-8",
            )
            try:
                (source_root / "linked.rs").symlink_to(external_source)
            except OSError as error:
                self.skipTest(f"symlink creation unavailable: {error}")

            written = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT_PATH),
                    "--root",
                    str(root),
                    "--write",
                ],
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertNotEqual(0, written.returncode)
            self.assertIn("outside --root", written.stderr)

    def test_scan_rejects_symlinked_module_directory(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source_root = root / "crates/flpdf/src"
            source_root.mkdir(parents=True)
            (source_root / "lib.rs").write_text(
                "//! qpdf correspondence: crate root.\n",
                encoding="utf-8",
            )
            linked_target = root / "shared"
            linked_target.mkdir()
            (linked_target / "mod.rs").write_text(
                "pub struct Unclassified;\n",
                encoding="utf-8",
            )
            try:
                (source_root / "shared").symlink_to(
                    linked_target, target_is_directory=True
                )
            except OSError as error:
                self.skipTest(f"symlink creation unavailable: {error}")

            with self.assertRaisesRegex(ValueError, "symlinked directory"):
                self.module.scan_modules(source_root, root)

    def test_scan_skips_directories_named_like_modules(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source_root = root / "crates/flpdf/src"
            source_root.mkdir(parents=True)
            (source_root / "lib.rs").write_text(
                "//! qpdf correspondence: crate root.\n",
                encoding="utf-8",
            )
            # rustc accepts `#[path = "assets.rs/inner.rs"]`, so a directory
            # whose name ends in `.rs` is a legal part of a source tree.
            module_like_directory = source_root / "assets.rs"
            module_like_directory.mkdir()
            (module_like_directory / "inner.rs").write_text(
                "//! qpdf correspondence: embedded asset table.\n",
                encoding="utf-8",
            )

            entries = self.module.scan_modules(source_root, root)

            self.assertEqual(
                [
                    Path("crates/flpdf/src/assets.rs/inner.rs"),
                    Path("crates/flpdf/src/lib.rs"),
                ],
                [source_path for source_path, _ in entries],
            )

    def test_write_rejects_empty_source_tree(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "crates/flpdf/src").mkdir(parents=True)

            written = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT_PATH),
                    "--root",
                    str(root),
                    "--write",
                ],
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertNotEqual(0, written.returncode)
            self.assertIn("no Rust modules", written.stderr)


class RepositoryPolicyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.module = load_checker()

    def test_only_d1_d2_audited_module_is_declared_as_mirror(self):
        repo_root = SCRIPT_PATH.parent.parent
        entries = self.module.scan_modules(repo_root / "crates/flpdf/src", repo_root)
        mirrors = {
            path.as_posix()
            for path, classification in entries
            if classification.kind == "mirror"
        }

        self.assertEqual(
            {
                "crates/flpdf/src/content_normalizer.rs",
                "crates/flpdf/src/matrix.rs",
                "crates/flpdf/src/pdf_version.rs",
                "crates/flpdf/src/encryption/rc4.rs",
                "crates/flpdf/src/tokenizer.rs",
            },
            mirrors,
        )

    def test_generated_index_is_pinned_to_lf(self):
        repo_root = SCRIPT_PATH.parent.parent
        checked = subprocess.run(
            [
                "git",
                "check-attr",
                "eol",
                "--",
                "docs/qpdf-module-doc-index.md",
            ],
            cwd=repo_root,
            check=True,
            capture_output=True,
            text=True,
        )

        self.assertEqual(
            "docs/qpdf-module-doc-index.md: eol: lf",
            checked.stdout.strip(),
        )


if __name__ == "__main__":
    unittest.main()
