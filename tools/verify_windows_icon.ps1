param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [Parameter(Mandatory = $true)][string]$ExpectedIcon
)

# Read the shell's associated EXE icon without starting the program or a desktop.
# A missing PE icon falls back to a generic shell icon and must fail comparison.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$actualIcon = $null
$expected = $null
$actualBitmap = $null
$expectedBitmap = $null
try {
    $executablePath = (Resolve-Path -LiteralPath $Executable).ProviderPath
    $iconPath = (Resolve-Path -LiteralPath $ExpectedIcon).ProviderPath
    $actualIcon = [System.Drawing.Icon]::ExtractAssociatedIcon($executablePath)
    if ($null -eq $actualIcon) { throw 'Windows returned no EXE icon' }
    $expected = [System.Drawing.Icon]::new($iconPath, $actualIcon.Size)
    $actualBitmap = $actualIcon.ToBitmap()
    $expectedBitmap = $expected.ToBitmap()
    if ($actualBitmap.Size -ne $expectedBitmap.Size) {
        throw 'Windows EXE icon dimensions differ from the approved ICO'
    }
    $visible = 0
    for ($y = 0; $y -lt $actualBitmap.Height; $y++) {
        for ($x = 0; $x -lt $actualBitmap.Width; $x++) {
            $actual = $actualBitmap.GetPixel($x, $y)
            $wanted = $expectedBitmap.GetPixel($x, $y)
            if ($actual.ToArgb() -ne $wanted.ToArgb()) {
                throw 'Windows EXE icon pixels differ from the approved ICO'
            }
            if ($actual.A -gt 0) { $visible++ }
        }
    }
    if ($visible -eq 0) { throw 'Windows EXE icon is empty' }
    Write-Output 'PASS: Windows shell EXE icon matches the approved ICO pixels; program not executed'
} finally {
    foreach ($resource in @($actualBitmap, $expectedBitmap, $actualIcon, $expected)) {
        if ($null -ne $resource) { $resource.Dispose() }
    }
}
