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
./scripts/build_hermes_vm.ps1 -Version 96 -HermesRepo C:\src\hermes

.EXAMPLE
# Build all three, then regenerate every fixture.
96, 98, 99 | ForEach-Object {
    ./scripts/build_hermes_vm.ps1 -Version $_ -HermesRepo C:\src\hermes -Fixtures
}
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
$Refs = @{
    96 = @{ Ref = '2afc7b09f'; Note = 'last commit before the v97 bump; RN 0.7x-era' }
    98 = @{ Ref = 'origin/250829098.0.0-stable'; Note = 'React Native shipped v98' }
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

Write-Host "Building Hermes v$Version" -ForegroundColor Green
Write-Host "  ref       $ref  ($($Refs[$Version].Note))"
Write-Host "  worktree  $tree"

if (-not (Test-Path $tree)) {
    Write-Host "  creating worktree" -ForegroundColor Cyan
    git -C $HermesRepo worktree add --detach $tree $ref
    if ($LASTEXITCODE -ne 0) { throw "git worktree add failed for $ref" }
} else {
    Write-Host "  worktree exists, reusing" -ForegroundColor DarkGray
}

Apply-Patches -Tree $tree -Version $Version

$cmake = Find-CMake
Write-Host "  cmake     $cmake" -ForegroundColor DarkGray

$build = Join-Path $tree 'build'
Write-Host "  configuring" -ForegroundColor Cyan
& $cmake -S $tree -B $build -G 'Visual Studio 17 2022' -A x64 `
    -DCMAKE_BUILD_TYPE=Release -DHERMES_ENABLE_DEBUGGER=OFF | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'cmake configure failed' }

Write-Host "  building hvm + hermesc (several minutes)" -ForegroundColor Cyan
& $cmake --build $build --config Release --target hvm hermesc -- -m -v:m | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'cmake build failed' }

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
    $fixtures = Join-Path $PSScriptRoot '..\crates\hbc-decomp\tests\fixtures'
    $fixtures = (Resolve-Path $fixtures).Path
    Write-Host "  recompiling fixtures in $fixtures" -ForegroundColor Cyan
    Get-ChildItem -Path $fixtures -Filter '*.js' | ForEach-Object {
        $out = Join-Path $fixtures "$($_.BaseName).v$Version.hbc"
        & $hermesc -emit-binary -out $out $_.FullName
        if ($LASTEXITCODE -ne 0) { throw "hermesc failed on $($_.Name)" }
        Write-Host "    $($_.BaseName).v$Version.hbc" -ForegroundColor DarkGray
    }
}

Write-Host ''
Write-Host 'Done. To run the VM-backed tests, set:' -ForegroundColor Green
Write-Host "  `$env:HERMES_VM_V$Version = '$hvm'"
Write-Host '  cargo test --test vm_verify'
