#!/usr/bin/env python3
"""Exhaustiveness of the GUI/backend protocol document.

Every operation listed in an `ops!{}` in src/backend must appear in
docs/gui-backend-protocol.md with its wire name, its type and its scope, and
the document must list nothing that the code does not declare. Every event
kind and every error kind the code serializes must be named in the document
too. Each operation must have its JSON fixture in tests/fixtures/gui_backend/,
or be named in PENDING_FIXTURES, a list that can only shrink: an entry whose
fixture has landed, or whose operation is gone, fails the test.

Fixtures belong to the package that adds the operation; this test only reads
them. The op names the Python client hard-codes are checked against the code
once those operations exist.

Pure text, no cargo: it reads the Rust sources and runs in milliseconds.

    python3 tests/gui_backend_protocol_test.py              # check the tree
    python3 tests/gui_backend_protocol_test.py --self-test  # plant faults first
"""

from __future__ import annotations

import json
import re
import sys
import tempfile
from pathlib import Path

DOC = "docs/gui-backend-protocol.md"
FIXTURES = "tests/fixtures/gui_backend"
CLIENT = "scripts/client-gui-backend.py"
BACKEND = "src/backend"

# Operations whose fixture has not landed yet. Shrink only. The three below
# predate the fixtures directory (see R4 in scripts/check-backend-seam.py).
PENDING_FIXTURES = {"LoadSessions", "RenameSession", "ClearMessages"}

SCOPES = {"Read": "a2a:read", "Send": "a2a:send", "Manage": "a2a:manage", "Admin": "a2a:admin"}


def strip_comments(code: str) -> str:
    code = re.sub(r"/\*.*?\*/", "", code, flags=re.S)
    return re.sub(r"//[^\n]*", "", code)


def snake(name: str) -> str:
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


def enum_variants(code: str, enum: str) -> list[str]:
    """Variant names of `pub enum <enum>`, top level of its body only."""
    m = re.search(rf"\bpub\s+enum\s+{enum}\s*\{{", code)
    if not m:
        return []
    depth, i, start = 1, m.end(), m.end()
    while depth and i < len(code):
        depth += {"{": 1, "}": -1}.get(code[i], 0)
        i += 1
    body, flat, depth = code[start : i - 1], [], 0
    for ch in body:
        if ch in "{(":
            depth += 1
        elif ch in "})":
            depth -= 1
        elif depth == 0:
            flat.append(ch)
    text = re.sub(r"#\[[^\]]*\]", "", "".join(flat))
    return re.findall(r"(?:^|,)\s*([A-Z]\w*)", text)


def declared_ops(root: Path) -> tuple[dict[str, tuple[str, str]], list[str]]:
    """{type: (wire name, scope string)} for every op in an `ops!{}`."""
    base = root / BACKEND
    files = sorted(base.rglob("*.rs")) if base.is_dir() else []
    code = {p: strip_comments(p.read_text(encoding="utf-8")) for p in files}
    everything = "\n".join(code.values())
    ops, errors = {}, []
    for path, text in code.items():
        if path.name == "mod.rs":
            continue  # the macro definitions, not a domain
        for m in re.finditer(r"\bops!\s*\{([^}]*)\}", text):
            for ty in re.findall(r"[A-Za-z_]\w*", m.group(1)):
                impl = re.search(rf"\bimpl\b[^{{;]*\bOp\s+for\s+{ty}\s*\{{", everything)
                if not impl:
                    errors.append(f"{ty}: no `impl Op for {ty}`")
                    continue
                block = everything[impl.end() : impl.end() + 4000]
                name = re.search(r'const\s+NAME\s*:\s*&\'static\s+str\s*=\s*"([^"]+)"', block)
                scope = re.search(r"const\s+SCOPE\s*:\s*Scope\s*=\s*Scope::(\w+)", block)
                if not name or not scope or scope.group(1) not in SCOPES:
                    errors.append(f"{ty}: NAME or SCOPE not readable in its impl")
                    continue
                if ty in ops:
                    errors.append(f"{ty}: listed in two ops!{{}}")
                ops[ty] = (name.group(1), SCOPES[scope.group(1)])
    names = [n for n, _ in ops.values()]
    errors += [f"wire name {n} used twice" for n in sorted({n for n in names if names.count(n) > 1})]
    return ops, errors


def doc_ops(doc: str) -> dict[str, tuple[str, str]]:
    """{type: (wire name, scope)} from the operations table: | `name` | `Type` | `scope` |"""
    rows = re.findall(r"^\|\s*`([a-z_]+\.[a-z_]+)`\s*\|\s*`([A-Z]\w*)`\s*\|\s*`(a2a:\w+)`\s*\|", doc, re.M)
    return {ty: (name, scope) for name, ty, scope in rows}


def check(root: Path, pending: set[str] = PENDING_FIXTURES) -> list[str]:
    fails = []
    ops, errs = declared_ops(root)
    fails += errs
    doc_path = root / DOC
    if not doc_path.is_file():
        return fails + [f"{DOC}: missing"]
    doc = doc_path.read_text(encoding="utf-8")
    documented = doc_ops(doc)

    for ty, (name, scope) in sorted(ops.items()):
        if ty not in documented:
            fails.append(f"{ty} ({name}): no row in the {DOC} operations table")
        elif documented[ty] != (name, scope):
            fails.append(f"{ty}: doc says {documented[ty]}, code says {(name, scope)}")
        fixture = root / FIXTURES / f"{ty}.json"
        if fixture.is_file():
            if ty in pending:
                fails.append(f"{ty}: fixture landed, drop it from PENDING_FIXTURES")
            try:
                data = json.loads(fixture.read_text(encoding="utf-8"))
            except ValueError as e:
                fails.append(f"{fixture.relative_to(root)}: not JSON ({e})")
                continue
            if not isinstance(data, dict) or "params" not in data or "reply" not in data:
                fails.append(f"{fixture.relative_to(root)}: needs an object with `params` and `reply`")
            elif data.get("op", name) != name:
                fails.append(f"{fixture.relative_to(root)}: op {data.get('op')!r}, code says {name!r}")
        elif ty not in pending:
            fails.append(f"{ty}: no {FIXTURES}/{ty}.json")
    for ty in sorted(set(documented) - set(ops)):
        fails.append(f"{ty}: in the {DOC} operations table but in no ops!{{}}")
    for ty in sorted(pending - set(ops)):
        fails.append(f"{ty}: in PENDING_FIXTURES but in no ops!{{}}")

    for rel, enum in (("event.rs", "Event"), ("error.rs", "BackendError")):
        path = root / BACKEND / rel
        if not path.is_file():
            continue
        for v in enum_variants(strip_comments(path.read_text(encoding="utf-8")), enum):
            if f"`{snake(v)}`" not in doc:
                fails.append(f"{enum}::{v}: kind `{snake(v)}` not named in {DOC}")

    client = root / CLIENT
    if client.is_file():
        pins = re.findall(r'^[A-Z_]+_OP\s*=\s*"([^"]+)"\s*#\s*op:\s*([A-Z]\w*)', client.read_text(encoding="utf-8"), re.M)
        if not pins:
            fails.append(f"{CLIENT}: no `X_OP = \"name\"  # op: Type` pins found")
        for name, ty in pins:
            # Pending until the type lands; then the pinned name must be its NAME.
            if ty in ops and ops[ty][0] != name:
                fails.append(f"{CLIENT}: {ty} pinned as {name}, code says {ops[ty][0]}")
    return fails


def self_test() -> int:
    """Plant one fault per rule in a copy and require each to be caught."""
    good_ops = 'ops! { Foo }\nimpl Op for Foo {\n    const NAME: &\'static str = "d.foo";\n    const SCOPE: Scope = Scope::Read;\n}\n'
    good_doc = "| `d.foo` | `Foo` | `a2a:read` | x |\n`turn_done`\n"
    good_fix = '{"op": "d.foo", "params": {}, "reply": null}'
    cases = [
        ("clean tree passes", {}, None),
        ("op missing from doc", {DOC: "nothing\n"}, "Foo (d.foo): no row"),
        ("scope drift", {DOC: good_doc.replace("a2a:read", "a2a:admin")}, "Foo: doc says"),
        ("stale doc row", {DOC: good_doc + "| `d.bar` | `Bar` | `a2a:read` | x |\n"}, "Bar: in the"),
        ("no fixture", {f"{FIXTURES}/Foo.json": None}, "Foo: no tests/fixtures"),
        ("bad fixture shape", {f"{FIXTURES}/Foo.json": "{}"}, "needs an object"),
        ("fixture op drift", {f"{FIXTURES}/Foo.json": good_fix.replace("d.foo", "d.baz")}, "op 'd.baz'"),
        ("event kind undocumented", {f"{BACKEND}/event.rs": "pub enum Event { TurnDone, RailChanged }"}, "Event::RailChanged"),
        ("client op drift", {CLIENT: 'FOO_OP = "d.nope"  # op: Foo\n'}, "Foo pinned as d.nope"),
    ]
    bad = 0
    for label, overrides, want in cases:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            files = {
                f"{BACKEND}/d.rs": good_ops,
                f"{BACKEND}/event.rs": "pub enum Event { TurnDone }",
                DOC: good_doc,
                f"{FIXTURES}/Foo.json": good_fix,
                CLIENT: 'FOO_OP = "d.foo"  # op: Foo\nBAR_OP = "d.later"  # op: Bar\n',
            }
            files.update(overrides)
            for rel, body in files.items():
                if body is not None:
                    (root / rel).parent.mkdir(parents=True, exist_ok=True)
                    (root / rel).write_text(body, encoding="utf-8")
            fails = check(root, pending=set())
            ok = not fails if want is None else any(want in f for f in fails)
            print(f"self-test {'ok  ' if ok else 'FAIL'} {label}" + ("" if ok else f": {fails}"))
            bad += not ok
    return 1 if bad else 0


def main() -> int:
    if "--self-test" in sys.argv[1:]:
        return self_test()
    root = Path(__file__).resolve().parent.parent
    ops, _ = declared_ops(root)
    fails = check(root)
    for f in fails:
        print(f"FAIL {f}")
    have = sum((root / FIXTURES / f"{t}.json").is_file() for t in ops)
    print(f"{len(ops)} ops, {have} with fixture, {len(PENDING_FIXTURES)} pending: {'FAIL' if fails else 'ok'}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
