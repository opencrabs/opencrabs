#!/usr/bin/env python3
"""policy-lint: machine-readable workflow policy for this repo.

Reads `governance.toml` at the repo root and enforces the four checks
issue #1934 fenced as machine-visible. Everything outside that fence
(tone, scope creep, "is this one problem?") stays in human review by
design; the schema's informational keys are deliberately not enforced
here so the gate can never grow teeth nobody approved.

Checks (PR context):
  1. workflow.child_issues_not_child_prs: if the PR body carries
     follow-up phrasing, it must also reference child issue numbers
     (#NNN). Deliberately narrow: false negatives acceptable, false
     positives not. FAIL.
  2. prs.max_files_warn: diff touching more than max_files_warn
     files. WARN only, never FAIL.
  3. commits.no_coauthor_trailers: any commit carrying a
     `Co-authored-by:` trailer. FAIL.
  4. commits.conventional_titles: commit subjects must match
     `type(scope)!: subject` / `type: subject`. Merge and Revert
     subjects are exempt: update-on-top leaves Merge commits inside
     the PR range (house merge flow), and undo keeps its history
     via revert-style subjects. FAIL.
  5. titles.issues_conventional / titles.prs_conventional: the issue
     or PR title must match the same shape. A bare area prefix
     (`TUI:`, `z.ai:`, `voice:`, `fix core(scope):`) is the
     documented ban and FAILs; a plain explanatory title with no
     colon prefix passes, because CONTRIBUTING.md allows one for
     anything that fits no type. FAIL.

Modes:
  python3 scripts/policy-lint.py                      # validate config (local run)
  python3 scripts/policy-lint.py --self-test          # fixtures in scripts/tests/policy-lint/
  python3 scripts/policy-lint.py --fixture NAME.json  # run one fixture, exit with its verdict
  python3 scripts/policy-lint.py --body-file B --subjects-file S --messages-file M --files-count N
  python3 scripts/policy-lint.py --title-file T --title-kind issue|pr

Stdlib + tomllib only, zero dependencies. Same rule as
check-windows-footguns.py. CI wires this in
.github/workflows/policy-lint.yml.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
from pathlib import Path

FOLLOW_UP_PHRASES = (
    "follow up",
    "follow-up",
    "followup",
    "next PR",
    "part 2",
    "rest in",
)
ISSUE_REF = re.compile(r"#\d+")
COAUTHOR = re.compile(r"(?im)^\s*co-authored-by\s*:")
# Types actually in use on main (receipt: git log --format=%s -40, 2026-10-09).
CONVENTIONAL = re.compile(
    r"^(feat|fix|refactor|docs|test|tests|chore|perf|style|build|ci|revert"
    r"|release|deps)(\([^)]*\))?!?: \S.*$"
)
EXEMPT_SUBJECTS = ("Merge ", "Revert ")

# Title shape, shared by issues, PRs and commit subjects. A title that carries
# a colon prefix must carry a real type in it: `TUI:`, `z.ai:`, `voice:` and
# `fix core(scope):` are the documented ban, not a scope.
TITLE_HEAD = re.compile(r"^(?P<head>[^:]{1,60}):\s")
VALID_TITLE_HEAD = re.compile(
    r"^(feat|fix|refactor|docs|test|tests|chore|perf|style|build|ci|revert"
    r"|release|deps)(\([^)]*\))?!?$"
)

EXPECTED_SECTIONS = {
    "workflow": {
        "sync_before_commit": bool,
        "child_issues_not_child_prs": bool,
        "one_pr_per_problem": bool,
        "single_sanity_ci": bool,
        "merge_at_completion": bool,
    },
    "prs": {
        "max_files_warn": int,
        "no_stub": bool,
    },
    "commits": {
        "no_coauthor_trailers": bool,
        "conventional_titles": bool,
    },
    "titles": {
        "issues_conventional": bool,
        "prs_conventional": bool,
    },
}


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def load_config(path: Path) -> dict:
    """Parse governance.toml and pin its schema. Exit 1 on any drift."""
    if not path.is_file():
        return fail_and_exit(f"config: {path} not found")
    try:
        cfg = tomllib.loads(path.read_text(encoding="utf-8"))
    except Exception as e:  # noqa: BLE001 - any parse error is a hard fail
        return fail_and_exit(f"config: {path} does not parse: {e}")
    problems = []
    for section, keys in EXPECTED_SECTIONS.items():
        block = cfg.get(section)
        if not isinstance(block, dict):
            problems.append(f"config: missing section [{section}]")
            continue
        for key, typ in keys.items():
            if key not in block:
                problems.append(f"config: missing [{section}].{key}")
            elif not isinstance(block[key], typ) or isinstance(block[key], bool) and typ is int:
                problems.append(f"config: [{section}].{key} has wrong type")
    if problems:
        for p in problems:
            print(f"FAIL: {p}")
        sys.exit(1)
    return cfg


def fail_and_exit(msg: str):
    print(f"FAIL: {msg}")
    sys.exit(1)


def check_title(title: str, kind: str, cfg: dict) -> list[str]:
    """The title fence. Returns failures for one issue or PR title.

    Three outcomes:
      - matches `type(scope)!: subject`            -> pass
      - carries a colon prefix with no real type   -> FAIL (bare area prefix)
      - carries no colon prefix at all             -> pass (plain explanatory
        title, which CONTRIBUTING.md allows for anything that fits no type)
    """
    key = "issues_conventional" if kind == "issue" else "prs_conventional"
    if not cfg.get("titles", {}).get(key, False):
        return []
    title = title.strip()
    if not title:
        return [f"titles.{key}: the {kind} title is empty"]
    if CONVENTIONAL.match(title):
        return []
    head = TITLE_HEAD.match(title)
    if head and not VALID_TITLE_HEAD.match(head.group("head").strip()):
        return [
            f"titles.{key}: bare area prefix {head.group('head')!r} in the "
            f"{kind} title. Use type(scope): subject, e.g. "
            f"'fix(tui): what is wrong'. A bare area prefix is not the "
            f"convention (CONTRIBUTING.md, Issue Titles)."
        ]
    return []

def run_checks(cfg: dict, body: str, subjects: list[str], messages: str, files_count: int):
    """The whole fence. Returns (failures, warnings)."""
    fails: list[str] = []
    warns: list[str] = []

    if cfg["workflow"]["child_issues_not_child_prs"]:
        lowered = body.lower()
        hit = [p for p in FOLLOW_UP_PHRASES if p.lower() in lowered]
        if hit and not ISSUE_REF.search(body):
            fails.append(
                "child_issues_not_child_prs: PR body announces follow-up work "
                f"({', '.join(repr(p) for p in hit)}) with zero issue refs. "
                "Follow-ups become CHILD ISSUES (#NNN), not sibling PRs."
            )

    cap = cfg["prs"]["max_files_warn"]
    if files_count > cap:
        warns.append(
            f"max_files_warn: diff touches {files_count} files (> {cap}). "
            "One PR = one change. Double-check nothing unrelated rode along."
        )

    if cfg["commits"]["no_coauthor_trailers"]:
        hits = COAUTHOR.findall(messages)
        if hits:
            fails.append(
                f"no_coauthor_trailers: {len(hits)} commit(s) carry a "
                "Co-authored-by trailer. Sole author, no exceptions."
            )

    if cfg["commits"]["conventional_titles"]:
        for subject in subjects:
            if not subject or subject.startswith(EXEMPT_SUBJECTS):
                continue
            if not CONVENTIONAL.match(subject):
                fails.append(
                    f"conventional_titles: subject does not match "
                    f"type(scope)!: subject  ->  {subject!r}"
                )

    return fails, warns


def verdict(fails: list[str], warns: list[str], label: str = "") -> int:
    tag = f"[{label}] " if label else ""
    for w in warns:
        print(f"WARN: {tag}{w}")
    if not fails:
        print(f"PASS: {tag}policy-lint found no drift")
        return 0
    for f in fails:
        print(f"FAIL: {tag}{f}")
    return 1


def fixture_files(tests_dir: Path) -> list[Path]:
    return sorted(tests_dir.glob("*.json"))


def run_fixture(path: Path, cfg: dict) -> int:
    data = json.loads(path.read_text(encoding="utf-8"))
    fails, warns = run_checks(
        cfg,
        data.get("body", ""),
        data.get("subjects", []),
        data.get("messages", ""),
        int(data.get("files_count", 0)),
    )
    if "title" in data:
        fails = fails + check_title(
            data["title"], data.get("title_kind", "issue"), cfg
        )
    return verdict(fails, warns, label=data.get("name", path.stem))


def main() -> int:
    """Run the linter, then honour --expect-exit if it was passed.

    --expect-exit exists because a negative case (a fixture whose verdict
    MUST be FAIL) can never satisfy a CI gate that only reads exit 0 as
    success. With it, `--fixture bad.json --expect-exit 1` exits 0 when the
    linter correctly rejected the fixture, and exits 1 when the linter
    wrongly passed it.
    """
    expected = _expected_exit_from_argv(sys.argv[1:])
    code = _run()
    if expected is None:
        return code
    if code == expected:
        print(f"EXPECT-EXIT PASS: exit {code}, as declared")
        return 0
    print(f"EXPECT-EXIT FAIL: exit {code}, expected {expected}")
    return 1

def _expected_exit_from_argv(argv: list[str]) -> int | None:
    for i, arg in enumerate(argv):
        if arg == "--expect-exit" and i + 1 < len(argv):
            return int(argv[i + 1])
        if arg.startswith("--expect-exit="):
            return int(arg.split("=", 1)[1])
    return None

def _run() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--config", type=Path, default=None)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--fixture", type=Path, default=None)
    parser.add_argument(
        "--expect-exit",
        type=int,
        default=None,
        help=(
            "assert the run's exit code equals this value and exit 0 when it "
            "does; used to prove a negative case (a fixture that must FAIL) "
            "from a CI gate that only reads exit 0 as success"
        ),
    )
    parser.add_argument("--body-file", type=Path)
    parser.add_argument("--subjects-file", type=Path)
    parser.add_argument("--messages-file", type=Path)
    parser.add_argument("--files-count", type=int, default=0)
    parser.add_argument("--title-file", type=Path)
    parser.add_argument(
        "--title-kind",
        choices=("issue", "pr"),
        default="issue",
        help="which title rule to apply in --title-file mode",
    )
    args = parser.parse_args()

    cfg = load_config(args.config or repo_root() / "governance.toml")

    if args.self_test:
        tests_dir = repo_root() / "scripts" / "tests" / "policy-lint"
        fixtures = fixture_files(tests_dir)
        if not fixtures:
            print("FAIL: no fixtures under scripts/tests/policy-lint/")
            return 1
        bad = 0
        for path in fixtures:
            data = json.loads(path.read_text(encoding="utf-8"))
            got = run_fixture(path, cfg)
            expected = int(data.get("expected_exit", 0))
            mark = "ok" if got == expected else "MISMATCH"
            if got != expected:
                bad += 1
            print(f"  fixture {path.name}: exit {got} (expected {expected}) {mark}")
        if bad:
            print(f"SELF-TEST FAIL: {bad}/{len(fixtures)} fixtures mismatched")
            return 1
        print(f"SELF-TEST PASS: {len(fixtures)}/{len(fixtures)} fixtures match their teeth")
        return 0

    if args.fixture:
        return run_fixture(args.fixture, cfg)

    if args.body_file or args.subjects_file or args.messages_file:
        missing = [
            name
            for name, val in (
                ("--body-file", args.body_file),
                ("--subjects-file", args.subjects_file),
                ("--messages-file", args.messages_file),
            )
            if val is None
        ]
        if missing:
            print(f"FAIL: PR mode needs all inputs; missing {', '.join(missing)}")
            return 1
        body = args.body_file.read_text(encoding="utf-8")
        subjects = [
            line.strip()
            for line in args.subjects_file.read_text(encoding="utf-8").splitlines()
            if line.strip()
        ]
        messages = args.messages_file.read_text(encoding="utf-8")
        return verdict(*run_checks(cfg, body, subjects, messages, args.files_count))

    if args.title_file:
        title = args.title_file.read_text(encoding="utf-8")
        return verdict(check_title(title, args.title_kind, cfg), [], label=args.title_kind)

    # No PR context: a local run still proves governance.toml parses and
    # carries the full schema. That is the whole point of the exit-0 path.
    print("PASS: governance.toml schema validated; no PR context, checks skipped")
    return 0


if __name__ == "__main__":
    sys.exit(main())
