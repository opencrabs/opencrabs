#!/usr/bin/env python3
"""Fail when the crate that `cargo publish` would upload is too large.

crates.io answers anything over 10 MiB (10,485,760 bytes) with `413 Payload
Too Large`, and it does so at the very end of a release: after every platform
has built, after the tag exists. v0.6.0 died exactly there, and because the
release job waited on the publish, five successful builds were discarded with
it. This runs on every push instead, so the ceiling is a red check on the
commit that crossed it rather than a dead release.

The file list comes from `cargo package --list`, which is what cargo itself
would upload. Without cargo on PATH (a bare CI container, a sandbox) it falls
back to `git ls-files` filtered through the manifest's `exclude` globs, which
tracks the same list closely enough to be worth failing on.

Both paths measure a gzipped tar of the same bytes, so the number here is
comparable to the published `.crate`, not to an unpacked directory.
"""

from __future__ import annotations

import argparse
import fnmatch
import io
import os
import re
import shutil
import subprocess
import sys
import tarfile

LIMIT = 10 * 1024 * 1024  # 10,485,760 bytes, the hard crates.io ceiling
# Leave room to grow inside a release cycle: an asset added the day before a
# tag should not be the thing that rediscovers the limit.
DEFAULT_BUDGET = 10 * 1000 * 1000  # 10,000,000 bytes

# `cargo package --list` also names files cargo writes into the archive at
# package time, so they ship but do not exist in the working tree:
# `.cargo_vcs_info.json` carries the commit hash. They cannot be stat-ed, so
# each is charged a fixed allowance instead of being dropped, which would
# under-measure the crate.
GENERATED_ALLOWANCE = 512


def manifest_excludes(manifest: str) -> list[str]:
    """Read `package.exclude` from Cargo.toml without a TOML dependency."""
    with open(manifest, encoding="utf-8") as handle:
        text = handle.read()
    match = re.search(r"^exclude\s*=\s*\[(.*?)^\]", text, re.S | re.M)
    if not match:
        return []
    body = match.group(1)
    return [m for m in re.findall(r'"([^"]+)"', body) if m.strip()]


def _matches(path: str, pattern: str) -> bool:
    """gitignore-flavoured match for a Cargo `exclude` entry.

    Cargo documents these as gitignore patterns, so a bare name excludes that
    path and everything under it, `*` stays inside one segment and `**`
    crosses segments.
    """
    pattern = pattern.rstrip("/")
    if not pattern:
        return False
    if path == pattern or path.startswith(pattern + "/"):
        return True
    if fnmatch.fnmatchcase(path, pattern):
        return True
    # `src/tests/**` must cover the directory itself as well as its children.
    if pattern.endswith("/**") and path.startswith(pattern[:-3] + "/"):
        return True
    # A globbed directory (`src/assets/onboard*.png`) only names files.
    return False


def files_from_git(root: str, excludes: list[str]) -> list[str]:
    out = subprocess.run(
        ["git", "-C", root, "ls-files", "-z"],
        capture_output=True,
        check=True,
    ).stdout.decode()
    tracked = [p for p in out.split("\0") if p]
    return [
        p
        for p in tracked
        if os.path.isfile(os.path.join(root, p))
        and not any(_matches(p, e) for e in excludes)
    ]


def files_from_cargo(root: str) -> list[str] | None:
    cargo = shutil.which("cargo")
    if not cargo:
        return None
    try:
        out = subprocess.run(
            [cargo, "package", "--list", "--allow-dirty", "--quiet"],
            cwd=root,
            capture_output=True,
            check=True,
            timeout=300,
        ).stdout.decode()
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as err:
        stderr = getattr(err, "stderr", b"") or b""
        print(
            f"warning: `cargo package --list` failed ({stderr.decode().strip()[:200]}); "
            "falling back to git ls-files",
            file=sys.stderr,
        )
        return None
    return [line for line in out.splitlines() if line.strip()]


def packaged_size(root: str, files: list[str]) -> int:
    """Gzipped tar of exactly these files, the way a `.crate` is built.

    Entries cargo generates at package time have no file on disk, so they are
    counted at GENERATED_ALLOWANCE rather than read.
    """
    buffer = io.BytesIO()
    generated = 0
    with tarfile.open(fileobj=buffer, mode="w:gz", compresslevel=9) as archive:
        for rel in sorted(files):
            path = os.path.join(root, rel)
            if not os.path.isfile(path):
                generated += GENERATED_ALLOWANCE
                continue
            archive.add(path, arcname=rel, recursive=False)
    return len(buffer.getvalue()) + generated


def self_test(root: str) -> int:
    """Prove the checker can still find things, and still exclude things.

    A size check that quietly measures an empty list passes every push, which
    is worse than having no check. These assertions fail loudly if the exclude
    matcher stops matching or `git ls-files` comes back empty.
    """
    failures: list[str] = []

    cases = [
        ("src/tests/foo_test.rs", "src/tests/**", True),
        ("src/tests/fixtures/legacy_word_floating_text.doc", "src/tests/**", True),
        ("src/gui/main.rs", "src/tests/**", False),
        ("src/assets/onboard3.png", "src/assets/onboard*.png", True),
        ("src/assets/split-panes.png", "src/assets/onboard*.png", False),
        ("src/assets/split-panes.png", "src/assets/split-panes.png", True),
        ("scripts/check-crate-size.py", "scripts/**", True),
        ("src/scripts_like_name.rs", "scripts/**", False),
        ("CHANGELOG.md", "CHANGELOG.md", True),
    ]
    for path, pattern, expected in cases:
        got = _matches(path, pattern)
        if got != expected:
            failures.append(f"_matches({path!r}, {pattern!r}) = {got}, want {expected}")

    excludes = manifest_excludes(os.path.join(root, "Cargo.toml"))
    if not excludes:
        failures.append("Cargo.toml declares no package.exclude, so the crate is unbounded")

    files = files_from_git(root, excludes)
    if len(files) < 500:
        failures.append(f"only {len(files)} files found; the listing is broken")

    for must_keep in (
        "Cargo.toml",
        "Cargo.lock",
        "src/lib.rs",
        "src/main.rs",
        "src/assets/icon.icns",
        "src/assets/icon-packs/topics/crab_api.png",
        "src/assets/sound/notification.wav",
        "src/brain/tools/doc_gen/fonts/DejaVuSans.ttf",
    ):
        if must_keep not in files:
            failures.append(f"{must_keep} is needed at build time but is not packaged")

    for must_drop in (
        "CHANGELOG.md",
        "src/tests/icon_assets_test.rs",
        "src/assets/onboard1.png",
    ):
        if must_drop in files:
            failures.append(f"{must_drop} is packaged and should not be")

    # v0.6.0's first Lint run died here: `cargo package --list` reports
    # `.cargo_vcs_info.json`, which cargo writes during packaging, and
    # measuring it raised FileNotFoundError. A generated entry has to be
    # counted, not crash the check.
    plain = packaged_size(root, ["Cargo.toml"])
    generated = packaged_size(root, ["Cargo.toml", ".cargo_vcs_info.json"])
    if generated <= plain:
        failures.append("a cargo-generated archive entry was not counted")

    for failure in failures:
        print(f"FAIL: {failure}", file=sys.stderr)
    if failures:
        return 1
    print(
        f"OK: {len(excludes)} exclude rules, {len(files)} packaged files, "
        "build-critical paths present and documentation paths dropped."
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=".", help="repository root")
    parser.add_argument(
        "--budget",
        type=int,
        default=DEFAULT_BUDGET,
        help=f"fail above this many bytes (default {DEFAULT_BUDGET:,})",
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="print the packaged file list and exit",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="check that the exclude matcher and the file listing still work",
    )
    args = parser.parse_args()

    root = os.path.abspath(args.root)
    if args.self_test:
        return self_test(root)
    files = files_from_cargo(root)
    source = "cargo package --list"
    if files is None:
        files = files_from_git(root, manifest_excludes(os.path.join(root, "Cargo.toml")))
        source = "git ls-files minus Cargo.toml exclude"

    if args.list:
        print("\n".join(sorted(files)))
        return 0

    size = packaged_size(root, files)
    biggest = sorted(
        (
            (os.path.getsize(os.path.join(root, f)), f)
            for f in files
            if os.path.isfile(os.path.join(root, f))
        ),
        reverse=True,
    )[:10]

    print(f"file list   : {source} ({len(files)} files)")
    print(f"crate size  : {size:,} bytes ({size / 1024 / 1024:.2f} MiB)")
    print(f"crates.io   : {LIMIT:,} bytes ({LIMIT / 1024 / 1024:.2f} MiB) hard limit")
    print(f"budget      : {args.budget:,} bytes, {args.budget - size:+,} remaining")
    print("\nlargest entries:")
    for entry_size, name in biggest:
        print(f"  {entry_size:>10,}  {name}")

    if size > LIMIT:
        print(
            f"\nFAIL: {size - LIMIT:,} bytes over the crates.io ceiling. "
            "`cargo publish` would answer 413. Exclude or shrink before tagging.",
            file=sys.stderr,
        )
        return 1
    if size > args.budget:
        print(
            f"\nFAIL: within the hard limit but over the {args.budget:,} byte budget, "
            "so there is no room left for the rest of the cycle.",
            file=sys.stderr,
        )
        return 1
    print("\nOK: the packaged crate fits.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
