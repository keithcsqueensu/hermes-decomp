#!/usr/bin/env python3
"""Materialize the Hermes checkouts `tests/upstream_pin.rs` compares against.

The commit for each version is not a constant here -- it is read from
`resources/bytecode/Bytecode<N>.json`'s `GitCommitHash`, which is the same field
`tables_record_the_commit_they_came_from` enforces. So a repin (see
`scripts/gen_bytecode_table.py`) moves what this fetches, with no second list to
keep in step.

Each checkout is a real git tree at that exact commit -- the pin runs
`git rev-parse HEAD` and requires it to *be* the recorded commit -- but a cheap
one: a blobless partial clone (`--filter=blob:none`), fetched by sha at depth 1,
sparse-checked-out to the one directory the tests read. That is ~0.5 MB and about
a second per version, against ~1.5 GB for a full clone, which is what makes
running these in CI on every push practical.

`scripts/build_hermes_vm.ps1` is the other way to get these trees: worktrees of a
full clone, with a built VM beside them. Use that one when you need `hvm` or
`hbcdump`; this one when you only need to check the format.

Usage:
    python scripts/fetch_pinned_hermes.py <dest-dir> [--versions 96,97,98,99]
    HBC_REQUIRE_ORACLES=src cargo test -p hbc-decomp --test upstream_pin
"""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import sys

REMOTE = "https://github.com/facebook/hermes.git"
# The one directory the pin reads: BytecodeFileFormat.h, BytecodeVersion.h,
# BytecodeList.def. Widening this costs blob downloads, so widen it only when a
# test actually needs another file.
SPARSE = "include/hermes/BCGen/HBC"
TABLE_DIR = pathlib.Path(__file__).resolve().parent.parent / "crates/hbc-decomp/resources/bytecode"
DEFAULT_VERSIONS = [96, 97, 98, 99]


def pinned_commit(version: int) -> str:
    path = TABLE_DIR / f"Bytecode{version}.json"
    doc = json.loads(path.read_text(encoding="utf-8-sig"))
    commit = doc.get("GitCommitHash")
    if not commit or len(commit) != 40:
        raise SystemExit(
            f"{path.name} records no usable GitCommitHash ({commit!r}). Regenerate it with "
            f"scripts/gen_bytecode_table.py --commit <sha>."
        )
    return commit


def git(*args: str, cwd: pathlib.Path | None = None) -> None:
    proc = subprocess.run(
        ["git", *args],
        cwd=cwd,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise SystemExit(
            f"git {' '.join(args)} failed ({proc.returncode}):\n{proc.stdout}{proc.stderr}"
        )


def head_of(tree: pathlib.Path) -> str | None:
    proc = subprocess.run(
        ["git", "-C", str(tree), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
    )
    return proc.stdout.strip() if proc.returncode == 0 else None


def fetch(version: int, commit: str, tree: pathlib.Path) -> str:
    """Leave `tree` checked out at `commit`. Idempotent -- reuses a tree already there."""
    if head_of(tree) == commit:
        return "already at the pinned commit"

    if tree.exists() and any(tree.iterdir()):
        raise SystemExit(
            f"{tree} exists and is not at {commit[:9]}. Remove it, or point --dest elsewhere."
        )

    tree.mkdir(parents=True, exist_ok=True)
    git("init", "-q", ".", cwd=tree)
    git("remote", "add", "origin", REMOTE, cwd=tree)
    # By sha, so this cannot drift with a branch; blobless + depth 1 so it does not
    # pay for history it will never read.
    git("fetch", "-q", "--depth", "1", "--filter=blob:none", "origin", commit, cwd=tree)
    git("sparse-checkout", "set", "--cone", SPARSE, cwd=tree)
    git("checkout", "-q", "--detach", "FETCH_HEAD", cwd=tree)

    got = head_of(tree)
    if got != commit:
        raise SystemExit(f"v{version}: asked for {commit}, checkout is at {got}")
    return "fetched"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("dest", help="directory to create hermes-v<N> trees under")
    ap.add_argument(
        "--versions",
        default=",".join(str(v) for v in DEFAULT_VERSIONS),
        help="comma-separated bytecode versions (default: 96,97,98,99)",
    )
    args = ap.parse_args()

    dest = pathlib.Path(args.dest).resolve()
    versions = [int(v) for v in args.versions.split(",") if v.strip()]

    env_lines = []
    for version in versions:
        commit = pinned_commit(version)
        tree = dest / f"hermes-v{version}"
        what = fetch(version, commit, tree)
        print(f"v{version}: {commit[:9]} -> {tree} ({what})")
        env_lines.append(f"HERMES_SRC_V{version}={tree}")

    print("\n# Set these, then the pin asserts instead of skipping:")
    for line in env_lines:
        print(line)
    print("HBC_REQUIRE_ORACLES=src")
    return 0


if __name__ == "__main__":
    sys.exit(main())
