#!/usr/bin/env python3
"""Demonstrate P4a archetype B: a regex bytecode stream compiled into one bundle
runs verbatim in another.

This is the evidence behind the **[measured]** claims in
`docs/UNMODELED_REGIONS_PLAN.md` P4a, kept runnable so they can be re-checked
rather than believed. It writes no code into the crate and patches nothing you
own: it compiles two throwaway bundles, moves the regex bytecode from one into
the other with a plain seek-and-write, and runs both on a real Hermes VM.

What it shows, in one run:

  * the stream is position-independent -- a donor compiled for its own bundle
    executes in a different file with no fixups,
  * an entry can be *shrunk* by editing only its `length`; the slack tail of the
    old stream is never read,
  * `.source` and `.flags` do **not** follow the bytecode. They are
    `CreateRegExp`'s string operands, so the patched host matches the donor's
    pattern while still reporting the host's text. That divergence is the trap
    P4a exists to name.

Usage:
    python scripts/regexp_transplant_demo.py --hermes C:/src/hermes-v96/build/bin/Release

    # or point at the pieces individually
    python scripts/regexp_transplant_demo.py --hermesc <path> --hvm <path> --cli <path>

`--hermes` is a directory holding `hermesc` and `hvm`; `scripts/build_hermes_vm.ps1`
produces one per version. Any of v96/v98/v99 works -- the same pattern compiles to
the same bytes at all three, which is the other half of the claim.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile

# The host matches equinox-<n>; the donor matches nope-<n> and is the shorter of
# the two, which is what lets it be written into the host's slot.
HOST_JS = """\
var re = /^equinox-(\\d+)$/i;
print('source:', re.source, 'flags:', re.flags);
print('equinox-42 ->', re.test('equinox-42'));
print('nope-7 ->', re.test('nope-7'));
"""

DONOR_JS = """\
var re = /^nope-(\\d+)$/i;
print(re.test('nope-7'));
"""


def run(args: list[str]) -> str:
    proc = subprocess.run(args, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        raise SystemExit(
            f"command failed ({proc.returncode}): {' '.join(args)}\n{proc.stderr}{proc.stdout}"
        )
    return proc.stdout


def exe(root: pathlib.Path, name: str) -> pathlib.Path:
    for candidate in (root / name, root / f"{name}.exe"):
        if candidate.is_file():
            return candidate
    raise SystemExit(f"no {name} under {root}")


def regexp_entries(cli: pathlib.Path, hbc: pathlib.Path) -> list[dict]:
    return json.loads(run([str(cli), "dump", str(hbc), "--kind", "regexp", "--json"]))


def section(cli: pathlib.Path, hbc: pathlib.Path, want: str) -> tuple[int, int]:
    """(offset, size) of a named section, from `dump --kind sections`."""
    for line in run([str(cli), "dump", str(hbc), "--kind", "sections"]).splitlines():
        parts = line.split()
        if len(parts) >= 3 and parts[0] == want and parts[1].startswith("0x"):
            return int(parts[1], 16), int(parts[2])
    raise SystemExit(f"no {want} section in {hbc.name}")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--hermes", type=pathlib.Path, help="directory holding hermesc and hvm")
    ap.add_argument("--hermesc", type=pathlib.Path)
    ap.add_argument("--hvm", type=pathlib.Path)
    ap.add_argument(
        "--cli",
        type=pathlib.Path,
        default=pathlib.Path(__file__).resolve().parent.parent / "target/debug/hermes-decomp.exe",
        help="this repo's CLI (default: target/debug/hermes-decomp.exe)",
    )
    ap.add_argument("--keep", action="store_true", help="leave the built bundles on disk")
    args = ap.parse_args()

    if args.hermes:
        hermesc = args.hermesc or exe(args.hermes, "hermesc")
        hvm = args.hvm or exe(args.hermes, "hvm")
    elif args.hermesc and args.hvm:
        hermesc, hvm = args.hermesc, args.hvm
    else:
        raise SystemExit("pass --hermes <dir>, or both --hermesc and --hvm")
    if not args.cli.is_file():
        raise SystemExit(f"{args.cli} not built; run `cargo build` first")

    tmp = pathlib.Path(tempfile.mkdtemp(prefix="regexp-transplant-"))
    print(f"working in {tmp}")

    host_js, donor_js = tmp / "host.js", tmp / "donor.js"
    host_js.write_text(HOST_JS, encoding="utf-8")
    donor_js.write_text(DONOR_JS, encoding="utf-8")

    host_hbc, donor_hbc = tmp / "host.hbc", tmp / "donor.hbc"
    run([str(hermesc), "-emit-binary", "-out", str(host_hbc), str(host_js)])
    run([str(hermesc), "-emit-binary", "-out", str(donor_hbc), str(donor_js)])

    host = regexp_entries(args.cli, host_hbc)
    donor = regexp_entries(args.cli, donor_hbc)
    if len(host) != 1 or len(donor) != 1:
        raise SystemExit(f"expected one regex each, got {len(host)} and {len(donor)}")

    payload = bytes.fromhex(donor[0]["bytecode_hex"])
    print(f"host entry:  {host[0]['length']} bytes")
    print(f"donor entry: {len(payload)} bytes")
    if len(payload) > host[0]["length"]:
        raise SystemExit("donor does not fit the host's slot; this demo only covers archetype B")

    # The six-byte header is the contract with the calling code: markedCount,
    # loopCount, syntaxFlags, constraints. Report it rather than assuming it.
    for label, raw in (("host", bytes.fromhex(host[0]["bytecode_hex"])), ("donor", payload)):
        marked = int.from_bytes(raw[0:2], "little")
        loops = int.from_bytes(raw[2:4], "little")
        print(f"  {label}: markedCount={marked} loopCount={loops} syntaxFlags=0x{raw[4]:02x} constraints=0x{raw[5]:02x}")
    if payload[0:2] != bytes.fromhex(host[0]["bytecode_hex"])[0:2]:
        print("  !! capture-group counts differ -- calling code reading m[n] would break")

    print("\n=== host, before ===")
    before = run([str(hvm), str(host_hbc)])
    print(before, end="")

    storage_off, _ = section(args.cli, host_hbc, "regexp_storage")
    table_off, _ = section(args.cli, host_hbc, "regexp_table")

    data = bytearray(host_hbc.read_bytes())
    if hashlib.sha1(bytes(data[:-20])).digest() != bytes(data[-20:]):
        raise SystemExit("footer is not a trailing SHA1; refusing to guess at this file's shape")

    entry_off = host[0]["offset"]
    # 1. the stream itself, into the slot the entry already points at
    data[storage_off + entry_off : storage_off + entry_off + len(payload)] = payload
    # 2. the table entry is {u32 offset, u32 length}; only the length moves
    data[table_off + 4 : table_off + 8] = len(payload).to_bytes(4, "little")
    # 3. the footer
    data[-20:] = hashlib.sha1(bytes(data[:-20])).digest()

    patched = tmp / "host.patched.hbc"
    patched.write_bytes(bytes(data))
    print(f"\npatched {patched.name}: same size as the original: "
          f"{patched.stat().st_size == host_hbc.stat().st_size}")

    print("\n=== host, after transplant ===")
    after = run([str(hvm), str(patched)])
    print(after, end="")

    ok = (
        "equinox-42 -> true" in before
        and "nope-7 -> false" in before
        and "equinox-42 -> false" in after
        and "nope-7 -> true" in after
    )
    print()
    if not ok:
        print("FAIL: the matching behaviour did not swap")
        return 1
    print("the matching behaviour swapped: the donor's stream is running in the host")
    if "^equinox-" in after:
        print("and `.source` still reports the host's pattern -- the divergence P4a warns about")
    if not args.keep:
        print(f"\n(pass --keep to retain {tmp})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
