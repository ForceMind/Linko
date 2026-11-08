[CmdletBinding(DefaultParameterSetName = 'Verify')]
param(
    [Parameter(Mandatory = $true, ParameterSetName = 'Verify')][string]$Executable,
    [Parameter(Mandatory = $true, ParameterSetName = 'Verify')][string]$ExpectedIcon,
    [Parameter(Mandatory = $true, ParameterSetName = 'SelfTest')][switch]$SelfTest
)

$ErrorActionPreference = 'Stop'
# Resource-only mapping: never execute the inspected EXE or resolve its imports.
# Raw PE bytes establish provenance; shell readback separately checks presentation.
Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;

namespace LinkoIconCheck {
    public sealed class Frame {
        public byte Width, Height, Colors, Reserved;
        public ushort Planes, Bits;
        public byte[] Data;
        public int PixelWidth { get { return Width == 0 ? 256 : Width; } }
        public int PixelHeight { get { return Height == 0 ? 256 : Height; } }
    }
    public static class Resources {
        const int Limit = 8 * 1024 * 1024;
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        static extern IntPtr LoadLibraryExW(string path, IntPtr file, uint flags);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        static extern IntPtr FindResourceW(IntPtr module, IntPtr name, IntPtr type);
        [DllImport("kernel32.dll", SetLastError = true)]
        static extern uint SizeofResource(IntPtr module, IntPtr resource);
        [DllImport("kernel32.dll", SetLastError = true)]
        static extern IntPtr LoadResource(IntPtr module, IntPtr resource);
        [DllImport("kernel32.dll", SetLastError = true)]
        static extern IntPtr LockResource(IntPtr resource);
        [DllImport("kernel32.dll")]
        static extern bool FreeLibrary(IntPtr module);
        [DllImport("user32.dll", SetLastError = true)]
        static extern IntPtr CreateIconFromResourceEx(byte[] data, uint size, bool icon,
                                                     uint version, int width, int height, uint flags);
        [DllImport("user32.dll")]
        static extern bool DestroyIcon(IntPtr icon);

        static void Range(byte[] data, int offset, int length) {
            if (offset < 0 || length < 0 || offset > data.Length - length)
                throw new InvalidDataException("Icon record is truncated or out of bounds");
        }
        static ushort U16(byte[] data, int at) { Range(data, at, 2); return BitConverter.ToUInt16(data, at); }
        static uint U32(byte[] data, int at) { Range(data, at, 4); return BitConverter.ToUInt32(data, at); }
        public static string Digest(byte[] data) {
            using (SHA256 hash = SHA256.Create())
                return BitConverter.ToString(hash.ComputeHash(data)).Replace("-", "").ToLowerInvariant();
        }
        static bool Equal(byte[] a, byte[] b) {
            if (a.Length != b.Length) return false;
            for (int i = 0; i < a.Length; i++) if (a[i] != b[i]) return false;
            return true;
        }
        public static Frame[] ParseIco(byte[] data) {
            if (data.Length > Limit || U16(data, 0) != 0 || U16(data, 2) != 1)
                throw new InvalidDataException("Expected file is not a bounded ICO");
            int count = U16(data, 4);
            if (count < 1 || count > 64) throw new InvalidDataException("ICO frame count invalid");
            int tableEnd = 6 + count * 16;
            Range(data, 0, tableEnd);
            Frame[] frames = new Frame[count];
            for (int i = 0; i < count; i++) {
                int at = 6 + 16 * i;
                uint size = U32(data, at + 8), offset = U32(data, at + 12);
                if (size == 0 || size > Limit || offset < tableEnd || offset > Limit)
                    throw new InvalidDataException("ICO frame bounds invalid");
                Range(data, (int)offset, (int)size);
                Frame frame = new Frame();
                frame.Width = data[at]; frame.Height = data[at + 1];
                frame.Colors = data[at + 2]; frame.Reserved = data[at + 3];
                frame.Planes = U16(data, at + 4); frame.Bits = U16(data, at + 6);
                if (frame.Reserved != 0 || frame.PixelWidth != frame.PixelHeight)
                    throw new InvalidDataException("ICO frame metadata invalid");
                frame.Data = new byte[(int)size];
                Buffer.BlockCopy(data, (int)offset, frame.Data, 0, (int)size);
                frames[i] = frame;
            }
            return frames;
        }
        public static void ValidateGroup(byte[] group, Frame[] frames, Func<int, byte[]> read) {
            if (U16(group, 0) != 0 || U16(group, 2) != 1 || U16(group, 4) != frames.Length ||
                group.Length != 6 + 14 * frames.Length)
                throw new InvalidDataException("PE RT_GROUP_ICON does not match ICO frame count/layout");
            for (int i = 0; i < frames.Length; i++) {
                int at = 6 + 14 * i;
                Frame frame = frames[i];
                if (group[at] != frame.Width || group[at + 1] != frame.Height ||
                    group[at + 2] != frame.Colors || group[at + 3] != frame.Reserved ||
                    U16(group, at + 4) != frame.Planes || U16(group, at + 6) != frame.Bits ||
                    U32(group, at + 8) != frame.Data.Length)
                    throw new InvalidDataException("PE icon frame metadata differs at index " + i);
                int id = U16(group, at + 12);
                if (id == 0) throw new InvalidDataException("PE RT_ICON id is zero");
                byte[] actual = read(id);
                if (!Equal(actual, frame.Data))
                    throw new InvalidDataException("PE RT_ICON " + id + " differs byte-for-byte; expected=" +
                                                   Digest(frame.Data) + " actual=" + Digest(actual));
            }
        }
        static byte[] Read(IntPtr module, int type, int id) {
            IntPtr resource = FindResourceW(module, new IntPtr(id), new IntPtr(type));
            if (resource == IntPtr.Zero) throw new InvalidDataException("Missing PE resource type/id " + type + "/" + id);
            uint size = SizeofResource(module, resource);
            if (size == 0 || size > Limit) throw new InvalidDataException("PE resource size invalid");
            IntPtr loaded = LoadResource(module, resource);
            IntPtr bytes = loaded == IntPtr.Zero ? IntPtr.Zero : LockResource(loaded);
            if (bytes == IntPtr.Zero) throw new InvalidDataException("PE resource could not be read");
            byte[] data = new byte[(int)size]; Marshal.Copy(bytes, data, 0, data.Length); return data;
        }
        public static void VerifyPe(string path, Frame[] frames) {
            // LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE: no DllMain/import initialization.
            IntPtr module = LoadLibraryExW(path, IntPtr.Zero, 0x00000002 | 0x00000020);
            if (module == IntPtr.Zero) throw new InvalidDataException("Resource-only PE mapping failed: " + Marshal.GetLastWin32Error());
            try { ValidateGroup(Read(module, 14, 1), frames, id => Read(module, 3, id)); }
            finally { FreeLibrary(module); }
        }
        public static IntPtr CreateFrame(Frame frame, int width, int height) {
            IntPtr icon = CreateIconFromResourceEx(frame.Data, (uint)frame.Data.Length, true,
                                                  0x00030000, width, height, 0);
            if (icon == IntPtr.Zero) throw new InvalidDataException("Windows could not decode source icon frame: " + Marshal.GetLastWin32Error());
            return icon;
        }
        public static void ReleaseIcon(IntPtr icon) { if (icon != IntPtr.Zero) DestroyIcon(icon); }
        public static void SelfTest(byte[] ico) {
            Frame[] frames = ParseIco(ico);
            byte[] group = new byte[6 + 14 * frames.Length];
            group[2] = 1; Buffer.BlockCopy(BitConverter.GetBytes((ushort)frames.Length), 0, group, 4, 2);
            byte[][] resources = new byte[frames.Length][];
            for (int i = 0; i < frames.Length; i++) {
                Buffer.BlockCopy(ico, 6 + 16 * i, group, 6 + 14 * i, 12);
                Buffer.BlockCopy(BitConverter.GetBytes((ushort)(i + 1)), 0, group, 18 + 14 * i, 2);
                resources[i] = (byte[])frames[i].Data.Clone();
            }
            ValidateGroup(group, frames, id => resources[id - 1]);
            resources[0][0] ^= 1;
            bool rejected = false;
            try { ValidateGroup(group, frames, id => resources[id - 1]); }
            catch (InvalidDataException) { rejected = true; }
            if (!rejected) throw new Exception("Changed RT_ICON bytes incorrectly accepted");
            resources[0][0] ^= 1;
            group[6] ^= 1; rejected = false;
            try { ValidateGroup(group, frames, id => resources[id - 1]); }
            catch (InvalidDataException) { rejected = true; }
            if (!rejected) throw new Exception("Changed group metadata incorrectly accepted");
            rejected = false;
            try { ParseIco(new byte[6]); } catch (InvalidDataException) { rejected = true; }
            if (!rejected) throw new Exception("Invalid ICO incorrectly accepted");
        }
    }
}
'@

function Compare-IconBitmap($Actual, $Wanted) {
    if ($Actual.Size -ne $Wanted.Size) { throw 'Native icon rasterization dimensions differ' }
    $raw = 0; $hidden = 0; $visibleDifference = 0; $visible = 0; $first = 'none'; $firstRaw = 'none'
    for ($y = 0; $y -lt $Actual.Height; $y++) {
        for ($x = 0; $x -lt $Actual.Width; $x++) {
            $a = $Actual.GetPixel($x, $y); $b = $Wanted.GetPixel($x, $y)
            if ($a.A -gt 0) { $visible++ }
            if ($a.ToArgb() -ne $b.ToArgb()) {
                $raw++
                $sample = "x=$x y=$y actual=($($a.A),$($a.R),$($a.G),$($a.B)) expected=($($b.A),$($b.R),$($b.G),$($b.B))"
                if ($firstRaw -eq 'none') { $firstRaw = $sample }
                # Fully transparent RGB is mathematically invisible, not a pixel tolerance.
                if ($a.A -eq 0 -and $b.A -eq 0) { $hidden++ }
                else {
                    $visibleDifference++
                    if ($first -eq 'none') { $first = $sample }
                }
            }
        }
    }
    return [pscustomobject]@{ Raw = $raw; Hidden = $hidden; VisibleDifference = $visibleDifference; Visible = $visible; First = $first; FirstRaw = $firstRaw }
}

if ($SelfTest) {
    $source = Join-Path $PSScriptRoot '../apps/peerglide-gui/assets/brand/Linko.ico'
    [LinkoIconCheck.Resources]::SelfTest([IO.File]::ReadAllBytes($source))
    function New-TestPixel($Alpha, $Red, $Green, $Blue) {
        $pixel = [pscustomobject]@{ A = $Alpha; R = $Red; G = $Green; B = $Blue }
        $pixel | Add-Member -MemberType ScriptMethod -Name ToArgb -Value {
            return ([long]$this.A * 16777216 + [long]$this.R * 65536 + [long]$this.G * 256 + $this.B)
        }
        return $pixel
    }
    function New-TestBitmap($Pixels) {
        $bitmap = [pscustomobject]@{ Width = 2; Height = 1; Size = '2x1'; Pixels = $Pixels }
        $bitmap | Add-Member -MemberType ScriptMethod -Name GetPixel -Value {
            param($X, $Y)
            return $this.Pixels[$X]
        }
        return $bitmap
    }
    $a = New-TestBitmap @((New-TestPixel 0 1 2 3), (New-TestPixel 255 50 60 70))
    $b = New-TestBitmap @((New-TestPixel 0 9 8 7), (New-TestPixel 255 50 60 70))
    $difference = Compare-IconBitmap $a $b
    if ($difference.VisibleDifference -ne 0 -or $difference.Hidden -ne 1 -or $difference.Visible -ne 1) {
        throw 'Transparent RGB semantic comparison is incorrect'
    }
    $b.Pixels[1] = New-TestPixel 255 51 60 70
    if ((Compare-IconBitmap $a $b).VisibleDifference -ne 1) {
        throw 'A one-channel visible pixel difference was incorrectly accepted'
    }
    $b.Pixels[1] = New-TestPixel 255 50 60 70
    $b.Pixels[0] = New-TestPixel 1 1 2 3
    if ((Compare-IconBitmap $a $b).VisibleDifference -ne 1) {
        throw 'An alpha difference was incorrectly accepted'
    }
    Write-Output 'PASS: raw resource mutation and exact visible-RGBA self-tests; no Windows API or program executed'
    return
}
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'Actual PE resource and shell verification requires Windows'
}
Add-Type -AssemblyName System.Drawing

$actualIcon = $null; $actualBitmap = $null
try {
    $executablePath = (Resolve-Path -LiteralPath $Executable).ProviderPath
    $iconPath = (Resolve-Path -LiteralPath $ExpectedIcon).ProviderPath
    $frames = [LinkoIconCheck.Resources]::ParseIco([IO.File]::ReadAllBytes($iconPath))
    [LinkoIconCheck.Resources]::VerifyPe($executablePath, $frames)
    Write-Output "PASS: PE RT_GROUP_ICON #1 and all $($frames.Length) RT_ICON payloads equal approved ICO original bytes"
    $actualIcon = [System.Drawing.Icon]::ExtractAssociatedIcon($executablePath)
    if ($null -eq $actualIcon) { throw 'Windows returned no EXE shell icon' }
    $actualBitmap = $actualIcon.ToBitmap()
    if ($actualBitmap.Width -lt 1 -or $actualBitmap.Width -gt 1024 -or $actualBitmap.Width -ne $actualBitmap.Height) {
        throw 'Windows shell icon dimensions invalid'
    }
    # Retain diagnostics for the failed old path without treating its frame-selection
    # and HICON conversion choices as proof of which bytes the PE contains.
    $legacy = $null; $legacyBitmap = $null
    try {
        $legacy = [System.Drawing.Icon]::new($iconPath, $actualIcon.Size)
        $legacyBitmap = $legacy.ToBitmap()
        $difference = Compare-IconBitmap $actualBitmap $legacyBitmap
        Write-Output "INFO: old-constructor size=$($actualIcon.Size) rawARGBDiff=$($difference.Raw) hiddenRGBDiff=$($difference.Hidden) visibleDiff=$($difference.VisibleDifference) firstRaw=$($difference.FirstRaw) firstVisible=$($difference.First)"
    } catch {
        Write-Output "INFO: old-constructor diagnostic unavailable: $($_.Exception.GetType().FullName)"
    } finally {
        if ($null -ne $legacyBitmap) { $legacyBitmap.Dispose() }
        if ($null -ne $legacy) { $legacy.Dispose() }
    }
    $matched = $false
    foreach ($frame in ($frames | Sort-Object @{ Expression = { [Math]::Abs($_.PixelWidth - $actualBitmap.Width) } })) {
        $handle = [IntPtr]::Zero; $native = $null; $wanted = $null
        try {
            # Same native HICON decoding/rasterization path, explicit source frame.
            # Shell may legally choose any declared frame; arbitrary fallback pixels cannot pass.
            $handle = [LinkoIconCheck.Resources]::CreateFrame($frame, $actualBitmap.Width, $actualBitmap.Height)
            $native = [System.Drawing.Icon]::FromHandle($handle)
            $wanted = $native.ToBitmap()
            $difference = Compare-IconBitmap $actualBitmap $wanted
            Write-Output "INFO: shell=$($actualIcon.Size) source=$($frame.PixelWidth)x$($frame.PixelHeight) rawARGBDiff=$($difference.Raw) hiddenRGBDiff=$($difference.Hidden) visibleDiff=$($difference.VisibleDifference) firstRaw=$($difference.FirstRaw) firstVisible=$($difference.First)"
            if ($difference.Visible -eq 0) { throw 'Windows EXE shell icon is empty' }
            if ($difference.VisibleDifference -eq 0) { $matched = $true; break }
        } finally {
            if ($null -ne $wanted) { $wanted.Dispose() }
            if ($null -ne $native) { $native.Dispose() }
            [LinkoIconCheck.Resources]::ReleaseIcon($handle)
        }
    }
    if (-not $matched) { throw 'Shell icon does not exactly match visible RGBA from any approved native ICO frame' }
    Write-Output 'PASS: nonempty Windows shell readback exactly matches an approved native ICO frame visually; EXE not executed'
} finally {
    if ($null -ne $actualBitmap) { $actualBitmap.Dispose() }
    if ($null -ne $actualIcon) { $actualIcon.Dispose() }
}
