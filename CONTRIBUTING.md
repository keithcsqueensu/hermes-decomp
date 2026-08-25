# Contributing

> ## 🛑 ⛔ ☠️ This fork does not contribute upstream ☠️ ⛔ 🛑
>
> This repository is a personal fork of
> [SymbioticSec/hermes-decomp](https://github.com/SymbioticSec/hermes-decomp), kept for
> private bytecode-analysis work. **Changes made here are not sent upstream**, and that is
> a standing decision rather than a backlog item.
>
> The work in this fork is heavily AI-assisted. It is tested against real bundles and a real
> Hermes VM, but it is not reviewed to the standard a shared upstream tree should expect, and
> submitting it would move that review burden onto maintainers who did not ask for it.
>
> Practically, for anyone — human or agent — working in this checkout:
>
> - **`origin` (`keithcsqueensu/hermes-decomp`) is the only push target.**
> - **`upstream` is fetch-only.** Its push URL is deliberately set to a non-URL, so
>   `git push upstream` fails rather than succeeding by accident. Do not restore it.
> - **Never open a pull request, issue or discussion against `SymbioticSec/hermes-decomp`** —
>   nothing with `-R SymbioticSec/...`. `gh repo set-default` governs all of them at once and
>   is set to the fork; leave it there. Feature requests and bug reports for work happening
>   here belong on the fork's own tracker.
> - Syncing *from* upstream is fine and expected; that is what the fetch URL is for.
>
> Everything below this line is upstream's own contribution guide, kept unmodified for
> reference. It describes how to contribute to *that* project, from a checkout that is not
> this one.

---

Contributions are welcome.

**Please open an issue first** before submitting a pull request. That avoids
duplicate work and lets us agree on an approach before code is written.

1. [Open an issue](https://github.com/SymbioticSec/hermes-decomp/issues/new) for the bug or feature.
2. Wait for feedback / confirmation that a PR is welcome.
3. Fork the repo and create a branch from `main`.
4. Make your change. Ensure:

   ```bash
   cargo build --release --workspace
   cargo test --workspace
   ```

   CI builds on **Linux**, **macOS**, and **Windows** keep all three green.

5. Open a pull request that references the issue.

## Docs

| File | Content |
|------|---------|
| [README.md](README.md) | Overview, install, quick start |
| [docs/USAGE.md](docs/USAGE.md) | Full CLI reference |
| [docs/MCP.md](docs/MCP.md) | MCP server setup & tools |
| [docs/LIBRARY.md](docs/LIBRARY.md) | Rust crate API |

## Scope notes

- Decompiler output is **best-effort** recovery, not source restoration.
- Bytecode patching (`asm`, `patch-*`, …) does **not** recompile decompiled JS.
