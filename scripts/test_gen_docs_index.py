#!/usr/bin/env python3
"""Offline fixtures for the live Seeker docs index generator."""
from contextlib import redirect_stderr, redirect_stdout
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).with_name("gen-docs-index.py")
SPEC = importlib.util.spec_from_file_location("gen_docs_index", SCRIPT)
gen = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gen)

SOURCE_TEMPLATE = """pub const PAGES: &[DocPage] = &[
];

/// Every path https://docs.opencrabs.com/sitemap.xml publishes. The
/// coverage test reads this, so a page that ships later fails the suite
/// instead of silently missing from the card.
pub const SITEMAP: &[&str] = &[
];

fn keep_this_helper() {}
"""


def fake_title(path: str) -> str:
    return path.rsplit("/", 1)[-1].removesuffix(".html").replace("-", " ").title()


def source_for(pages: list[gen.Page], paths: list[str]) -> str:
    return gen.render_index(SOURCE_TEMPLATE, pages, paths)


class DocsIndexTests(unittest.TestCase):
    def run_check(self, source: str, live_paths: list[str]):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "docs.rs"
            target.write_text(source)
            errors = io.StringIO()
            fixture_keywords = {
                path: gen.KEYWORDS[path] for path in live_paths if path in gen.KEYWORDS
            }
            with (
                mock.patch.object(gen, "TARGET", target),
                mock.patch.object(gen, "KEYWORDS", fixture_keywords),
                mock.patch.object(gen, "sitemap_paths", return_value=live_paths),
                mock.patch.object(gen, "page_title", side_effect=fake_title),
                redirect_stderr(errors),
                redirect_stdout(io.StringIO()),
            ):
                code = gen.main(["--check"])
            self.assertEqual(target.read_text(), source, "--check must never write")
            return code, errors.getvalue()

    def test_current_fixture_passes_without_network(self):
        paths = ["/introduction.html", "/toc.html"]
        pages = gen.expected_pages(paths, fake_title)
        code, errors = self.run_check(source_for(pages, paths), paths)
        self.assertEqual(code, 0, errors)

    def test_unclassified_page_is_rejected_without_writing(self):
        original_paths = ["/introduction.html"]
        source = source_for(gen.expected_pages(original_paths, fake_title), original_paths)
        live_paths = original_paths + ["/features/brand-new.html"]
        code, errors = self.run_check(source, live_paths)
        self.assertEqual(code, 1)
        self.assertIn("unclassified URLs", errors)
        self.assertIn("/features/brand-new.html", errors)
        self.assertIn("missing pages", errors)

    def test_stale_keywords_and_removed_pages_are_reported_together(self):
        original_paths = ["/introduction.html", "/old.html"]
        pages = [
            gen.Page("Introduction", "/introduction.html", "stale words"),
            gen.Page("Old", "/old.html", "old"),
        ]
        live_paths = ["/introduction.html"]
        code, errors = self.run_check(source_for(pages, original_paths), live_paths)
        self.assertEqual(code, 1)
        self.assertIn("stale keywords", errors)
        self.assertIn("removed pages still indexed", errors)
        self.assertIn("sitemap drift", errors)

    def test_exclusions_match_the_rust_coverage_contract(self):
        for path, reason in gen.EXCLUDED_PATHS.items():
            self.assertEqual(gen.exclusion_reason(path), reason)
        self.assertEqual(gen.exclusion_reason("/stories/a-case.html"), "case study")
        for path in [
            "/introduction.html",
            "/user-stories.html",
            "/features/themes.html",
            "/getting-started/videos.html",
        ]:
            expected = path == "/getting-started/videos.html"
            self.assertEqual(gen.exclusion_reason(path) is not None, expected)

    def test_manually_enriched_keywords_cannot_be_erased(self):
        self.assertIn(
            "daemon trouble stuck down restart service not starting",
            gen.KEYWORDS["/getting-started/troubleshooting.html"],
        )
        self.assertIn(
            "automation automate every morning daily reminder routine",
            gen.KEYWORDS["/features/cron-jobs.html"],
        )
        for word in ["sidebar", "open", "close", "hide", "reveal"]:
            self.assertIn(word, gen.KEYWORDS["/features/sessions.html"].split())
        for word in ["theme", "themes", "appearance", "colour", "color", "customization"]:
            self.assertIn(word, gen.KEYWORDS["/features/themes.html"].split())

    def test_every_reference_path_has_exactly_one_metadata_entry(self):
        self.assertEqual(len(gen.KEYWORDS), len(set(gen.KEYWORDS)))
        for path, keywords in gen.KEYWORDS.items():
            self.assertIsNone(gen.exclusion_reason(path), path)
            self.assertTrue(path.startswith("/") and path.endswith(".html"), path)
            self.assertTrue(keywords.strip(), path)


if __name__ == "__main__":
    unittest.main()
