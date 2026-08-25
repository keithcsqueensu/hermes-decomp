#!/usr/bin/env python3
"""Regenerate resources/bytecode/Bytecode<N>.json from a Hermes checkout.

`tests/upstream_pin.rs` asserts that the bundled opcode table still matches
upstream's `BytecodeList.def`. This is the other half of that loop: when a
checkout legitimately moves, this re-derives the table instead of hand-editing
220 entries. Its parse deliberately mirrors `parse_bytecode_list` in that test,
including the `DEFINE_JUMP_n` expansion -- if the two ever disagree, the test is
the authority.

Two things in the file are ours rather than upstream's and are carried over from
the existing table by opcode name:

  * the trailing `S` on string-id operands (`UInt16S`). Same width as `UInt16`;
    it is a semantic marker, and `patch-operand` uses it to find a string id.
  * `IsJump`, which is not derivable -- v96's `SwitchImm` has an `Addr32` operand
    and is deliberately not flagged as a jump.

Usage:
    # Rewrite the v99 table from a checkout, recording the commit it came from.
    python scripts/gen_bytecode_table.py --version 99 \\
        --src C:/src/hermes-v99 --commit $(git -C C:/src/hermes-v99 rev-parse HEAD)

    # Verify only: does the checkout still reproduce the committed table?
    python scripts/gen_bytecode_table.py --version 99 --src C:/src/hermes-v99 --check
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

BS = chr(92)
CRLF = chr(13) + chr(10)
DEF_REL = "include/hermes/BCGen/HBC/BytecodeList.def"
TABLE_DIR = pathlib.Path(__file__).resolve().parent.parent / "crates/hbc-decomp/resources/bytecode"


def strip_comments(src: str) -> str:
    """Drop C block and line comments so they cannot contribute fake matches."""
    out = []
    i, block, line = 0, False, False
    while i < len(src):
        two = src[i : i + 2]
        if block:
            if two == "*/":
                block, i = False, i + 2
                continue
        elif line:
            if src[i] == chr(10):
                line = False
                out.append(chr(10))
        elif two == "/*":
            block, i = True, i + 2
            continue
        elif two == "//":
            line, i = True, i + 2
            continue
        else:
            out.append(src[i])
        i += 1
    return "".join(out)


def is_opcode_name(s: str) -> bool:
    # The file both defines and uses these macros; the definitions contain lines
    # like DEFINE_OPCODE_1(name, Addr8) inside #define DEFINE_JUMP_1(name). Real
    # opcode names are CamelCase, so requiring that drops the macro parameters
    # (`name`, `name##Long`).
    return bool(s) and s[0].isupper() and "#" not in s


def parse_bytecode_list(src_root: pathlib.Path) -> list[tuple[str, list[str]]]:
    """Upstream's opcode list, in file order. Position is the opcode number."""
    text = strip_comments((src_root / DEF_REL).read_text(encoding="utf-8", errors="replace"))
    ops: list[tuple[str, list[str]]] = []
    for raw in text.splitlines():
        stripped = raw.strip()
        open_at = stripped.find("(")
        close_at = stripped.rfind(")")
        if open_at < 0 or close_at < 0:
            continue
        head = stripped[:open_at]
        args = [a.strip() for a in stripped[open_at + 1 : close_at].split(",") if a.strip()]

        if head.startswith("DEFINE_OPCODE_"):
            n = head[len("DEFINE_OPCODE_") :]
            if not n.isdigit() or not args or not is_opcode_name(args[0]):
                continue
            ops.append((args[0], args[1:]))
        elif head.startswith("DEFINE_JUMP_"):
            n = head[len("DEFINE_JUMP_") :]
            if not n.isdigit() or len(args) != 1 or not is_opcode_name(args[0]):
                continue
            # Upstream expands this to a short Addr8 form plus a Long Addr32 one.
            extra = ["Reg8"] * (int(n) - 1)
            ops.append((args[0], ["Addr8"] + extra))
            ops.append((args[0] + "Long", ["Addr32"] + extra))

    if len(ops) <= 100:
        sys.exit(f"parsed only {len(ops)} opcodes from {DEF_REL}; the macro shape probably changed")
    return ops


def carry_over(name: str, upstream: list[str], prev: dict) -> tuple[list[str], bool]:
    """Re-apply our `S` markers and `IsJump` to an upstream-derived entry."""
    old = prev.get(name)
    if old is None:
        return upstream, False
    types = [
        o if o == u + "S" else u
        for u, o in zip(upstream, old["OperandTypes"] + [None] * len(upstream))
    ]
    return types, old["IsJump"]


def detect_style(raw: bytes) -> dict:
    """How the existing file is formatted.

    The three committed tables do not agree: Bytecode96.json is tab-indented,
    Bytecode98.json is 2-space with a trailing newline and carries neither
    optional key, Bytecode99.json is 2-space with no trailing newline. Rewriting
    one must not reformat it, or every regeneration produces a diff that is
    entirely whitespace and `--check` reports drift that is not there.
    """
    text = raw.decode("utf-8")
    newline = CRLF if CRLF in text else chr(10)
    lines = text.split(newline)
    indent = "  "
    if len(lines) > 1:
        lead = lines[1][: len(lines[1]) - len(lines[1].lstrip())]
        if lead:
            indent = lead
    return {
        "newline": newline,
        "indent": indent,
        "trailing_newline": text.endswith(newline),
    }


def build(version: int, src_root: pathlib.Path, commit: str | None, prev_raw: bytes) -> bytes:
    prev_doc = json.loads(prev_raw)
    prev = {e["Name"]: e for e in prev_doc["Definitions"]}
    style = detect_style(prev_raw)

    definitions = []
    for opcode, (name, upstream) in enumerate(parse_bytecode_list(src_root)):
        types, is_jump = carry_over(name, upstream, prev)
        entry = {"Opcode": opcode, "Name": name, "OperandTypes": types, "IsJump": is_jump}
        # Bytecode96.json carries a fifth per-entry key, "AbstractDefinition", that
        # the later tables dropped -- and carries it on only some of its entries
        # (opcode 0, Unreachable, has none). Like IsJump it is not derivable from
        # BytecodeList.def, so carry it per opcode, exactly where it already is.
        old_entry = prev.get(name)
        if old_entry is not None and "AbstractDefinition" in old_entry:
            entry["AbstractDefinition"] = old_entry["AbstractDefinition"]
        definitions.append(entry)

    doc: dict = {"Version": version, "Definitions": definitions}
    # Only keys the file already had -- except GitCommitHash, which appears as soon
    # as a caller records one.
    if "AbstractDefinitions" in prev_doc:
        doc["AbstractDefinitions"] = prev_doc["AbstractDefinitions"]
    if commit or "GitCommitHash" in prev_doc:
        doc["GitCommitHash"] = commit or prev_doc.get("GitCommitHash", "")

    body = json.dumps(doc, indent=style["indent"])
    if style["trailing_newline"]:
        body += chr(10)
    return body.replace(chr(10), style["newline"]).encode()


def main() -> int | str:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--version", type=int, required=True, help="bytecode version, e.g. 99")
    ap.add_argument("--src", required=True, help="path to a Hermes checkout")
    ap.add_argument("--commit", help="commit hash to record in GitCommitHash")
    ap.add_argument(
        "--check",
        action="store_true",
        help="do not write; exit non-zero if the checkout does not reproduce the table",
    )
    args = ap.parse_args()

    src_root = pathlib.Path(args.src)
    if not (src_root / DEF_REL).is_file():
        return f"no {DEF_REL} under {src_root}"

    out_path = TABLE_DIR / f"Bytecode{args.version}.json"
    prev_raw = out_path.read_bytes()
    new_raw = build(args.version, src_root, args.commit, prev_raw)

    if new_raw == prev_raw:
        print(f"{out_path.name}: unchanged ({len(json.loads(new_raw)['Definitions'])} opcodes)")
        return 0

    old = {e["Name"]: e for e in json.loads(prev_raw)["Definitions"]}
    new = {e["Name"]: e for e in json.loads(new_raw)["Definitions"]}
    print(f"{out_path.name}: {len(old)} -> {len(new)} opcodes")
    for name in dict.fromkeys(list(old) + list(new)):
        was, now = old.get(name), new.get(name)
        if was is None and now is not None:
            print(f"  + {name} @ {now['Opcode']} {now['OperandTypes']}")
        elif now is None and was is not None:
            print(f"  - {name} (was @ {was['Opcode']})")
        elif was is not None and now is not None and (
            was["OperandTypes"] != now["OperandTypes"] or was["Opcode"] != now["Opcode"]
        ):
            print(
                f"  ~ {name}: @{was['Opcode']} {was['OperandTypes']}"
                f" -> @{now['Opcode']} {now['OperandTypes']}"
            )

    if args.check:
        return "checkout does not reproduce the committed table"
    out_path.write_bytes(new_raw)
    print(f"wrote {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
