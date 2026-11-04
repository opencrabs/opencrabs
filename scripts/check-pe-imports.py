#!/usr/bin/env python3
r"""Fail CI when a Windows artifact imports a DLL a clean Windows does not have.

The bug this exists to stop: v0.5.15 shipped, a user on a fresh Windows ran
`opencrabs.exe`, and it died before the first line of its own output with
"VCRUNTIME140_1.dll was not found" / "VCOMP140.DLL was not found". Both come
from the MSVC runtime, which is not part of Windows: it arrives with Visual
Studio, the Office installer, or an explicit vc_redist install (`+crt-static`
in the build would have removed the first, OpenMP off the second). The dialog
told the user to reinstall the program, which was never the problem.

The gate is import-table archaeology, not a smoke run. `windows-latest` ships
the vc_redist 14.x pre-installed, so "the exe starts on the runner" is TRUE for
a binary that cannot start anywhere else. The bytes are the only honest signal,
and they can be read on any OS, which is why this runs on ubuntu and needs no
dumpbin, no wine, no Windows runner.

Rules
  crt           VCRUNTIME140*.dll, MSVCP140*.dll, VCOMP140.dll, CONCRT140.dll.
                Never present on a stock Windows, so it must either be linked
                statically or travel next to the exe.
  system        Any other import must be in SYSTEM_DLLS (or api-ms-win-* /
                ext-ms-win-*), i.e. shipped with Windows itself.
  shipped       Anything else is a failure UNLESS the archive carries that DLL
                beside the exe. App-local deployment is what Microsoft supports
                for these files, and it is the only answer for a third-party
                prebuilt like rtk.exe, which imports VCRUNTIME140.dll and can
                never be rebuilt here. The gate therefore checks CLOSURE: every
                DLL a binary asks for is either part of Windows or in the zip.
                A bare .exe is judged against the DLLs in its own directory.
                The DLL must also parse as a PE image: a zero-byte or truncated
                file satisfies the name and still fails the loader.

Usage
  scripts/check-pe-imports.py dist/*.exe
  scripts/check-pe-imports.py --zip opencrabs-v0.5.15-windows-amd64.zip
  scripts/check-pe-imports.py --self-test

Exit code is 0 only when every scanned binary is clean. The self-test builds
synthetic PE files in memory (a clean one and a polluting one) so the checker
is exercised on a machine with no Windows binary at hand.
"""

from __future__ import annotations

import argparse
import struct
import sys
import zipfile
from pathlib import Path

# The MSVC runtime family. Absent from every stock Windows install.
CRT_DLLS = {
    "vcruntime140.dll",
    "vcruntime140_1.dll",
    "vcruntime140_1d.dll",
    "vcruntime140d.dll",
    "msvcp140.dll",
    "msvcp140_1.dll",
    "msvcp140_2.dll",
    "msvcp140_atomic_wait.dll",
    "msvcp140_codecvt_ids.dll",
    "msvcp140d.dll",
    "vcomp140.dll",
    "vcomp140d.dll",
    "concrt140.dll",
    "msvcr100.dll",
    "msvcr110.dll",
    "msvcr120.dll",
}

# Shipped with Windows 10/11 (or by every supported build of it). Deliberately
# conservative: an import that is not here and is not an api-ms-win-* forwarder
# gets reported, because "it happened to be on the runner" is not a promise.
SYSTEM_DLLS = {
    "advapi32.dll",
    "bcrypt.dll",
    "bcryptprimitives.dll",
    "cabinet.dll",
    "combase.dll",
    "comctl32.dll",
    "comdlg32.dll",
    "crypt32.dll",
    "dnsapi.dll",
    "gdi32.dll",
    "gdiplus.dll",
    "iphlpapi.dll",
    "kernel32.dll",
    "kernelbase.dll",
    "mpr.dll",
    "msimg32.dll",
    "msvcrt.dll",
    "ncrypt.dll",
    "netapi32.dll",
    "ntdll.dll",
    "ole32.dll",
    "oleaut32.dll",
    "psapi.dll",
    "rpcrt4.dll",
    "secur32.dll",
    "setupapi.dll",
    "shell32.dll",
    "shlwapi.dll",
    "sspicli.dll",
    "user32.dll",
    "userenv.dll",
    "usp10.dll",
    "uxtheme.dll",
    "version.dll",
    "winhttp.dll",
    "wininet.dll",
    "winmm.dll",
    "wintrust.dll",
    "ws2_32.dll",
    "wtsapi32.dll",
    # The UCRT ships with Windows 10+, so these are not the v0.5.15 failure.
    # They are listed to stay honest: if a build reaches for them, it is
    # dynamic-CRT, which is a size/style question, not a crash.
    "ucrtbase.dll",
    "api-ms-win-crt-runtime-l1-1-0.dll",
}

SYSTEM_PREFIXES = ("api-ms-win-", "ext-ms-win-")


class PEError(Exception):
    pass


def _sections(data: bytes) -> tuple[int, list[tuple[int, int, int, int]]]:
    """Return (import_dir_rva, [(vaddr, vsize, rawptr, rawsize), ...])."""
    if data[:2] != b"MZ":
        raise PEError("missing MZ header")
    e_lfanew = struct.unpack_from("<I", data, 0x3C)[0]
    if data[e_lfanew : e_lfanew + 4] != b"PE\0\0":
        raise PEError("missing PE signature")
    coff = e_lfanew + 4
    nsections, opt_size = struct.unpack_from("<H", data, coff + 2)[0], struct.unpack_from(
        "<H", data, coff + 16
    )[0]
    opt = coff + 20
    magic = struct.unpack_from("<H", data, opt)[0]
    if magic == 0x20B:  # PE32+
        n_dirs_off, dirs_off = opt + 108, opt + 112
    elif magic == 0x10B:  # PE32
        n_dirs_off, dirs_off = opt + 92, opt + 96
    else:
        raise PEError(f"unknown optional-header magic 0x{magic:x}")
    if struct.unpack_from("<I", data, n_dirs_off)[0] < 2:
        return 0, []
    import_rva = struct.unpack_from("<I", data, dirs_off + 8)[0]
    sections = []
    for i in range(nsections):
        base = opt + opt_size + i * 40
        # Section header field order is VirtualSize, VirtualAddress,
        # SizeOfRawData, PointerToRawData. Reading it as vaddr-first silently
        # mis-maps every RVA, which is how the first version of this script
        # "read" garbage DLL names out of a real exe.
        vsize, vaddr, rawsize, rawptr = struct.unpack_from("<IIII", data, base + 8)
        sections.append((vaddr, vsize, rawptr, rawsize))
    return import_rva, sections


def _rva_to_offset(rva: int, sections: list[tuple[int, int, int, int]]) -> int | None:
    for vaddr, vsize, rawptr, rawsize in sections:
        span = max(vsize, rawsize)
        if vaddr <= rva < vaddr + span:
            delta = rva - vaddr
            if delta >= rawsize:
                return None
            return rawptr + delta
    return None


def imported_dlls(data: bytes) -> list[str]:
    """Every DLL named in the PE import directory, in table order."""
    import_rva, sections = _sections(data)
    if not import_rva:
        return []
    offset = _rva_to_offset(import_rva, sections)
    if offset is None:
        raise PEError("import directory RVA is not backed by a section")
    names: list[str] = []
    while True:
        if offset + 20 > len(data):
            raise PEError("truncated import descriptor")
        ilt, _ts, _fwd, name_rva, iat = struct.unpack_from("<IIIII", data, offset)
        if not (ilt or name_rva or iat):
            break
        name_off = _rva_to_offset(name_rva, sections) if name_rva else None
        if name_off is None:
            raise PEError(f"unreadable DLL name RVA 0x{name_rva:x}")
        end = data.find(b"\0", name_off)
        if end < 0:
            raise PEError("unterminated DLL name")
        names.append(data[name_off:end].decode("ascii", "replace"))
        offset += 20
    return names


def classify(dll: str) -> str:
    """'crt', 'system' or 'unknown'."""
    low = dll.lower()
    if low in CRT_DLLS or low.startswith("vcruntime14"):
        return "crt"
    if low in SYSTEM_DLLS or low.startswith(SYSTEM_PREFIXES):
        return "system"
    return "unknown"


def check_binary(
    label: str, data: bytes, shipped: frozenset[str] = frozenset(), report: bool = True
) -> list[str]:
    """Print a report per binary; return the human-readable failures.

    `shipped` holds the DLL names that travel next to this exe, lowercased. A
    non-system import fails only when nothing ships it: the question this gate
    answers is "will the loader find every dependency", not "is the CRT
    dynamic". rtk.exe is a prebuilt third party binary that needs
    VCRUNTIME140.dll, so a static CRT in our own exe could never have answered
    it, and closure is the only policy that covers both.
    """
    failures: list[str] = []
    shipped = frozenset(name.lower() for name in shipped)
    try:
        dlls = imported_dlls(data)
    except PEError as exc:
        failures.append(f"{label}: not a readable PE file ({exc})")
        return failures
    if not dlls:
        failures.append(f"{label}: no import table (statically linked? check the build)")
        return failures
    if report:
        print(f"\n{label}")
    for dll in dlls:
        kind = classify(dll)
        if kind == "system":
            if report:
                print(f"  {dll:<40} ok")
        elif dll.lower() in shipped:
            if report:
                print(f"  {dll:<40} ok (ships next to the exe)")
        elif kind == "crt":
            if report:
                print(f"  {dll:<40} FAIL  MSVC runtime, shipped nowhere")
            failures.append(
                f"{label}: imports {dll}, which a clean Windows does not have and "
                "the archive does not ship beside the exe"
            )
        else:
            if report:
                print(f"  {dll:<40} FAIL  not a Windows system DLL")
            failures.append(
                f"{label}: imports {dll}, which is neither part of Windows nor "
                "shipped beside the exe"
            )
    return failures


def is_pe_image(data: bytes) -> bool:
    """True when `data` is a PE image the Windows loader could map.

    Closure by file name alone is not closure: a zero-byte or truncated
    vcruntime140.dll in the archive satisfies the name match and the exe still
    dies with "is not a valid Win32 application". The check is deliberately
    shallow (MZ, e_lfanew, PE signature) because anything deeper is a job for
    a real loader.
    """
    if len(data) < 0x40 or data[:2] != b"MZ":
        return False
    offset = struct.unpack_from("<I", data, 0x3C)[0]
    if offset < 4 or offset + 4 > len(data):
        return False
    return data[offset : offset + 4] == b"PE\0\0"


def _dlls_by_dir(names: list[str]) -> dict[str, frozenset[str]]:
    """Map every directory in an archive to the DLL names it carries (lowercased)."""
    by_dir: dict[str, set[str]] = {}
    for name in names:
        if not name.lower().endswith(".dll"):
            continue
        directory, _, base = name.rpartition("/")
        by_dir.setdefault(directory, set()).add(base.lower())
    return {directory: frozenset(bases) for directory, bases in by_dir.items()}


def check_zip(path: Path) -> list[str]:
    failures: list[str] = []
    found = False
    with zipfile.ZipFile(path) as archive:
        names = archive.namelist()
        shipped_by_dir = _dlls_by_dir(names)
        for name in sorted(names):
            if not name.lower().endswith(".dll"):
                continue
            if not is_pe_image(archive.read(name)):
                failures.append(
                    f"{path.name}:{name}: carried as a DLL but is not a PE image"
                )
        for name in sorted(names):
            if not name.lower().endswith(".exe"):
                continue
            found = True
            directory, _, _ = name.rpartition("/")
            failures += check_binary(
                f"{path.name}:{name}",
                archive.read(name),
                shipped_by_dir.get(directory, frozenset()),
            )
    if not found:
        failures.append(f"{path}: no .exe inside, nothing was checked")
    return failures


def check_path(path: Path) -> list[str]:
    if path.suffix.lower() == ".zip":
        return check_zip(path)
    failures = []
    siblings = frozenset()
    for sibling in path.parent.glob("*.dll"):
        if not is_pe_image(sibling.read_bytes()):
            failures.append(f"{sibling.name}: carried as a DLL but is not a PE image")
        siblings |= {sibling.name.lower()}
    return failures + check_binary(path.name, path.read_bytes(), siblings)


def build_minimal_pe(dll_name: str) -> bytes:
    """A one-import PE32+ image, used only by --self-test."""
    buf = bytearray(0x400)
    buf[0:2] = b"MZ"
    struct.pack_into("<I", buf, 0x3C, 0x40)
    buf[0x40:0x44] = b"PE\0\0"
    opt = 0x58
    struct.pack_into("<HHIIIHH", buf, 0x44, 0x8664, 1, 0, 0, 0, 0xF0, 0x22)
    struct.pack_into("<H", buf, opt, 0x20B)
    struct.pack_into("<I", buf, opt + 108, 16)
    struct.pack_into("<II", buf, opt + 120, 0x1000, 0x28)  # data dir 1: import
    sec = opt + 0xF0
    buf[sec : sec + 8] = b".idata\0\0"
    struct.pack_into("<IIII", buf, sec + 8, 0x200, 0x1000, 0x200, 0x200)
    struct.pack_into("<IIIII", buf, 0x200, 0, 0, 0, 0x1040, 0)
    name = dll_name.encode("ascii") + b"\0"
    buf[0x240 : 0x240 + len(name)] = name
    return bytes(buf)


def self_test() -> list[str]:
    problems = []
    clean = imported_dlls(build_minimal_pe("KERNEL32.dll"))
    if clean != ["KERNEL32.dll"]:
        problems.append(f"self-test: parsed {clean!r}, expected ['KERNEL32.dll']")
    elif classify(clean[0]) != "system":
        problems.append("self-test: KERNEL32.dll was not classified as system")
    dirty = imported_dlls(build_minimal_pe("VCRUNTIME140_1.dll"))
    if dirty != ["VCRUNTIME140_1.dll"]:
        problems.append(f"self-test: parsed {dirty!r}, expected the CRT name")
    elif classify(dirty[0]) != "crt":
        problems.append("self-test: VCRUNTIME140_1.dll was not classified as crt")
    if classify("VCOMP140.DLL") != "crt":
        problems.append("self-test: VCOMP140.DLL (uppercase) was not classified as crt")
    if classify("SOMETHINGELSE.dll") != "unknown":
        problems.append("self-test: an unknown DLL passed the allowlist")
    # Closure is the policy now, so both halves of it get pinned here: the same
    # CRT import must fail with nothing shipped and pass once the DLL travels
    # beside the exe. Without this pair the gate could regress to "always fail"
    # (which blocks every release) or "never fail" (which ships 0.5.15 again)
    # and the self-test would still look green.
    crt_pe = build_minimal_pe("VCRUNTIME140.dll")
    if not check_binary("unshipped-crt", crt_pe, frozenset(), report=False):
        problems.append("self-test: a CRT import with nothing shipped passed")
    if check_binary("shipped-crt", crt_pe, frozenset({"vcruntime140.dll"}), report=False):
        problems.append("self-test: a CRT import that ships beside the exe was reported")
    if check_binary("shipped-crt-upper", crt_pe, frozenset({"VCRUNTIME140.DLL"}), report=False):
        problems.append("self-test: shipped DLL matching is case sensitive")
    unknown_pe = build_minimal_pe("SOMETHINGELSE.dll")
    if not check_binary("unshipped-unknown", unknown_pe, frozenset(), report=False):
        problems.append("self-test: an unknown import with nothing shipped passed")
    if check_binary("shipped-unknown", unknown_pe, frozenset({"somethingelse.dll"}), report=False):
        problems.append("self-test: a shipped unknown import was still reported")
    if check_binary("system-only", build_minimal_pe("KERNEL32.dll"), frozenset(), report=False):
        problems.append("self-test: a system-only binary was reported")
    # A name is not a DLL: the same archive that satisfies closure must also
    # carry a real image, or the loader rejects the file the gate blessed.
    if not is_pe_image(build_minimal_pe("vcruntime140.dll")):
        problems.append("self-test: a synthetic PE was not recognised as one")
    for junk in (b"", b"MZ", b"MZ" + b"\0" * 100):
        if is_pe_image(junk):
            problems.append(f"self-test: {junk[:8]!r} was accepted as a PE image")
    return problems


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("paths", nargs="*", type=Path, help=".exe or .zip artifact")
    parser.add_argument("--zip", action="store_true", help="deprecated no-op (suffix decides)")
    parser.add_argument("--self-test", action="store_true", help="exercise the parser in memory")
    args = parser.parse_args(argv)

    failures: list[str] = []
    if args.self_test:
        problems = self_test()
        if problems:
            for problem in problems:
                print(f"self-test FAIL: {problem}")
            return 1
        print("self-test ok: parser, CRT detection and allowlist all behave")

    for path in args.paths:
        if not path.exists():
            failures.append(f"{path}: does not exist")
            continue
        failures += check_path(path)

    if not args.self_test and not args.paths:
        parser.error("give at least one artifact path, or --self-test")

    if failures:
        print("\nFAILED: this artifact will not start on a clean Windows install")
        for failure in dict.fromkeys(failures):
            print(f"  - {failure}")
        return 1
    if args.paths:
        print("\nok: every import is a Windows system DLL or ships beside the exe")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
