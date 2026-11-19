# Installs SuperBrick for the current user (no administrator rights needed).
#
#   powershell -ExecutionPolicy Bypass -File .\install.ps1            install or update
#   powershell -ExecutionPolicy Bypass -File .\install.ps1 -Uninstall remove it (your data stays)
#
# Copies bin\ and share\ from the folder this script is in to -Prefix, adds bin to your PATH,
# registers the .sb file type (and "Open with" for .ldr/.mpd) and adds a Start Menu shortcut.
# NOTE: tools/install.py on Windows does the same through tools/windows/winsetup.py; keep the
# two in sync (tools/test_install_windows.py checks that the registry keys match).
param(
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA 'Programs\SuperBrick'),
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'

Add-Type -Namespace SB -Name Native -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("user32.dll", CharSet = System.Runtime.InteropServices.CharSet.Unicode)]
public static extern System.IntPtr SendMessageTimeout(System.IntPtr hWnd, uint Msg, System.UIntPtr wParam, string lParam, uint flags, uint timeout, out System.UIntPtr result);
[System.Runtime.InteropServices.DllImport("shell32.dll")]
public static extern void SHChangeNotify(int eventId, uint flags, System.IntPtr item1, System.IntPtr item2);
'@

function Send-EnvironmentChange {
    $r = [UIntPtr]::Zero
    [void][SB.Native]::SendMessageTimeout([IntPtr]0xFFFF, 0x1A, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$r)
}
function Send-AssocChange { [SB.Native]::SHChangeNotify(0x08000000, 0, [IntPtr]::Zero, [IntPtr]::Zero) }
function Norm([string]$p) { $p.Trim().Trim('"').TrimEnd('\', '/').ToLowerInvariant() }

function Get-UserPath {
    $key = Get-Item 'HKCU:\Environment'
    [string]$key.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
}
function Set-UserPath([string]$value) {
    Set-ItemProperty -Path 'HKCU:\Environment' -Name 'Path' -Value $value -Type ExpandString
    Send-EnvironmentChange
}
function Set-RegValue([string]$Key, [string]$Name, [string]$Value) {
    $path = "HKCU:\Software\Classes\$Key"
    if (-not (Test-Path -LiteralPath $path)) { New-Item -Path $path -Force | Out-Null }
    if ($Name -eq '') { Set-Item -LiteralPath $path -Value $Value }
    else { New-ItemProperty -LiteralPath $path -Name $Name -Value $Value -PropertyType String -Force | Out-Null }
}

$bin = Join-Path $Prefix 'bin'
$lnk = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\SuperBrick.lnk'

if ($Uninstall) {
    foreach ($k in 'SuperBrick.Project', 'SuperBrick.LDraw', 'Applications\SuperBrick.exe') {
        Remove-Item -LiteralPath "HKCU:\Software\Classes\$k" -Recurse -Force -ErrorAction SilentlyContinue
    }
    # .sb: give the default back to its previous owner (or clear it) only if it is still ours,
    # drop our value, and remove the key only when nothing else (other apps' entries) is left.
    # The .NET registry API, because the PowerShell provider cannot reliably delete a default value.
    $classes = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Classes', $true)
    $sb = if ($classes) { $classes.OpenSubKey('.sb', $true) } else { $null }
    if ($sb) {
        $previous = $sb.GetValue('SuperBrick.Previous')
        if ($sb.GetValue('') -eq 'SuperBrick.Project') {
            if ($previous) { $sb.SetValue('', $previous) } else { $sb.DeleteValue('', $false) }
        }
        $sb.DeleteValue('SuperBrick.Previous', $false)
        $empty = $sb.SubKeyCount -eq 0 -and $sb.ValueCount -eq 0
        $sb.Close()
        if ($empty) { $classes.DeleteSubKey('.sb', $false) }
    }
    if ($classes) { $classes.Close() }
    foreach ($e in '.ldr', '.mpd') {
        Remove-ItemProperty -LiteralPath "HKCU:\Software\Classes\$e\OpenWithProgids" -Name 'SuperBrick.LDraw' -ErrorAction SilentlyContinue
    }
    Send-AssocChange
    Remove-Item -LiteralPath $lnk -Force -ErrorAction SilentlyContinue
    $kept = @((Get-UserPath) -split ';' | Where-Object { $_.Trim() -ne '' -and (Norm $_) -ne (Norm $bin) })
    Set-UserPath ($kept -join ';')
    if (Test-Path -LiteralPath $bin) {  # a running exe can be renamed but not deleted
        Get-ChildItem -LiteralPath $bin -Filter *.exe | ForEach-Object { Rename-Item -LiteralPath $_.FullName -NewName ($_.Name + '.old') -Force -ErrorAction SilentlyContinue }
    }
    Remove-Item -LiteralPath $Prefix -Recurse -Force -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $Prefix) { Write-Warning "Some files in $Prefix are in use; delete the folder after closing SuperBrick." }
    Write-Host "Removed SuperBrick. Your data in $env:APPDATA\SuperBrick is untouched."
    return
}

$source = $PSScriptRoot
if ((Norm $source) -ne (Norm $Prefix)) {
    New-Item -ItemType Directory -Force -Path $bin | Out-Null
    foreach ($name in 'sb.exe', 'sb-dev.exe', 'SuperBrick.exe') {
        $target = Join-Path $bin $name
        if (Test-Path -LiteralPath $target) {  # Windows can rename a running exe, not overwrite it
            Remove-Item -LiteralPath "$target.old" -Force -ErrorAction SilentlyContinue
            Rename-Item -LiteralPath $target -NewName "$name.old" -Force -ErrorAction SilentlyContinue
        }
        Copy-Item -LiteralPath (Join-Path $source "bin\$name") -Destination $target -Force
        Remove-Item -LiteralPath "$target.old" -Force -ErrorAction SilentlyContinue
    }
    $share = Join-Path $Prefix 'share'
    New-Item -ItemType Directory -Force -Path $share | Out-Null
    Copy-Item -Path (Join-Path $source 'share\*') -Destination $share -Recurse -Force
}

$exe = Join-Path $bin 'SuperBrick.exe'
$command = '"' + $exe + '" "%1"'
# Remember who owned .sb before us, so uninstall can give it back.
$sbKey = 'HKCU:\Software\Classes\.sb'
if (Test-Path -LiteralPath $sbKey) {
    $owner = (Get-Item -LiteralPath $sbKey).GetValue('')
    if ($owner -and $owner -ne 'SuperBrick.Project' -and $null -eq (Get-Item -LiteralPath $sbKey).GetValue('SuperBrick.Previous')) {
        Set-RegValue '.sb' 'SuperBrick.Previous' $owner
    }
}
Set-RegValue 'SuperBrick.Project' '' 'SuperBrick Project'
Set-RegValue 'SuperBrick.Project' 'FriendlyTypeName' 'SuperBrick Project'
Set-RegValue 'SuperBrick.Project\DefaultIcon' '' "$exe,0"
Set-RegValue 'SuperBrick.Project\shell\open\command' '' $command
Set-RegValue '.sb' '' 'SuperBrick.Project'
Set-RegValue 'SuperBrick.LDraw' '' 'LDraw Model'
Set-RegValue 'SuperBrick.LDraw' 'FriendlyTypeName' 'LDraw Model'
Set-RegValue 'SuperBrick.LDraw\DefaultIcon' '' "$exe,0"
Set-RegValue 'SuperBrick.LDraw\shell\open\command' '' $command
Set-RegValue 'Applications\SuperBrick.exe' 'FriendlyAppName' 'SuperBrick'
Set-RegValue 'Applications\SuperBrick.exe\DefaultIcon' '' "$exe,0"
Set-RegValue 'Applications\SuperBrick.exe\shell\open\command' '' $command
foreach ($e in '.sb', '.ldr', '.mpd') { Set-RegValue 'Applications\SuperBrick.exe\SupportedTypes' $e '' }
# .ldr and .mpd are offered under "Open with", never taken over.
foreach ($e in '.ldr', '.mpd') { Set-RegValue "$e\OpenWithProgids" 'SuperBrick.LDraw' '' }
Send-AssocChange

$parts = @((Get-UserPath) -split ';' | Where-Object { $_.Trim() -ne '' })
if (-not ($parts | Where-Object { (Norm $_) -eq (Norm $bin) })) { Set-UserPath ((@($parts) + $bin) -join ';') }

$shell = New-Object -ComObject WScript.Shell
$s = $shell.CreateShortcut($lnk)
$s.TargetPath = $exe
$s.WorkingDirectory = $bin
$s.IconLocation = "$exe,0"
$s.Description = 'SuperBrick'
$s.Save()

Write-Host "Installed SuperBrick in $Prefix. Open it from the Start Menu, or run 'sb' in a new terminal."
Write-Host "First launch downloads the LDraw parts library (about 147 MB)."
