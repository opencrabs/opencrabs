#!/usr/bin/env python3
r"""Ratchets on the seam between the GUI and the backend, R1 to R7.

The GUI is moving behind `crate::backend`: every place the window opens the
ledger, holds the agent service, or touches the disk by itself is a door the
remote backend cannot go through. Each rule below counts one kind of door and
compares the count with a baseline written in this file. Both directions fail:

  count above baseline  a new door was opened. Route it through an `Op`, a
                        fact, or `localize`, or (for a window-only function)
                        add it to LOCAL_ONLY in a commit someone reviews.
  count below baseline  good, and the number is now stale: the message says
                        "baixa o baseline para N". The number only goes down,
                        and it goes down by commit, so the history shows who
                        closed which door.

Pure Python, no cargo, milliseconds. The rules read text, so they are as
strong as their patterns and no stronger; the limits are listed per rule.

What is counted
  Code only. A small lexer blanks comments (`//`, `///`, `//!`, `/* */`) so a
  doc line that names a pattern does not count; string literals still count
  for the rules that look for wire names (R7). Test code is ignored: every
  item under `#[cfg(test)]` (the attribute to the item's closing brace), every
  file declared `#[cfg(test)] mod x;`, and `src/gui/tests/`. That is the
  brief's "everything below #[cfg(test)]" made exact: a `#[cfg(test)]`
  helper in the middle of a file (backend_glue.rs has one) hides only
  itself, not the production code after it. R4 is the one exception: the
  parity tests it looks for live under `#[cfg(test)]` by design.

Limits
  Text, not types. `use std::fs; fs::read(..)` counts once, at the `use`.
  A LOCAL_ONLY entry matches every function of that name in that file.

Functions
  R2, R3 and R6 exempt whole functions. A function is `fn <name>` up to its
  matching brace, found on the blanked text, so braces inside strings and
  comments do not confuse it. A site is inside every function whose body
  contains it; an exemption on any enclosing function applies.

Exit codes: 0 every rule at its baseline, 1 a rule moved, 2 bad usage or a
stale LOCAL_ONLY entry.
"""
from __future__ import annotations

import argparse
import functools
import re
import shutil
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Baselines. Each number is the measured count on the tree named in its
# comment; it only goes down. Measured 2026-11-24 on rb/f5 @ 995badcce.
BASELINES = {
    # The bridge accessor on `Backend`: 35 callers that still open the
    # ledger themselves plus its definition in backend_glue.rs. The F5
    # handover quoted 57 from F4's report; this tree has 36 raw grep hits
    # for `local_db_path(` in src/gui, all of them code, so 57 is not this
    # tree's number and 36 is.
    "R1": 7,
    # Ledger opened by the window outside LOCAL_ONLY: the L1/L2/L3/L5
    # functions the inventory routes to an `op`, not migrated yet
    # (sessions.rs 37, agent_bridge.rs 12, transcript.rs 8, the rest 5).
    "R2": 9,
    # The agent service reached around the backend: the `facto` and
    # `chamada` sites of L6/L7/L8 plus agent_bridge.rs outside `build`.
    "R3": 10,
    # ops! names missing an impl, a fixture, or a parity test. The brief
    # expects 0; it is 3 because tests/fixtures/gui_backend/ does not exist
    # yet: LoadSessions, RenameSession and ClearMessages have their impl and
    # their parity! test but no JSON fixture. Fixtures are outside this
    # package (scripts and workflow only), so the gap is recorded, not hidden.
    "R4": 0,
    # The window branching on the remote backend outside the seam files.
    "R5": 0,
    # The window touching the disk outside LOCAL_ONLY and without localize:
    # the `blob` functions of C5, plus L5 prompt files and L7 task detail.
    # L8 migrated its two disk sites (channel ownership, cd entries) to the
    # ChannelStatus and ListDir operations, which took the count to 20.
    "R6": 9,
    # The retired remote panel and its polling. The brief expects 0; it is 1
    # because src/remote/scopes.rs maps the A2A method "tasks/get" to its
    # scope (a2a:read). That is the A2A protocol table, not the panel; it
    # leaves with the panel's server side or stays as the one known site.
    "R7": 1,
}

# Functions of the window that may touch the disk, the ledger or the service
# by themselves: what the inventory (INVENTARIO-backend.md, F2) routes to
# `janela` (belongs to this window, never to the crab) and `boot` (builds the
# local crab before any backend exists). One entry per function, with why.
# A new function that touches the disk enters here by a reviewed commit, or
# goes through `localize`.
LOCAL_ONLY: dict[tuple[str, str], str] = {
    # janela: C5, previews and thumbnails work on already localized files
    ("src/gui/preview.rs", "tool_available"): "probes a local tool binary",
    ("src/gui/preview.rs", "is_usable"): "local preview cache",
    ("src/gui/preview.rs", "extract_poster"): "local ffmpeg on the localized path",
    ("src/gui/preview.rs", "duration_secs"): "local ffprobe on the localized path",
    ("src/gui/preview.rs", "launch"): "opens the localized file on this machine",
    ("src/gui/thumbs.rs", "write_square"): "local thumbnail cache",
    ("src/gui/thumbs.rs", "is_usable"): "local thumbnail cache",
    ("src/gui/thumbs.rs", "prune"): "local thumbnail cache",
    # janela: C7, layout and rail activity of this window, keyed by crab
    ("src/gui/layout_store.rs", "load"): "this window's layout",
    ("src/gui/layout_store.rs", "save"): "this window's layout",
    ("src/gui/layout_store.rs", "discard"): "this window's layout",
    ("src/gui/rail_activity.rs", "load"): "this window's rail activity",
    ("src/gui/rail_activity.rs", "save"): "this window's rail activity",
    # janela: L6, [gui] config of whoever looks
    ("src/gui/dashboard_screens.rs", "toggle_themes"): "[gui] config of this window",
    ("src/gui/dashboard_screens.rs", "close_themes"): "[gui] config of this window",
    ("src/gui/dashboard_screens.rs", "keep_transparency"): "[gui] config of this window",
    ("src/gui/dashboard_screens.rs", "pick_theme"): "[gui] config of this window",
    ("src/gui/dashboard_screens.rs", "pick_base"): "[gui] config of this window",
    ("src/gui/dashboard_screens.rs", "keep_accent"): "[gui] config of this window",
    # janela: L8
    ("src/gui/config_reload.rs", "watch"): "watches this window's config",
    ("src/gui/dashboard_keys.rs", "play_plan_chime"): "[gui] config of this window",
    ("src/gui/dashboard_topics.rs", "cycle_topic_edge"): "[gui] config of this window",
    ("src/gui/editor.rs", "write_clipboard_image"): "writes locally; the upload is a blob (P8)",
    ("src/gui/fonts.rs", "keep_ui"): "[gui] config of this window",
    ("src/gui/logging.rs", "init"): "this window's log file",
    # janela: P3, P4
    ("src/gui/bang.rs", "open_in_terminal"): "local-only capability (Cap::OpenTerminal)",
    ("src/gui/voice_input.rs", "write_clip"): "records locally; the clip goes up as a turn attachment",
    ("src/gui/voice_media.rs", "decode"): "decodes the already localized blob",
    # boot: F4, builds the local crab before a backend exists
    ("src/gui/agent_bridge.rs", "build"): "builds the local Ctx from the window's service",
    ("src/gui/agent_bridge.rs", "boot_resume"): "boot: rescans the turns this process died inside; a remote crab resumes its own",
    ("src/gui/app.rs", "run"): "process entry, reads the config",
    ("src/gui/boot.rs", "ledger_path"): "where the local ledger lives",
    ("src/gui/boot.rs", "boot_config"): "loads the config before a backend exists",
}

# R3 exempts the seam itself and the one function that hands the service to it.
R3_FILES = {"src/gui/backend_glue.rs"}
R3_FUNCTIONS = {("src/gui/agent_bridge.rs", "build")}
R5_FILES = {"src/gui/backend_glue.rs", "src/gui/backend_remote.rs", "src/backend/cap.rs"}

FIXTURES = "tests/fixtures/gui_backend"


# ---------------------------------------------------------------- lexing


def blank(text: str) -> tuple[str, str]:
    """Two copies of `text`, same length, newlines kept.

    `code`: comments replaced by spaces, strings intact (what a pattern sees).
    `bare`: comments and string contents replaced by spaces (what the brace
    matcher sees).
    """
    code = list(text)
    bare = list(text)
    n = len(text)
    i = 0

    def wipe(a: int, b: int, both: bool) -> None:
        if b <= a:
            return
        spaces = list(NOT_NL.sub(" ", text[a:b]))
        bare[a:b] = spaces
        if both:
            code[a:b] = spaces

    while True:
        m = TOKEN.search(text, i)
        if not m:
            break
        i, tok = m.start(), m.group(0)
        if tok == "//":
            j = text.find("\n", i)
            j = n if j < 0 else j
            wipe(i, j, True)
            i = j
        elif tok == "/*":
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif text.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            wipe(i, j, True)
            i = j
        elif tok.endswith('"') and "r" in tok:
            close = '"' + m.group(1)
            j = text.find(close, m.end())
            j = n if j < 0 else j
            wipe(m.end(), j, False)
            i = j + len(close)
        elif tok.endswith('"'):
            j = m.end()
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            wipe(m.end(), min(j, n), False)
            i = j + 1
        else:
            # char literal ('x', '\n', '\u{..}') or a lifetime ('a)
            if i + 1 < n and text[i + 1] == "\\":
                j = text.find("'", i + 2)
                j = n if j < 0 else j
                wipe(i + 1, j, False)
                i = j + 1
            elif i + 2 < n and text[i + 2] == "'":
                wipe(i + 1, i + 2, False)
                i += 3
            else:
                i += 1
    return "".join(code), "".join(bare)


NOT_NL = re.compile(r"[^\n]")

# What starts a comment, a string (plain, byte, raw, byte raw) or a char.
TOKEN = re.compile(r"""//|/\*|\bb?r(#*)"|"|'""")


@dataclass
class Source:
    rel: str
    code: str  # comments blanked, test tail cut
    bare: str  # comments and strings blanked, test tail cut
    full_code: str  # comments blanked, nothing cut (R4)
    full_bare: str  # comments and strings blanked, nothing cut
    line_starts: list[int]
    fns: list[tuple[str, int, int]]  # name, body start, body end

    def line(self, offset: int) -> int:
        lo, hi = 0, len(self.line_starts) - 1
        while lo < hi:
            mid = (lo + hi + 1) // 2
            if self.line_starts[mid] <= offset:
                lo = mid
            else:
                hi = mid - 1
        return lo + 1

    def enclosing(self, offset: int) -> list[tuple[str, int, int]]:
        return [f for f in self.fns if f[1] <= offset < f[2]]


FN = re.compile(r"\bfn\s+([A-Za-z_]\w*)")


def braces(bare: str) -> dict[int, int]:
    """Offset of every `{` to the offset just past its matching `}`."""
    pairs, stack = {}, []
    for m in BRACE.finditer(bare):
        if m.group(0) == "{":
            stack.append(m.start())
        elif stack:
            pairs[stack.pop()] = m.end()
    return pairs


BRACE = re.compile(r"[{}]")
OPEN_OR_END = re.compile(r"[{;]")


def functions(bare: str, pairs: dict[int, int]) -> list[tuple[str, int, int]]:
    out = []
    for m in FN.finditer(bare):
        # the body opens at the first `{` before a `;` (a bodiless decl)
        j = OPEN_OR_END.search(bare, m.end())
        if j and j.group(0) == "{" and j.start() in pairs:
            out.append((m.group(1), j.start(), pairs[j.start()]))
    return out


CFG_TEST = re.compile(r"#\[cfg\(test\)\]")
ATTR = re.compile(r"\s*#\[[^\]]*\]")
TEST_MOD = re.compile(r"#\[cfg\(test\)\](?:\s*#\[[^\]]*\])*\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;")


def test_items(bare: str, pairs: dict[int, int]) -> list[tuple[int, int]]:
    """Spans of the items under `#[cfg(test)]`: the attribute to the item's
    closing brace, or to its `;` for a bodiless one (`mod x;`, `use ..;`)."""
    spans = []
    for m in CFG_TEST.finditer(bare):
        j = m.end()
        while a := ATTR.match(bare, j):
            j = a.end()
        k = OPEN_OR_END.search(bare, j)
        if not k:
            spans.append((m.start(), len(bare)))
        elif k.group(0) == "{":
            spans.append((m.start(), pairs.get(k.start(), len(bare))))
        else:
            spans.append((m.start(), k.end()))
    return spans


def _wipe(s: str, spans: list[tuple[int, int]]) -> str:
    if not spans:
        return s
    chars = list(s)
    for a, b in spans:
        chars[a:b] = NOT_NL.sub(" ", s[a:b])
    return "".join(chars)


@functools.lru_cache(maxsize=None)
def load(root: Path, rel: str) -> Source:
    text = (root / rel).read_text(encoding="utf-8", errors="replace")
    full_code, full_bare = blank(text)
    spans = test_items(full_bare, braces(full_bare))
    code, bare = _wipe(full_code, spans), _wipe(full_bare, spans)
    starts = [0] + [m.end() for m in re.finditer("\n", text)]
    return Source(rel, code, bare, full_code, full_bare, starts, functions(bare, braces(bare)))


def test_modules(root: Path, files: list[Path]) -> set[str]:
    """Files declared `#[cfg(test)] mod x;`: `x.rs` and `x/` next to a
    main.rs, lib.rs or mod.rs, under `foo/` for `foo.rs`. Test-only whole."""
    out = set()
    for p in files:
        rel = p.relative_to(root).as_posix()
        if "cfg(test)" not in p.read_text(encoding="utf-8", errors="replace"):
            continue
        bare = load(root, rel).full_bare
        here = p.parent if p.name in ("main.rs", "lib.rs", "mod.rs") else p.with_suffix("")
        for m in TEST_MOD.finditer(bare):
            out.add((here / f"{m.group(1)}.rs").relative_to(root).as_posix())
            out.add((here / m.group(1)).relative_to(root).as_posix() + "/")
    return out


def sources(root: Path, *dirs: str) -> list[Source]:
    out = []
    for d in dirs:
        base = root / d
        if not base.is_dir():
            continue
        files = sorted(base.rglob("*.rs"))
        tests = test_modules(root, files)
        for p in files:
            rel = p.relative_to(root).as_posix()
            if rel.startswith("src/gui/tests/") or rel in tests or any(rel.startswith(t) for t in tests if t.endswith("/")):
                continue
            out.append(load(root, rel))
    return out


# ---------------------------------------------------------------- rules


@dataclass
class Result:
    rid: str
    label: str
    sites: list[str]  # "path:line  what"


def scan(srcs, pattern: re.Pattern, skip=lambda src, off: False) -> list[str]:
    sites = []
    for src in srcs:
        for m in pattern.finditer(src.code):
            if skip(src, m.start()):
                continue
            sites.append(f"{src.rel}:{src.line(m.start())}  {m.group(0)}")
    return sites


def in_any(src: Source, off: int, names: set[tuple[str, str]]) -> bool:
    return any((src.rel, f[0]) in names for f in src.enclosing(off))


def rules(root: Path, local_only: set[tuple[str, str]]) -> list[Result]:
    gui = sources(root, "src/gui")
    out = []

    out.append(Result("R1", "local_db_path", scan(gui, re.compile(r"local_db_path\("))))

    out.append(
        Result(
            "R2",
            "Database::connect, Repository::new",
            scan(gui, re.compile(r"Database::connect|Repository::new\("), lambda s, o: in_any(s, o, local_only)),
        )
    )

    out.append(
        Result(
            "R3",
            ".service. / bridge.service",
            scan(
                gui,
                re.compile(r"bridge\.service|\.service\."),
                lambda s, o: s.rel in R3_FILES or in_any(s, o, R3_FUNCTIONS),
            ),
        )
    )

    out.append(Result("R4", "ops without impl/fixture/parity", r4(root)))

    out.append(
        Result(
            "R5",
            "Backend::Remote / is_remote",
            scan(gui, re.compile(r"Backend::Remote|is_remote"), lambda s, o: s.rel in R5_FILES),
        )
    )

    def r6_skip(src: Source, off: int) -> bool:
        if in_any(src, off, local_only):
            return True
        inner = src.enclosing(off)
        if not inner:
            return False
        _, a, b = min(inner, key=lambda f: f[2] - f[1])
        return "localize" in src.code[a:b]

    out.append(Result("R6", "std::fs / tokio::fs / .exists()", scan(gui, re.compile(r"std::fs::|tokio::fs::|\.exists\(\)"), r6_skip)))

    out.append(
        Result(
            "R7",
            "remote_panel / remote_rail_rows / tasks/get",
            scan(sources(root, "src/gui", "src/remote"), re.compile(r"remote_panel|remote_rail_rows|tasks/get")),
        )
    )
    return out


OPS = re.compile(r"\bops!\s*\{([^}]*)\}")


def r4(root: Path) -> list[str]:
    # Every file, test-only modules included: parity tests live there.
    base = root / "src/backend"
    files = sorted(base.rglob("*.rs")) if base.is_dir() else []
    backend = [load(root, p.relative_to(root).as_posix()) for p in files]
    everything = "\n".join(s.full_code for s in backend)
    missing = []
    for src in backend:
        for m in OPS.finditer(src.full_code):
            where = f"{src.rel}:{src.line(m.start())}"
            for name in re.findall(r"[A-Za-z_]\w*", m.group(1)):
                n = re.escape(name)
                if not re.search(rf"\bimpl\b[^{{;]*\bOp\s+for\s+{n}\b", everything):
                    missing.append(f"{where}  {name}: no `impl Op for {name}`")
                if not (root / FIXTURES / f"{name}.json").is_file():
                    missing.append(f"{where}  {name}: no {FIXTURES}/{name}.json")
                if not re.search(rf"\bparity!\(\s*[^,()]+,\s*{n}\b", everything):
                    missing.append(f"{where}  {name}: no `parity!(_, {name}`")
    return missing


def stale_local_only(root: Path, local_only: set[tuple[str, str]]) -> list[str]:
    """LOCAL_ONLY entries naming a function the tree no longer has."""
    stale = []
    cache: dict[str, Source] = {}
    for rel, name in sorted(local_only):
        if not (root / rel).is_file():
            stale.append(f"{rel}::{name} (no such file)")
            continue
        src = cache.setdefault(rel, load(root, rel))
        if not any(f[0] == name for f in src.fns):
            stale.append(f"{rel}::{name} (no such function)")
    return stale


def check(
    root: Path,
    baselines: dict[str, int],
    local_only: set[tuple[str, str]],
    out=print,
    allow_below: bool = False,
    only_files: tuple[str, ...] = (),
) -> int:
    worst = 0
    for r in rules(root, local_only):
        sites = r.sites
        if only_files:
            # Measuring scope: count only sites whose file starts with one of
            # the given prefixes. No baseline comparison here, the global
            # baseline says nothing about a file subset; the numbers are for
            # the package's own brief gate.
            sites = [s for s in sites if s.split(":")[0].startswith(only_files)]
            out(f"{r.rid} {r.label}: {len(sites)} [files: {', '.join(only_files)}]")
            continue
        count, base = len(sites), baselines[r.rid]
        out(f"{r.rid} {r.label}: {count} (baseline {base})")
        if count > base:
            worst = 1
            out(f"::error::{r.rid} {r.label}: {count} sites, baseline {base}. New door(s) among:")
            for s in sites:
                out(f"  {s}")
        elif count < base:
            if allow_below:
                out(
                    f"::warning::{r.rid} {r.label}: {count} sites, baseline {base}."
                    " Below baseline, allowed on a package branch:"
                    " lower the baseline in scripts/check-backend-seam.py at the merge."
                )
            else:
                worst = 1
                out(f"::error::{r.rid} {r.label}: baixa o baseline para {count} (scripts/check-backend-seam.py)")
    return worst


# ---------------------------------------------------------------- self-test


def self_test() -> int:
    failures: list[str] = []
    zero = {k: 0 for k in BASELINES}
    lo = {("src/gui/thumbs.rs", "prune"), ("src/gui/agent_bridge.rs", "build")}

    planted: list[Path] = []

    def plant(files: dict[str, str]) -> Path:
        d = Path(tempfile.mkdtemp(prefix="seam-selftest-"))
        planted.append(d)
        for rel, body in files.items():
            p = d / rel
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(body, encoding="utf-8")
        return d

    def run(files, baselines=zero, **kw):
        lines: list[str] = []
        code = check(plant(files), baselines, lo, lines.append, **kw)
        return code, "\n".join(lines)

    def expect_fail(case, files, where):
        code, text = run(files)
        if code != 1 or where not in text:
            failures.append(f"{case}: wanted exit 1 naming {where}, got {code}:\n{text}")

    def expect_pass(case, files, baselines=zero):
        code, text = run(files, baselines)
        if code != 0:
            failures.append(f"{case}: wanted exit 0, got {code}:\n{text}")

    # Each rule fails on a planted violation and names file and line.
    expect_fail("R1", {"src/gui/a.rs": "fn f(b: B) {\n    let p = b.local_db_path();\n}\n"}, "src/gui/a.rs:2")
    expect_fail("R2", {"src/gui/a.rs": "fn f() {\n\n    Database::connect(p);\n}\n"}, "src/gui/a.rs:3")
    expect_fail("R2 repo", {"src/gui/a.rs": "fn f() {\n    SessionRepository::new(pool);\n}\n"}, "src/gui/a.rs:2")
    expect_fail("R3", {"src/gui/a.rs": "fn f() {\n    x.service.run();\n}\n"}, "src/gui/a.rs:2")
    expect_fail("R3 bridge", {"src/gui/a.rs": "fn f() {\n    let s = bridge.service;\n}\n"}, "src/gui/a.rs:2")
    expect_fail("R4 no impl", {"src/backend/d.rs": "\nops! { Foo }\n"}, "src/backend/d.rs:2  Foo: no `impl Op for Foo`")
    expect_fail("R4 no fixture", {"src/backend/d.rs": "ops! { Foo }\n"}, "Foo: no tests/fixtures/gui_backend/Foo.json")
    expect_fail("R4 no parity", {"src/backend/d.rs": "ops! { Foo }\n"}, "Foo: no `parity!(_, Foo`")
    expect_fail("R5", {"src/gui/a.rs": "fn f() {\n    if is_remote() {}\n}\n"}, "src/gui/a.rs:2")
    expect_fail("R5 variant", {"src/gui/a.rs": "fn f() {\n    match b { Backend::Remote(_) => {} }\n}\n"}, "src/gui/a.rs:2")
    expect_fail("R6", {"src/gui/a.rs": "fn f() {\n    std::fs::read(p);\n}\n"}, "src/gui/a.rs:2")
    expect_fail("R6 tokio", {"src/gui/a.rs": "fn f() {\n    tokio::fs::read(p);\n}\n"}, "src/gui/a.rs:2")
    expect_fail("R6 exists", {"src/gui/a.rs": "fn f() {\n    p.exists();\n}\n"}, "src/gui/a.rs:2")
    expect_fail("R7 gui", {"src/gui/a.rs": "fn remote_panel() {}\n"}, "src/gui/a.rs:1")
    expect_fail("R7 remote", {"src/remote/a.rs": 'fn f() {\n    call("tasks/get");\n}\n'}, "src/remote/a.rs:2")
    expect_fail("R7 rows", {"src/remote/a.rs": "fn f() {\n    remote_rail_rows();\n}\n"}, "src/remote/a.rs:2")

    # A count below the baseline fails too, and says what to write.
    code, text = run({"src/gui/a.rs": ""}, {**zero, "R1": 2})
    if code != 1 or "baixa o baseline para 0" not in text:
        failures.append(f"below baseline: wanted exit 1 and 'baixa o baseline para 0', got {code}:\n{text}")

    # --allow-below: the same drop warns and passes (package-branch mode).
    code, text = run({"src/gui/a.rs": ""}, {**zero, "R1": 2}, allow_below=True)
    if code != 0 or "Below baseline" not in text:
        failures.append(f"allow-below: wanted exit 0 with a warning, got {code}:\n{text}")

    # --files: counts restricted to the given prefixes, no baseline math.
    two_files = {
        "src/gui/a.rs": "fn f(b: B) {\n    let p = b.local_db_path();\n}\n",
        "src/gui/b.rs": "fn g(c: C) {\n    let p = c.local_db_path();\n}\n",
    }
    code, text = run(two_files, zero, only_files=("src/gui/a.rs",))
    if code != 0 or "R1 local_db_path: 1 [files: src/gui/a.rs]" not in text or "b.rs" in text:
        failures.append(f"--files scope: wanted exit 0 counting only a.rs, got {code}:\n{text}")
    code, text = run(two_files, zero, only_files=("src/gui/",))
    if code != 0 or "R1 local_db_path: 2 [files: src/gui/]" not in text:
        failures.append(f"--files prefix: wanted exit 0 counting both, got {code}:\n{text}")

    # What does not count.
    expect_pass(
        "comments, strings in braces, test tail",
        {
            "src/gui/a.rs": (
                "//! local_db_path( in a doc\n"
                "/// Database::connect\n"
                "/* std::fs:: /* nested */ is_remote */\n"
                'fn f() { let s = "}{"; let c = \'{\'; } // x.service.y\n'
                "#[cfg(test)]\n"
                "mod tests { fn t() { std::fs::read(p); b.local_db_path(); } }\n"
            ),
            "src/gui/tests/t.rs": "fn t() { std::fs::read(p); }\n",
        },
    )
    expect_pass(
        "LOCAL_ONLY, localize, seam files",
        {
            "src/gui/thumbs.rs": "fn prune() {\n    let f = || std::fs::remove_file(p);\n    Database::connect(p);\n}\n",
            "src/gui/b.rs": "fn g() {\n    let p = localize(x);\n    p.exists();\n}\n",
            "src/gui/backend_glue.rs": "fn h() { x.service.y(); if is_remote {} }\n",
            "src/gui/agent_bridge.rs": "async fn build() -> R { s.service.z(); }\n",
        },
    )
    # The LOCAL_ONLY exemption is by function, not by file.
    expect_fail("LOCAL_ONLY is per function", {"src/gui/thumbs.rs": "fn prune() {}\nfn other() {\n    std::fs::read(p);\n}\n"}, "src/gui/thumbs.rs:3")
    # localize in another function does not cover this one.
    expect_fail("localize is per function", {"src/gui/b.rs": "fn g() { localize(x); }\nfn h() {\n    p.exists();\n}\n"}, "src/gui/b.rs:3")
    # A #[cfg(test)] item hides itself only; a test-only module file hides whole.
    expect_fail(
        "cfg(test) item mid-file",
        {"src/gui/a.rs": "#[cfg(test)]\nfn helper() { std::fs::read(p); }\n\nfn real(b: B) {\n    b.local_db_path();\n}\n"},
        "src/gui/a.rs:5",
    )
    expect_pass(
        "cfg(test) mod file",
        {"src/gui/main.rs": "#[cfg(test)]\nmod t_only;\nfn main() {}\n", "src/gui/t_only.rs": "fn t() { std::fs::read(p); }\n"},
    )
    # A complete op: impl, fixture, parity under #[cfg(test)] (R4 reads it).
    expect_pass(
        "R4 complete op",
        {
            "src/backend/d.rs": "ops! { Foo }\nimpl Op for Foo {}\n#[cfg(test)]\nmod t { fn x() { parity!(\n    gateway,\n    Foo { a: 1 }\n); } }\n",
            "tests/fixtures/gui_backend/Foo.json": "{}\n",
        },
    )
    # Stale LOCAL_ONLY entries are reported.
    if stale_local_only(plant({"src/gui/thumbs.rs": "fn other() {}\n"}), {("src/gui/thumbs.rs", "prune")}) != [
        "src/gui/thumbs.rs::prune (no such function)"
    ]:
        failures.append("stale LOCAL_ONLY entry not reported")

    for d in planted:
        shutil.rmtree(d, ignore_errors=True)
    if failures:
        for f in failures:
            print(f"self-test FAIL: {f}")
        return 1
    print("self-test OK: 7 rules planted (19 cases), below-baseline, 7 exemption cases, stale LOCAL_ONLY")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--self-test", action="store_true", help="plant a violation per rule and check the exit")
    ap.add_argument("--root", type=Path, default=REPO, help="tree to check (default: this repo)")
    ap.add_argument(
        "--allow-below",
        action="store_true",
        help="a count below the baseline is a warning, not an error (package branches;"
        " the baseline is lowered at the merge into the integration branch)",
    )
    ap.add_argument(
        "--files",
        nargs="+",
        metavar="PREFIX",
        help="count only sites in files starting with these prefixes and skip the"
        " baseline comparison (measuring tool for a package's own brief gate)",
    )
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    local_only = set(LOCAL_ONLY)
    stale = stale_local_only(args.root, local_only)
    if stale:
        for s in stale:
            print(f"::error::LOCAL_ONLY names a function that is gone, drop it: {s}")
        return 2
    return check(args.root, BASELINES, local_only, allow_below=args.allow_below, only_files=tuple(args.files or ()))


if __name__ == "__main__":
    sys.exit(main())
