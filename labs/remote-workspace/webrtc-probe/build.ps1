$ErrorActionPreference = 'Stop'

$webrtcOrigin = 'https://webrtc.googlesource.com/src.git'
$webrtcRevision = '692e07f2cb2f3602af5a8083b695f4f3bc2c9a6f'
$depotOrigin = 'https://chromium.googlesource.com/chromium/tools/depot_tools.git'
$depotRevision = '1b0972d3d5e5b1e78055147cb3576361e274b9ed'

function Invoke-Checked([string] $Executable, [string[]] $Arguments) {
    & $Executable @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Executable failed with exit code $LASTEXITCODE."
    }
}

function Get-PinnedSource([string] $Path, [string] $Url, [string] $Revision) {
    $null = New-Item -ItemType Directory -Path $Path
    Invoke-Checked git @('-C', $Path, 'init', '--quiet')
    Invoke-Checked git @('-C', $Path, 'remote', 'add', 'origin', $Url)
    Invoke-Checked git @('-C', $Path, 'fetch', '--quiet', '--depth=1', 'origin', $Revision)
    Invoke-Checked git @('-C', $Path, 'checkout', '--quiet', '--detach', 'FETCH_HEAD')
    $head = Invoke-Checked git @('-C', $Path, 'rev-parse', 'HEAD')
    if ($head -cne $Revision) {
        throw "Unexpected checkout revision for $Url."
    }
}

$buildRoot = Join-Path $env:RUNNER_TEMP 'rw04-source'
$reports = Join-Path $env:RUNNER_TEMP 'rw04-report'
$null = New-Item -ItemType Directory -Path $buildRoot, $reports
$report = Join-Path $reports 'revisions.txt'
@("WebRTC $webrtcRevision", "depot_tools $depotRevision", "Runner $env:RUNNER_OS $env:RUNNER_ARCH", "Linko build $env:LINKO_VERSION") |
    Set-Content -LiteralPath $report -Encoding utf8

if ($IsWindows) {
    $sdk = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10/Include/10.0.28000.0/um/windows.h'
    if (!(Test-Path -LiteralPath $sdk)) {
        throw 'The pinned Chromium toolchain requires Windows SDK 10.0.28000.0 on the runner.'
    }
}

$depot = Join-Path $buildRoot 'depot_tools'
$workspace = Join-Path $buildRoot 'workspace'
$null = New-Item -ItemType Directory -Path $workspace
$source = Join-Path $workspace 'src'

Get-PinnedSource $depot $depotOrigin $depotRevision
Get-PinnedSource $source $webrtcOrigin $webrtcRevision
$env:PATH = $depot + [IO.Path]::PathSeparator + $env:PATH
$env:DEPOT_TOOLS_UPDATE = '0'
if ($IsWindows) {
    $env:DEPOT_TOOLS_WIN_TOOLCHAIN = '0'
    Invoke-Checked (Join-Path $depot 'cipd_bin_setup.bat') @()
    Invoke-Checked (Join-Path $depot 'bootstrap/win_tools.bat') @()
} else {
    Invoke-Checked bash @((Join-Path $depot 'ensure_bootstrap'))
}
$suffix = if ($IsWindows) { '.bat' } else { '' }
Invoke-Checked (Join-Path $depot "python-bin/python3$suffix") @('--version')
$gclient = Join-Path $depot "gclient$suffix"
$gn = Join-Path $depot "gn$suffix"
$autoninja = Join-Path $depot "autoninja$suffix"

Set-Location -LiteralPath $workspace
Invoke-Checked $gclient @('config', '--name=src', $webrtcOrigin)
Invoke-Checked $gclient @('sync', '--no-history', '--nohooks', '--revision', "src@$webrtcRevision")
Invoke-Checked $gclient @('runhooks')

Set-Location -LiteralPath $source
$actual = Invoke-Checked git @('rev-parse', 'HEAD')
if ($actual -cne $webrtcRevision) {
    throw 'WebRTC checkout changed during dependency sync.'
}
foreach ($file in @('DEPS', 'LICENSE', 'PATENTS')) {
    $blob = Invoke-Checked git @('rev-parse', "HEAD:$file")
    Add-Content -LiteralPath $report -Value "$file git blob $blob" -Encoding utf8
}
$out = 'out/linko-rw04'
$null = New-Item -ItemType Directory -Path $out, linko_probe -Force
foreach ($file in @('BUILD.gn', 'frame_probe.cc', 'h264_probe.h', 'h264_probe_main.c', 'h264_probe.mm')) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $file) -Destination linko_probe
}
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'args.gn') -Destination (Join-Path $out 'args.gn')
Copy-Item -LiteralPath (Join-Path $out 'args.gn') -Destination $reports
Invoke-Checked $gn @('gen', $out, '--fail-on-unused-args', '--root-target=//linko_probe:linko_rw04_all_probes')
$exe = if ($IsWindows) { 'linko_rw04_probe.exe' } else { 'linko_rw04_probe' }
$targets = @('-C', $out, $exe)
if ($IsMacOS) {
    $targets += 'linko_rw04_macos_h264'
}
Invoke-Checked $autoninja $targets
$program = Join-Path (Join-Path $source $out) $exe
Invoke-Checked $program @()
Add-Content -LiteralPath $report -Value 'Synthetic I420 VideoFrame and RTP timestamp roundtrip passed.' -Encoding utf8

if ($IsMacOS) {
    Invoke-Checked (Join-Path (Join-Path $source $out) 'linko_rw04_macos_h264') @()
    Add-Content -LiteralPath $report -Value 'C ABI and native WebRTC VideoToolbox H.264 roundtrip passed.' -Encoding utf8
}
