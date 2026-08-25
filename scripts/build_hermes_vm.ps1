<#
.SYNOPSIS
Build a version-matched Hermes VM (`hvm`) and compiler (`hermesc`) for verifying
hbc-decomp's write path against a real engine.

.DESCRIPTION
`tests/vm_verify.rs` asserts that patched bytecode *runs*, not merely that it
reparses. That needs an actual Hermes VM, and an `hvm` only accepts its own
bytecode version:

    > hvm.exe file-v96.hbc
    Wrong bytecode version. Expected 99 but got 96

so there is one binary per version, not one binary. This script builds them from
a facebook/hermes checkout using `git worktree`, which leaves the original
checkout untouched.

Each version needs small source patches to build with a current MSVC; they are
applied here, idempotently, and each is explained at its call site in
Apply-Patches. None of them change bytecode semantics -- they are portability
fixes for a toolchain upstream does not test against on Windows.

.PARAMETER Version
Bytecode version to build: 96, 98 or 99.

.PARAMETER HermesRepo
Path to an existing facebook/hermes clone with full history. Must contain the
`static_h` branch and the `origin/*-stable` release branches.

.PARAMETER WorktreeRoot
Where to create the per-version worktrees. Defaults to a sibling of HermesRepo.

.PARAMETER Fixtures
After building, recompile the test fixtures in
crates/hbc-decomp/tests/fixtures for this version.

.EXAMPLE
./scripts/build_hermes_vm.ps1 -Version 96 -HermesRepo C:\src\hermes-src

.EXAMPLE
# Build all three, then regenerate every fixture.
96, 98, 99 | ForEach-Object {
    ./scripts/build_hermes_vm.ps1 -Version $_ -HermesRepo C:\src\hermes-src -Fixtures
}

.NOTES
Name the clone something outside the `hermes-v<N>` pattern -- the worktrees claim
those names. A clone at C:\src\hermes-v99 collides with the worktree for v99; the
script detects that and refuses rather than building in the clone.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet(96, 98, 99)][int]$Version,
    [Parameter(Mandatory)][string]$HermesRepo,
    [string]$WorktreeRoot,
    [switch]$Fixtures
)

$ErrorActionPreference = 'Stop'

# Which upstream ref provides each bytecode version.
#
# NOTE: the version integer does NOT identify the header layout on its own --
# upstream has changed the modern function-header shape twice without bumping the
# version (see crates/hbc-decomp/src/modern_layout.rs). These refs are the ones
# ModernLayout was derived from, so keep the two in step.
#
# The same goes for the opcode tables: `tests/upstream_pin.rs` parses
# BytecodeList.def out of each checkout and compares it against
# resources/bytecode/Bytecode<N>.json, so a ref here that is not the commit that
# JSON records in `GitCommitHash` makes the pin test fail by construction.
$Refs = @{
    96 = @{ Ref = '2afc7b09f'; Note = 'last commit before the v97 bump; RN 0.7x-era' }
    98 = @{ Ref = 'origin/250829098.0.0-stable'; Note = 'React Native shipped v98' }
    # The release branch, NOT static_h. Both declare BYTECODE_VERSION 99 and their
    # BytecodeFileFormat.h is byte-identical, so the header layout cannot tell them
    # apart -- but static_h carries a later `NewFastArray` that took a third operand
    # (upstream d4f5193f0), which this branch does not:
    #
    #     stable   DEFINE_OPCODE_2(NewFastArray, Reg8, UInt16)        <- 4 bytes
    #     static_h DEFINE_OPCODE_3(NewFastArray, Reg8, Reg8, UInt16)  <- 5 bytes
    #
    # React Native ships from the release branch, so the 2-operand form is what
    # real v99 bundles contain and what Bytecode99.json therefore encodes.
    99 = @{ Ref = 'origin/260318099.0.0-stable'; Note = 'React Native shipped v99' }
}

function Find-CMake {
    $cmd = Get-Command cmake -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    # Visual Studio bundles one; prefer the newest it finds.
    $bundled = Get-ChildItem -Path 'C:\Program Files\Microsoft Visual Studio' -Recurse `
        -Filter cmake.exe -ErrorAction SilentlyContinue |
        Where-Object FullName -like '*CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe' |
        Sort-Object FullName -Descending | Select-Object -First 1
    if ($bundled) { return $bundled.FullName }
    throw 'cmake not found on PATH or under Visual Studio. Install CMake or the VS "C++ CMake tools" component.'
}

function Edit-File {
    param([string]$Path, [string]$Find, [string]$Replace, [string]$Why)
    $text = [IO.File]::ReadAllText($Path)
    if ($text.Contains($Replace)) {
        Write-Host "    already patched: $Why" -ForegroundColor DarkGray
        return
    }
    if (-not $text.Contains($Find)) {
        throw "patch target not found in $Path ($Why). Upstream may have moved; re-derive the patch."
    }
    [IO.File]::WriteAllText($Path, $text.Replace($Find, $Replace))
    Write-Host "    patched: $Why" -ForegroundColor DarkGray
}

function Apply-Patches {
    param([string]$Tree, [int]$Version)

    Write-Host "  applying build patches" -ForegroundColor Cyan

    if ($Version -eq 96) {
        # CMake 4 removed CMP0026-OLD. Per the comment upstream it exists only for
        # Apple dSYM bundles, which a Windows host build does not produce.
        Edit-File -Path (Join-Path $Tree 'CMakeLists.txt') `
            -Find "if (POLICY CMP0026)`n  cmake_policy(SET CMP0026 OLD)`nendif()" `
            -Replace "if (POLICY CMP0026 AND CMAKE_VERSION VERSION_LESS 4.0)`n  cmake_policy(SET CMP0026 OLD)`nendif()" `
            -Why 'CMP0026 OLD was removed in CMake 4'

        # `union { Storage storage; T val; } ret{};` makes a current MSVC demand a
        # default constructor for the variant member T (CompressedPointer has
        # none). Initialising the first member explicitly is equivalent.
        Edit-File -Path (Join-Path $Tree 'lib/VM/gcs/HadesGC.cpp') `
            -Find "    } ret{};" -Replace "    } ret{0};" `
            -Why 'MSVC rejects value-init of a union with a non-default-constructible member'
    }

    if ($Version -eq 98) {
        # Two calls bypass the project's own SH_LIKELY macro, which already has an
        # MSVC fallback in sh_config.h. Route them through it.
        $sh = Join-Path $Tree 'include/hermes/VM/static_h.h'
        Edit-File -Path $sh `
            -Find 'if (__builtin_expect(sh_tryfast_f64_to_i64(d, fast), 1))' `
            -Replace 'if (SH_LIKELY(sh_tryfast_f64_to_i64(d, fast)))' `
            -Why '__builtin_expect is not an MSVC builtin (i64 path)'
        Edit-File -Path $sh `
            -Find 'if (__builtin_expect(sh_tryfast_f64_to_i32(d, fast), 1))' `
            -Replace 'if (SH_LIKELY(sh_tryfast_f64_to_i32(d, fast)))' `
            -Why '__builtin_expect is not an MSVC builtin (i32 path)'

        # The sampling profiler calls timeBeginPeriod/timeEndPeriod, which live in
        # winmm. Upstream never builds this tool on Windows so it is not declared.
        Edit-File -Path (Join-Path $Tree 'tools/hvm/CMakeLists.txt') `
            -Find "  LINK_LIBS hermesvm_a)" `
            -Replace "  LINK_LIBS hermesvm_a `${HVM_EXTRA_LIBS})" `
            -Why 'hvm needs winmm for the sampling profiler (declaration)'
        Edit-File -Path (Join-Path $Tree 'tools/hvm/CMakeLists.txt') `
            -Find "add_hermes_tool(hvm" `
            -Replace "if (MSVC)`n  set(HVM_EXTRA_LIBS winmm)`nendif()`n`nadd_hermes_tool(hvm" `
            -Why 'hvm needs winmm for the sampling profiler (definition)'
    }

    if ($Version -eq 99) {
        Write-Host "    none needed" -ForegroundColor DarkGray
    }
}

# --- main -------------------------------------------------------------------

$HermesRepo = (Resolve-Path $HermesRepo).Path
if (-not $WorktreeRoot) { $WorktreeRoot = Split-Path -Parent $HermesRepo }
$tree = Join-Path $WorktreeRoot "hermes-v$Version"
$ref = $Refs[$Version].Ref

# Normalise before comparing: $tree is built by string join and may not exist yet,
# so Resolve-Path is not available for it.
$norm = { param($p) [IO.Path]::GetFullPath($p).TrimEnd([IO.Path]::DirectorySeparatorChar) }
$treeFull = & $norm $tree
$repoFull = & $norm $HermesRepo

Write-Host "Building Hermes v$Version" -ForegroundColor Green
Write-Host "  ref       $ref  ($($Refs[$Version].Note))"
Write-Host "  worktree  $tree"

# The clone itself must never be used as the worktree. The default worktree name
# is "hermes-v<N>", so a clone that happens to be named that -- C:\src\hermes-v99,
# say -- collides with its own worktree path. Left unguarded that lands in the
# "exists, reusing" branch below and silently builds whatever ref the clone is
# checked out at, in the clone, which also breaks this script's promise to leave
# the original checkout untouched.
if ($treeFull -ieq $repoFull) {
    throw @"
Worktree path for v$Version is the clone itself: $treeFull

-WorktreeRoot defaults to the clone's parent and the worktree is named
hermes-v$Version, so a clone named hermes-v$Version collides with it. Either

  * pass -WorktreeRoot <dir> to put the worktree elsewhere, or
  * skip this build -- if the clone is already at $ref, its own
    build\bin\Release already holds the v$Version binaries.

Building in the clone is not an option: this script patches sources in the tree.
"@
}

if (-not (Test-Path $tree)) {
    Write-Host "  creating worktree" -ForegroundColor Cyan
    git -C $HermesRepo worktree add --detach $tree $ref
    if ($LASTEXITCODE -ne 0) { throw "git worktree add failed for $ref" }
} else {
    # Reuse is only safe if the tree is actually at the ref this version means.
    # A stale worktree from an earlier $Refs entry looks identical from the
    # outside and produces a binary for the wrong commit, which is exactly the
    # class of drift upstream_pin exists to catch -- catch it here too, before
    # spending several minutes building the wrong thing.
    $want = (git -C $HermesRepo rev-parse --verify "$ref^{commit}" 2>&1)
    if ($LASTEXITCODE -ne 0) { throw "cannot resolve $ref in ${HermesRepo}: $want" }
    $have = (git -C $tree rev-parse --verify HEAD 2>&1)
    if ($LASTEXITCODE -ne 0) { throw "$tree exists but is not a git worktree: $have" }
    if ($want -ne $have) {
        throw @"
Worktree $tree is at $have, but v$Version means $ref ($want).

Remove it (git -C $HermesRepo worktree remove --force $tree) and re-run, or pass
-WorktreeRoot <dir> to build alongside it.
"@
    }
    Write-Host "  worktree exists at $ref, reusing" -ForegroundColor DarkGray
}

Apply-Patches -Tree $tree -Version $Version

$cmake = Find-CMake
Write-Host "  cmake     $cmake" -ForegroundColor DarkGray

$build = Join-Path $tree 'build'

# Keep the output instead of discarding it: on failure the compiler diagnostic is
# the only thing that says what went wrong, and 'cmake build failed' on its own
# sends you back to re-run the build by hand to find out.
function Invoke-CMake {
    param([string]$What, [string[]]$CMakeArgs)
    $log = Join-Path $build "$What.log"
    $out = & $cmake @CMakeArgs 2>&1
    if ($LASTEXITCODE -ne 0) {
        New-Item -ItemType Directory -Force -Path $build | Out-Null
        $out | Out-File -FilePath $log -Encoding utf8
        $out | Select-Object -Last 40 | ForEach-Object { Write-Host "    $_" -ForegroundColor DarkRed }
        throw "cmake $What failed (full log: $log)"
    }
}

Write-Host "  configuring" -ForegroundColor Cyan
Invoke-CMake configure @(
    '-S', $tree, '-B', $build, '-G', 'Visual Studio 17 2022', '-A', 'x64',
    '-DCMAKE_BUILD_TYPE=Release', '-DHERMES_ENABLE_DEBUGGER=OFF'
)

Write-Host "  building hvm + hermesc (several minutes)" -ForegroundColor Cyan
# '-v:m' must be quoted. Unquoted, PowerShell reads `-v:` as a parameter name with
# `m` as its argument and passes MSBuild two tokens -- `-v:` and `m` -- which it
# rejects with "MSB1016: Specify the verbosity level". cmake already passes /v:m
# of its own accord, so this only ever mattered as a way to break the build.
Invoke-CMake build @('--build', $build, '--config', 'Release', '--target', 'hvm', 'hermesc', '--', '-m', '-v:m')

$hvm = Join-Path $build 'bin\Release\hvm.exe'
$hermesc = Join-Path $build 'bin\Release\hermesc.exe'
foreach ($exe in $hvm, $hermesc) {
    if (-not (Test-Path $exe)) { throw "expected binary was not produced: $exe" }
}

# Prove it round-trips before declaring success: compile a trivial script and run
# it. A binary that exists but rejects its own output is worse than none.
$tmp = Join-Path ([IO.Path]::GetTempPath()) "hermes-v$Version-smoke"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$js = Join-Path $tmp 'smoke.js'
$hbc = Join-Path $tmp 'smoke.hbc'
Set-Content -Path $js -Value 'print("ok");'
& $hermesc -emit-binary -out $hbc $js
$out = (& $hvm $hbc) -join ''
if ($out.Trim() -ne 'ok') { throw "smoke test failed: expected 'ok', got '$out'" }
Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
Write-Host "  smoke test passed" -ForegroundColor Green

if ($Fixtures) {
    # NOT $fixtures: PowerShell variable names are case-insensitive, so that is the
    # same variable as the [switch]$Fixtures parameter, and assigning a path to it
    # fails the parameter's type constraint ("Cannot convert ... to SwitchParameter")
    # after the build has already succeeded.
    $fixtureDir = Join-Path $PSScriptRoot '..\crates\hbc-decomp\tests\fixtures'
    $fixtureDir = (Resolve-Path $fixtureDir).Path
    Write-Host "  recompiling fixtures in $fixtureDir" -ForegroundColor Cyan
    # Compile from inside the fixture directory, passing a bare filename. hermesc
    # records the input path it was given verbatim in the output, so building with
    # $_.FullName bakes the builder's absolute checkout path into a committed test
    # fixture -- machine-specific bytes that nobody else can reproduce, and a
    # whole-file diff whenever someone regenerates from a different directory.
    Push-Location $fixtureDir
    try {
        Get-ChildItem -Path $fixtureDir -Filter '*.js' | ForEach-Object {
            $dest = "$($_.BaseName).v$Version.hbc"
            & $hermesc -emit-binary -out $dest $_.Name
            if ($LASTEXITCODE -ne 0) { throw "hermesc failed on $($_.Name)" }
            Write-Host "    $dest" -ForegroundColor DarkGray
        }
    } finally {
        Pop-Location
    }
}

Write-Host ''
Write-Host 'Done. To run the VM-backed tests, set:' -ForegroundColor Green
Write-Host "  `$env:HERMES_VM_V$Version = '$hvm'"
Write-Host '  cargo test --test vm_verify'
