<#
.SYNOPSIS
    Sets up Field Stitch from source: native libraries, the Rust build config, the frontend packages.

.DESCRIPTION
    1. Checks the prerequisites and lists anything missing (it never installs them).
    2. Opens a Visual Studio developer shell.
    3. vcpkg installs the libraries from vcpkg.json (OpenCV) into vcpkg_env\installed.
       The first run builds OpenCV from source: expect 20-40+ minutes.
    4. Writes src-tauri\.cargo\config.toml, which tells cargo where OpenCV, libclang
       and the MSVC headers/libs are. An existing file is kept as config.toml.bak.
    5. Installs the frontend's npm packages (npm ci).
    6. Runs cargo check, which also generates the OpenCV bindings (a few minutes the first time).

    Safe to run again: finished steps are quick the second time.

    Why vcpkg_env\ and not vcpkg_installed\: the Rust vcpkg crate (used by the opencv crate)
    only reads a classic vcpkg layout, <VCPKG_ROOT>\.vcpkg-root plus <VCPKG_ROOT>\installed\.
    vcpkg_env\ is laid out that way, so cargo's VCPKG_ROOT points there, not at Visual Studio's vcpkg.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\setup.ps1
.EXAMPLE
    .\setup.ps1 -LlvmBin 'C:\Program Files\LLVM\bin'
#>
[CmdletBinding()]
param(
    # Folder holding libclang.dll and clang.exe. Default: Visual Studio's own clang, then C:\Program Files\LLVM\bin.
    [string]$LlvmBin,
    # Skip npm ci.
    [switch]$SkipNpm,
    # Skip the final cargo check.
    [switch]$SkipCheck,
    # Delete vcpkg_env\ and src-tauri\target\ first, then rebuild everything.
    [switch]$Clean
)

$ErrorActionPreference = 'Stop'
$Repo = $PSScriptRoot
$Tauri = Join-Path $Repo 'src-tauri'
$VcpkgEnv = Join-Path $Repo 'vcpkg_env'
$Triplet = 'x64-windows'
$MinNodeMajor = 20

function Write-Step([string]$Text) { Write-Host ''; Write-Host "==> $Text" -ForegroundColor Cyan }
function Write-Ok([string]$Text) { Write-Host "    ok    $Text" -ForegroundColor Green }
function Write-Bad([string]$Text) { Write-Host "    MISSING $Text" -ForegroundColor Red }
function Write-Warn([string]$Text) { Write-Host "    note  $Text" -ForegroundColor Yellow }

# Windows PowerShell 5.1 turns a native program's redirected stderr into a terminating error
# when $ErrorActionPreference is Stop, so native programs run with Continue and are judged
# by their exit code instead.

# Runs a native command and stops the script when it fails.
function Invoke-Checked([string]$What, [scriptblock]$Command) {
    $old = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $Command } finally { $ErrorActionPreference = $old }
    if ($LASTEXITCODE -ne 0) { throw "$What failed (exit code $LASTEXITCODE)." }
}

# Runs a native command with its stderr discarded and returns its output.
function Invoke-Quiet([scriptblock]$Command) {
    $old = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $Command 2>$null } finally { $ErrorActionPreference = $old }
}

# A TOML literal string can't contain a single quote; none of these paths should.
function ConvertTo-TomlLiteral([string]$Value) {
    if ($Value.Contains("'")) { throw "Can't write a path containing a single quote to config.toml: $Value" }
    "'$Value'"
}

Set-Location $Repo

# ---------------------------------------------------------------- 1. prerequisites
Write-Step 'Checking prerequisites'
$missing = @()

if (Get-Command git -ErrorAction SilentlyContinue) { Write-Ok 'Git' }
else { $missing += 'Git (vcpkg downloads its package definitions with it): https://git-scm.com/download/win' }

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vsPath = $null
if (Test-Path $vswhere) {
    $vsPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
}
if ($vsPath) {
    Write-Ok "Visual Studio C++ tools ($vsPath)"
    if (-not $env:VCPKG_ROOT -and -not (Test-Path (Join-Path $vsPath 'VC\vcpkg\vcpkg.exe'))) {
        $missing += 'Visual Studio component "vcpkg package manager" (Visual Studio Installer > Modify > Individual components), or set VCPKG_ROOT to your own vcpkg clone'
    }
} else {
    $missing += 'Visual Studio 2022 or later (Community or Build Tools) with the "Desktop development with C++" workload: https://visualstudio.microsoft.com/downloads/'
}

# libclang: the opencv crate's binding generator parses OpenCV's headers with it.
$llvmCandidates = @()
if ($LlvmBin) { $llvmCandidates += $LlvmBin }
else {
    if ($vsPath) { $llvmCandidates += (Join-Path $vsPath 'VC\Tools\Llvm\x64\bin') }
    $llvmCandidates += (Join-Path $env:ProgramFiles 'LLVM\bin')
}
$llvm = $llvmCandidates | Where-Object { (Test-Path (Join-Path $_ 'libclang.dll')) -and (Test-Path (Join-Path $_ 'clang.exe')) } | Select-Object -First 1
if ($llvm) { Write-Ok "libclang ($llvm)" }
elseif ($LlvmBin) { $missing += "libclang.dll and clang.exe in $LlvmBin" }
else { $missing += 'libclang: Visual Studio component "C++ Clang Compiler for Windows" (Visual Studio Installer > Modify > Individual components), or LLVM from https://github.com/llvm/llvm-project/releases (or pass -LlvmBin <folder>)' }

$node = Get-Command node -ErrorAction SilentlyContinue
if ($node) {
    $nodeVersion = (& node -v).TrimStart('v')
    if ([int]($nodeVersion.Split('.')[0]) -ge $MinNodeMajor) { Write-Ok "Node.js $nodeVersion" }
    else { $missing += "Node.js $MinNodeMajor or later (found $nodeVersion): https://nodejs.org/" }
} else {
    $missing += 'Node.js (LTS): https://nodejs.org/'
}
if (Get-Command cargo -ErrorAction SilentlyContinue) { Write-Ok ((& cargo -V) -join '') }
else { $missing += 'Rust: https://rustup.rs/' }

# The Tauri build refuses to start without the bundled resources.
foreach ($model in @('superpoint.onnx', 'superpoint_lightglue.trt.onnx')) {
    if (Test-Path (Join-Path $Tauri "resources\$model")) { Write-Ok "resources\$model" }
    else { $missing += "src-tauri\resources\$model (not in git; contact the developer, see src-tauri\resources\README.md)" }
}

if ($missing.Count -gt 0) {
    Write-Host ''
    Write-Host 'Install these, then run setup.ps1 again:' -ForegroundColor Yellow
    foreach ($m in $missing) { Write-Bad $m }
    exit 1
}

$envFile = Join-Path $Repo '.env'
if (-not ((Test-Path $envFile) -and (Select-String -Path $envFile -Pattern '^\s*PROTOMAP_KEY\s*=\s*\S' -Quiet))) {
    Write-Warn 'No PROTOMAP_KEY in .env: the app builds, but offline map downloads will fail (see .env.example).'
}
if ($env:OPENCV_PACKAGE_NAME) {
    Write-Warn "OPENCV_PACKAGE_NAME=$env:OPENCV_PACKAGE_NAME is set on this machine; config.toml overrides it for cargo."
}

# ---------------------------------------------------------------- 2. developer shell
Write-Step 'Visual Studio developer shell'
$userVcpkgRoot = $env:VCPKG_ROOT
# The dev shell's batch scripts call vswhere.exe by name and print an error when it isn't on PATH.
$env:PATH = "$(Split-Path $vswhere);$env:PATH"
# The dev shell script writes harmless noise to stderr.
Invoke-Quiet { & (Join-Path $vsPath 'Common7\Tools\Launch-VsDevShell.ps1') -Arch amd64 -HostArch amd64 -SkipAutomaticLocation } | Out-Null
if ($userVcpkgRoot) { $env:VCPKG_ROOT = $userVcpkgRoot }   # keep a vcpkg the user chose
Set-Location $Repo
if (-not (Get-Command cl -ErrorAction SilentlyContinue)) { throw 'The developer shell did not provide the C++ compiler (cl.exe).' }
$vcpkgExe = Join-Path $env:VCPKG_ROOT 'vcpkg.exe'
if (-not (Test-Path $vcpkgExe)) { throw "No vcpkg.exe in VCPKG_ROOT ($env:VCPKG_ROOT)." }
Write-Ok "compiler ready; vcpkg: $env:VCPKG_ROOT"

# ---------------------------------------------------------------- 3. native libraries
if ($Clean) {
    Write-Step 'Cleaning'
    if (Test-Path $VcpkgEnv) { Remove-Item -Recurse -Force $VcpkgEnv; Write-Ok 'removed vcpkg_env\' }
    $target = Join-Path $Tauri 'target'
    if (Test-Path $target) { Remove-Item -Recurse -Force $target; Write-Ok 'removed src-tauri\target\' }
}

Write-Step "Installing vcpkg.json libraries into vcpkg_env\installed ($Triplet)"
Write-Host '    the first run builds OpenCV from source (20-40+ minutes); later runs take seconds'
$installRoot = Join-Path $VcpkgEnv 'installed'
Invoke-Checked 'vcpkg install' { & $vcpkgExe install --triplet $Triplet "--x-manifest-root=$Repo" "--x-install-root=$installRoot" }

# The marker that makes the Rust vcpkg crate accept vcpkg_env\ as a vcpkg root.
$marker = Join-Path $VcpkgEnv '.vcpkg-root'
if (-not (Test-Path $marker)) { New-Item -ItemType File $marker | Out-Null }
foreach ($required in @('vcpkg\status', "$Triplet\bin\opencv_core4.dll")) {
    if (-not (Test-Path (Join-Path $installRoot $required))) { throw "vcpkg finished, but vcpkg_env\installed\$required is missing." }
}
Write-Ok 'OpenCV installed'

# ---------------------------------------------------------------- 4. cargo config
Write-Step 'Writing src-tauri\.cargo\config.toml'
# Every entry is forced: cargo's [env] otherwise leaves an already-set variable alone, and the
# developer shell sets VCPKG_ROOT to Visual Studio's vcpkg, which the Rust vcpkg crate can't read.
$config = @"
# Generated by setup.ps1 for this machine (gitignored). Run setup.ps1 again instead of editing.
# See config.toml.example for what each entry does.

[env]
VCPKG_ROOT = { value = $(ConvertTo-TomlLiteral $VcpkgEnv), force = true }
VCPKGRS_DYNAMIC = { value = '1', force = true }
OPENCV_PACKAGE_NAME = { value = 'opencv4', force = true }
OPENCV_DISABLE_PROBES = { value = 'pkg_config,cmake,vcpkg_cmake', force = true }
LIBCLANG_PATH = { value = $(ConvertTo-TomlLiteral $llvm), force = true }
CLANG_PATH = { value = $(ConvertTo-TomlLiteral (Join-Path $llvm 'clang.exe')), force = true }
INCLUDE = { value = $(ConvertTo-TomlLiteral $env:INCLUDE.TrimEnd(';')), force = true }
LIB = { value = $(ConvertTo-TomlLiteral $env:LIB.TrimEnd(';')), force = true }
"@
$cargoDir = Join-Path $Tauri '.cargo'
$configPath = Join-Path $cargoDir 'config.toml'
$old = if (Test-Path $configPath) { [IO.File]::ReadAllText($configPath) } else { $null }
if ($old -eq $config) {
    Write-Ok 'unchanged'
} else {
    if ($old) { Copy-Item $configPath "$configPath.bak" -Force; Write-Ok 'previous file kept as config.toml.bak' }
    New-Item -ItemType Directory -Force $cargoDir | Out-Null
    # No BOM: cargo's TOML parser rejects it.
    [IO.File]::WriteAllText($configPath, $config, (New-Object System.Text.UTF8Encoding $false))
    # build.rs copies the vcpkg DLLs next to the exe once per profile and marks that with this file;
    # removing it makes the next build copy them again from the (possibly new) VCPKG_ROOT.
    Get-ChildItem (Join-Path $Tauri 'target\*\.vcpkg-dlls-copied') -Force -ErrorAction SilentlyContinue | Remove-Item -Force
    Write-Ok 'written'
}

# A PATH entry into another vcpkg's DLLs can make the app load those instead of its own copies.
$ownBin = (Join-Path $installRoot "$Triplet\bin").TrimEnd('\')
foreach ($scope in @('User', 'Machine')) {
    $entries = [Environment]::GetEnvironmentVariable('Path', $scope) -split ';' | Where-Object { $_ -match 'vcpkg' -and $_.TrimEnd('\') -ne $ownBin }
    foreach ($e in $entries) {
        Write-Warn "$scope PATH contains $e. It isn't needed (build.rs copies the DLLs next to the exe) and can hide a DLL missing from the installer; consider removing it."
    }
}

# ---------------------------------------------------------------- 5. npm packages
if (-not $SkipNpm) {
    Write-Step 'Frontend packages'
    Invoke-Checked 'npm ci' { npm ci --no-audit --no-fund }
    Write-Ok 'npm packages installed'
}

# ---------------------------------------------------------------- 6. cargo check
if (-not $SkipCheck) {
    Write-Step 'cargo check (generates the OpenCV bindings; a few minutes the first time)'
    Push-Location $Tauri
    try {
        Invoke-Checked 'cargo check' { cargo check }
        Write-Ok 'Rust backend compiles'
    } finally {
        Pop-Location
    }
}

Write-Host ''
Write-Host 'Setup complete.' -ForegroundColor Green
Write-Host 'Start the app:   npm run tauri dev'
Write-Host 'Build installer: npm run tauri build'
