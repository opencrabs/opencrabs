#!/usr/bin/env python3
r"""Fail CI on a NEW clippy::string_slice site, and only on a new one.

Slicing a str at a byte index that is not a char boundary panics. That is
the #2220 shape (rfind returns a byte offset, `i + 1` walks one byte into a
character up to four bytes wide) and it shipped once already. clippy has a
lint for it, and this repo has 900+ live slice sites, so turning the lint
into a hard gate would mean 900 hand-written allow attributes, every one of
them saying "safe by construction" about code that already is. That is noise
that buries the one line worth seeing: a new site.

So the lint is ratcheted instead of enabled. Existing sites are recorded in
byte-slice-baseline.txt and cost nothing; a site that is not in the baseline
fails the job with its file and line. Fix the index with the house idiom
(ceil_char_boundary / floor_char_boundary), or, when it provably cannot land
mid-character, bump the baseline in the same PR so a reviewer sees it.

Why the lint is not in Cargo.toml
  `[lints.clippy]` cannot give the 900 existing sites a pass, and a crate
  level allow kills the lint outright (tested: `-A` emits nothing at all,
  so there is no list to ratchet). The lint is therefore off by default, and
  this script switches it on for its own clippy run with `--force-warn`.

Why --force-warn and not -W
  `-D warnings -W clippy::string_slice` still reports an ERROR: the group
  deny wins over a later single-lint warn (measured). `--force-warn
  clippy::string_slice` keeps it a warning under `-D warnings`, so one
  clippy run enforces the normal gate (any other diagnostic is an error and
  fails) and still hands this script the site list. One compile, not two.

Vacuity
  On a restored cargo cache every unit is reported fresh, cargo emits no
  diagnostics at all, and "zero sites" would read as a clean tree. A ratchet
  that passes without checking is worse than no ratchet, so this script
  fails when the run produced no new compilation of a local crate and no
  slice diagnostic either. The CI step touches src/lib.rs first so our crate
  always recompiles; dependencies stay cached.

Baseline format (tab separated, one line per file plus expression):
  <count>\t<path>\t<expression>
Regenerate with `--print-baseline` and redirect over the file. A count that
goes UP, or a new expression, is a finding. A count that goes down is fine
and is reported so the baseline can be tightened.

Exit codes: 0 clean, 1 findings, 2 bad usage or bad baseline.
"""
from __future__ import annotations

import argparse
import collections
import contextlib
import io
import json
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
DEFAULT_BASELINE = HERE / "byte-slice-baseline.txt"
LINT = "clippy::string_slice"
WARN_SUMMARY = re.compile(r"^\d+ warnings? emitted$")

# The single clippy invocation. Both halves matter: -D warnings is the gate
# every other lint already lives under, --force-warn is what lets this lint
# exist as a warning next to it (see the module docstring).
CLIPPY_ARGS = [
    "cargo",
    "clippy",
    "--locked",
    "--lib",
    "--bins",
    "--tests",
    "--examples",
    "--all-features",
    "--message-format=json",
    "--",
    "-D",
    "warnings",
    "--force-warn",
    f"{LINT}",
]


class Site(collections.namedtuple("Site", "path line col expr")):
    __slots__ = ()


def extract_expr(line: str, hs: int, he: int) -> str:
    """The highlighted expression of a rustc span, from its 1-based columns.

    rustc reports columns 1-based with an exclusive end, and highlights the
    indexing expression itself (`s[i + 1]`), not the `&` in front of it.
    """
    if not line or hs < 1 or he < 1:
        return ""
    start = min(hs - 1, len(line))
    end = min(max(he - 1, start), len(line))
    return line[start:end].strip()


def analyze(text: str) -> tuple[list[Site], list[str], int, int]:
    """Parse a cargo --message-format=json stream.

    Returns (sites, problems, fresh_local_units, skipped_lines). `problems`
    are diagnostics at error level (a real error, or a normal lint promoted
    by -D warnings) plus any unexpected warning that is not our lint: both
    are the pre-existing gate and must keep failing the job.
    """
    sites: dict[tuple, Site] = {}
    problems: list[str] = []
    fresh = 0
    skipped = 0
    for raw in text.splitlines():
        raw = raw.strip()
        if not raw:
            continue
        if not raw.startswith("{"):
            skipped += 1
            continue
        try:
            rec = json.loads(raw)
        except json.JSONDecodeError:
            skipped += 1
            continue
        reason = rec.get("reason")
        if reason == "compiler-artifact":
            pkg = rec.get("package_id") or ""
            if rec.get("fresh") is False and not pkg.startswith("registry+"):
                fresh += 1
            continue
        if reason != "compiler-message":
            continue
        msg = rec.get("message") or {}
        level = msg.get("level")
        code = (msg.get("code") or {}).get("code")
        rendered = (msg.get("rendered") or "").strip()
        if level == "error":
            problems.append(rendered or msg.get("message", "error"))
            continue
        if level == "warning" and code != LINT:
            text_msg = msg.get("message", "")
            if WARN_SUMMARY.match(text_msg):
                continue
            problems.append(rendered or text_msg)
            continue
        if code != LINT:
            continue
        for span in msg.get("spans") or []:
            if not span.get("is_primary"):
                continue
            path = span.get("file_name") or ""
            if not path:
                continue
            key = (path, span.get("line_start"), span.get("column_start"))
            if key in sites:
                continue
            texts = span.get("text") or []
            expr = ""
            if texts:
                expr = extract_expr(
                    texts[0].get("text", ""),
                    texts[0].get("highlight_start", 0),
                    texts[0].get("highlight_end", 0),
                )
            sites[key] = Site(path, span.get("line_start"), span.get("column_start"), expr)
    return list(sites.values()), problems, fresh, skipped


def read_baseline(path: Path) -> collections.Counter:
    counter: collections.Counter = collections.Counter()
    for lineno, raw in enumerate(path.read_text().splitlines(), 1):
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) != 3:
            raise ValueError(
                f"{path}:{lineno}: expected 3 tab-separated fields (count, path, expression)"
            )
        count, rel, expr = parts
        counter[(rel, expr)] += int(count)
    return counter


def render_baseline(sites: list[Site]) -> str:
    counter = collections.Counter((s.path, s.expr) for s in sites)
    out = [
        "# byte-slice ratchet baseline, regenerated by:",
        "#   python3 scripts/check-byte-slice-ratchet.py --print-baseline > scripts/byte-slice-baseline.txt",
        "# count<TAB>path<TAB>expression, sorted. A count that goes up, or a",
        "# new expression, fails the gate: look at the index before bumping it.",
    ]
    for (path, expr), count in sorted(counter.items()):
        out.append(f"{count}\t{path}\t{expr}")
    return "\n".join(out) + "\n"


def report(text: str, baseline: collections.Counter | None) -> int:
    sites, problems, fresh, skipped = analyze(text)
    if problems:
        for problem in problems:
            print(problem)
        print(
            f"::error::{len(problems)} clippy diagnostic(s) at error level. "
            "That is the -D warnings gate, not the ratchet; fix them first."
        )
        return 1
    if not sites and fresh == 0:
        print(
            "::error::clippy emitted no string_slice diagnostic and compiled no local "
            "crate, so nothing was checked. The cargo cache was probably reused; touch "
            "src/lib.rs (the CI step does) and run again."
        )
        return 1
    current = collections.Counter((s.path, s.expr) for s in sites)
    where: dict[tuple, list[str]] = collections.defaultdict(list)
    for site in sites:
        where[(site.path, site.expr)].append(f"{site.path}:{site.line}")
    new = current - baseline if baseline is not None else current
    gone = baseline - current if baseline is not None else collections.Counter()
    checked = f"{len(sites)} site(s) in {len({s.path for s in sites})} file(s)"
    if skipped:
        print(f"byte-slice ratchet: ignored {skipped} non-JSON line(s) of cargo output")
    if new:
        print(f"::error::byte-slice ratchet: {sum(new.values())} new {LINT} site(s)")
        print("")
        print("Slicing a str at a byte index that is not a char boundary panics. Fix the")
        print("index (ceil_char_boundary / floor_char_boundary), or, if it provably cannot")
        print("land mid-character, raise its count in scripts/byte-slice-baseline.txt in")
        print("this same change so a reviewer sees it.")
        print("")
        for (path, expr), count in sorted(new.items()):
            base = baseline.get((path, expr), 0) if baseline is not None else 0
            for location in where[(path, expr)]:
                print(f"  {location}  {expr}  (baseline {base}, now {count})")
        print("")
        print(f"byte-slice ratchet: {checked}, {sum(new.values())} new")
        return 1
    if gone:
        print(
            f"byte-slice ratchet: {sum(gone.values())} baseline site(s) no longer exist. "
            "Tighten with --print-baseline when convenient."
        )
    print(f"byte-slice ratchet: OK, {checked}, no new site")
    return 0


def run_clippy() -> str:
    proc = subprocess.run(
        CLIPPY_ARGS,
        cwd=REPO,
        stdout=subprocess.PIPE,
        text=True,
        errors="replace",
        check=False,
    )
    if proc.returncode != 0 and not proc.stdout.strip():
        print(f"::error::cargo clippy exited {proc.returncode} with no JSON on stdout")
    return proc.stdout


_FIXTURE_ARTIFACT = json.dumps(
    {"reason": "compiler-artifact", "package_id": "path+file:///repo#opencrabs", "fresh": False}
)
_FIXTURE_REGISTRY_FRESH = json.dumps(
    {"reason": "compiler-artifact", "package_id": "registry+https://x#serde", "fresh": False}
)


def _slice(path: str, line: int, col: int, text: str, hs: int, he: int) -> str:
    return json.dumps(
        {
            "reason": "compiler-message",
            "message": {
                "level": "warning",
                "code": {"code": LINT},
                "message": "indexing into a string may panic",
                "spans": [
                    {
                        "file_name": path,
                        "line_start": line,
                        "column_start": col,
                        "is_primary": True,
                        "text": [{"text": text, "highlight_start": hs, "highlight_end": he}],
                    }
                ],
            },
        }
    )


def _span_cols(text: str, expr: str) -> tuple[int, int]:
    """The columns rustc reports for a highlighted span: 1-based, end exclusive."""
    start = text.index(expr)
    return start + 1, start + len(expr) + 1


def _slice_for(path: str, lineno: int, text: str, expr: str) -> str:
    """A slice diagnostic whose columns come from the line itself, so a fixture
    line and its offsets can never disagree."""
    hs, he = _span_cols(text, expr)
    return _slice(path, lineno, hs, text, hs, he)


def self_test() -> int:
    """Prove the ratchet still has teeth, offline, with no clippy run."""
    failures: list[str] = []
    tail = "    let tail = &s[idx + prefix.len()..];"
    tail_expr = "s[idx + prefix.len()..]"
    other = "  &x[..n]"
    other_expr = "x[..n]"
    known = _slice_for("src/one.rs", 10, tail, tail_expr)
    another = _slice_for("src/two.rs", 99, other, other_expr)
    healthy = "\n".join([_FIXTURE_ARTIFACT, _FIXTURE_REGISTRY_FRESH, known, another])

    def check(name: str, text: str, baseline: collections.Counter | None, want: int) -> None:
        # report() narrates to stdout by design, and some fixtures are failure
        # paths. The self-test reads the exit code, so swallow the narration:
        # otherwise a green CI step prints the fixture's own error lines and
        # reads like a failure to whoever opens the log.
        with contextlib.redirect_stdout(io.StringIO()):
            got = report(text, baseline)
        if got != want:
            failures.append(f"{name}: exit {got}, wanted {want}")

    baseline_ok = collections.Counter({("src/one.rs", tail_expr): 1, ("src/two.rs", other_expr): 1})
    check("baseline matches", healthy, baseline_ok, 0)

    # The whole point: a site that is not in the baseline fails, and the
    # expression is extracted from the span, not from the whole source line.
    partial = collections.Counter({("src/one.rs", tail_expr): 1})
    check("new expression in a known file", healthy, partial, 1)
    check("empty baseline", healthy, collections.Counter(), 1)
    # --no-baseline passes None through the same code path the option uses.
    check("no baseline at all", healthy, None, 1)

    # A bump inside a file already in the baseline is still new.
    two_same = "\n".join(
        [
            _FIXTURE_ARTIFACT,
            _slice_for("src/one.rs", 10, tail, tail_expr),
            _slice_for("src/one.rs", 44, tail, tail_expr),
        ]
    )
    check("count increase", two_same, partial, 1)

    # Shrinking is allowed (the baseline is just stale).
    check("baseline larger than the tree", healthy, baseline_ok + collections.Counter({("src/three.rs", "&gone[..1]"): 2}), 0)

    # Vacuity: a fresh cache emits no diagnostic and compiles nothing local.
    check("no diagnostics, nothing compiled", _FIXTURE_REGISTRY_FRESH, collections.Counter(), 1)
    check("no diagnostics, local compile happened", _FIXTURE_ARTIFACT, collections.Counter(), 0)

    # The ordinary -D warnings gate must keep failing the job.
    err = json.dumps(
        {
            "reason": "compiler-message",
            "message": {
                "level": "error",
                "code": {"code": "clippy::needless_return"},
                "message": "unneeded return",
                "rendered": "error: unneeded return",
                "spans": [],
            },
        }
    )
    check("error diagnostic still fails", "\n".join([_FIXTURE_ARTIFACT, err]), baseline_ok, 1)

    # The arg list is the contract with clippy: no --force-warn, no list.
    if "--force-warn" not in CLIPPY_ARGS or LINT not in CLIPPY_ARGS:
        failures.append("CLIPPY_ARGS lost --force-warn clippy::string_slice")
    if "-D" not in CLIPPY_ARGS or "warnings" not in CLIPPY_ARGS:
        failures.append("CLIPPY_ARGS lost -D warnings")

    # The span convention: rustc columns are 1-based and the end is exclusive.
    # Pinned arithmetically so no fixture whitespace can hide a regression, plus
    # a round trip through the same helper the fixtures above use. The real
    # convention was also checked against 1568 spans of a live clippy run (see
    # the commit body), which is not something a self-test can carry.
    hs, he = _span_cols(tail, tail_expr)
    for text, start, end, want in [
        ("abcdef", 1, 7, "abcdef"),
        ("abcdef", 3, 5, "cd"),
        (tail, hs, he, tail_expr),
    ]:
        got_expr = extract_expr(text, start, end)
        if got_expr != want:
            failures.append(f"extract_expr({text!r}, {start}, {end}) = {got_expr!r}, wanted {want!r}")

    for failure in failures:
        print(f"self-test FAIL: {failure}")
    if failures:
        return 1
    print("self-test OK: 9 cases and 5 extra assertions")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description="byte-slice ratchet (clippy::string_slice)")
    ap.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
    ap.add_argument("--no-baseline", action="store_true", help="report every site as new")
    ap.add_argument("--from", dest="from_log", type=Path, default=None,
                    help="read a cargo --message-format=json log instead of running clippy")
    ap.add_argument("--print-baseline", action="store_true")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    if args.self_test:
        return self_test()

    if args.from_log:
        text = args.from_log.read_text(errors="replace")
    else:
        if not Path(REPO / "Cargo.toml").exists():
            print(f"::error::no Cargo.toml at {REPO}; run this from the repository root")
            return 2
        text = run_clippy()

    if args.print_baseline:
        sites, problems, _, _ = analyze(text)
        if problems:
            for problem in problems:
                print(problem, file=sys.stderr)
            print("::error::clippy reported errors, so the baseline would be built from a red tree", file=sys.stderr)
            return 1
        sys.stdout.write(render_baseline(sites))
        return 0

    baseline: collections.Counter | None = None
    if not args.no_baseline:
        if not args.baseline.exists():
            print(f"::error::baseline {args.baseline} is missing. Generate it with --print-baseline")
            return 2
        try:
            baseline = read_baseline(args.baseline)
        except (ValueError, OSError) as exc:
            print(f"::error::{exc}")
            return 2
    return report(text, baseline)


if __name__ == "__main__":
    sys.exit(main())