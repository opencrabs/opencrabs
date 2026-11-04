#Requires -Version 5.1
<#
.SYNOPSIS
    OpenCrabs installer for Windows.

.DESCRIPTION
    iwr -useb https://opencrabs.com/install.ps1 | iex

    Resolves the latest release, verifies the SHA-256 against the release's own
    SHA256SUMS file, and installs every exe and DLL the archive ships (the
    binary, the rtk helper, and the MSVC runtime beside them) into a directory
    on your user PATH.

    To pass flags, build a script block instead of piping straight into iex:
      & ([scriptblock]::Create((iwr -useb https://opencrabs.com/install.ps1))) -Version v0.5.11

.PARAMETER Version
    Install a specific release tag (default: the latest).

.PARAMETER Dir
    Install directory (default: %LOCALAPPDATA%\OpenCrabs\bin).

.PARAMETER DryRun
    Print the plan and exit without downloading anything.

.PARAMETER NoPathUpdate
    Install the binaries but leave the user PATH alone.

.PARAMETER Help
    Show this help.
#>
[CmdletBinding()]
param(
    [string]$Version = '',
    [string]$Dir = '',
    [switch]$DryRun,
    [switch]$NoPathUpdate,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repo = 'opencrabs/opencrabs'
$BinName = 'opencrabs'
$RtkName = 'rtk'
$AssetArch = 'amd64'

function Write-Say {
    param([string]$Message)
    Write-Host $Message
}

function Write-Warn {
    param([string]$Message)
    Write-Host "warning: $Message" -ForegroundColor Yellow
}

function Die {
    param([string]$Message)
    Write-Host ''
    Write-Host "error: $Message" -ForegroundColor Red
    throw 'OpenCrabs install failed.'
}

function Show-Usage {
    Write-Host @'
OpenCrabs installer for Windows.

  iwr -useb https://opencrabs.com/install.ps1 | iex

Resolves the latest release, verifies the SHA-256 against the release's own
SHA256SUMS file, and installs every exe and DLL the archive ships (the binary,
the rtk helper, and the MSVC runtime beside them) into a directory on your user
PATH.

To pass flags, build a script block instead of piping straight into iex:
  & ([scriptblock]::Create((iwr -useb https://opencrabs.com/install.ps1))) -Version v0.5.11

  -Version <tag>   Install a specific release (default: the latest)
  -Dir <path>      Install directory (default: %LOCALAPPDATA%\OpenCrabs\bin)
  -DryRun          Print the plan and exit without downloading anything
  -NoPathUpdate    Install the binaries but leave the user PATH alone
  -Help            Show this help
'@
}

# ---------------------------------------------------------------- platform

if ($Help) {
    Show-Usage
    return
}

if (-not ($env:OS -eq 'Windows_NT')) {
    Die 'this installer is for Windows. On macOS or Linux run: curl -fsSL https://opencrabs.com/install.sh | bash'
}

if (-not [System.Environment]::Is64BitOperatingSystem) {
    Die 'OpenCrabs ships 64-bit builds only'
}

# PROCESSOR_ARCHITECTURE reports the architecture of the running process, so an
# x64 shell on Windows on ARM says AMD64. ARCHITEW6432 carries the real one.
$realArch = $env:PROCESSOR_ARCHITEW6432
if (-not $realArch) { $realArch = $env:PROCESSOR_ARCHITECTURE }
if ($realArch -eq 'ARM64') {
    Write-Warn 'there is no native arm64 Windows build yet; installing the amd64 build, which runs under Windows 11 emulation'
}

try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {
    # Windows PowerShell on an old build may not expose the enum member; the
    # default protocol then still works on anything GitHub accepts.
}

# ---------------------------------------------------------------- release

function Resolve-ReleaseTag {
    if ($Version) {
        $tag = $Version
        if ($tag -notmatch '^v') { $tag = "v$tag" }
        if ($tag -notmatch '^v\d+\.\d+\.\d+') { Die "not a release tag: $Version" }
        return $tag
    }

    # The /releases/latest redirect already carries the tag, and unlike the API
    # it is not rate limited per IP (a shared NAT runs out of API calls fast).
    try {
        $request = [System.Net.HttpWebRequest]::Create("https://github.com/$Repo/releases/latest")
        $request.AllowAutoRedirect = $false
        $request.Method = 'HEAD'
        $request.UserAgent = 'opencrabs-installer'
        $request.Timeout = 20000
        $response = $request.GetResponse()
        try {
            $location = [string]$response.Headers['Location']
        } finally {
            $response.Close()
        }
        if ($location) {
            $candidate = ($location -split '\?')[0].TrimEnd('/').Split('/')[-1]
            if ($candidate -match '^v\d+\.\d+\.\d+') { return $candidate }
        }
    } catch {
        # fall through to the API
    }

    try {
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" `
            -UseBasicParsing -Headers @{ 'User-Agent' = 'opencrabs-installer' }
        if ($release.tag_name -match '^v\d+\.\d+\.\d+') { return $release.tag_name }
    } catch {
        # reported below
    }

    Die 'could not resolve the latest release (github.com may be unreachable)'
}

# ---------------------------------------------------------------- download

function Get-RemoteFile {
    param([string]$Url, [string]$Path)
    $lastError = $null
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        try {
            Invoke-WebRequest -Uri $Url -OutFile $Path -UseBasicParsing -MaximumRedirection 5
            return
        } catch {
            $lastError = $_
            if ($attempt -lt 3) { Start-Sleep -Seconds 1 }
        }
    }
    Die "download failed: $Url`n  $($lastError.Exception.Message)"
}

function Get-ExpectedHash {
    param([string]$SumsPath, [string]$AssetName)
    foreach ($line in (Get-Content -LiteralPath $SumsPath)) {
        $parts = $line -split '\s+', 2
        if ($parts.Count -eq 2 -and $parts[1].Trim() -eq $AssetName) {
            return $parts[0].Trim().ToLowerInvariant()
        }
    }
    return $null
}

# ---------------------------------------------------------------- PATH

function Get-UserPathRaw {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $false)
    if ($null -eq $key) { return '' }
    try {
        return [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    } finally {
        $key.Close()
    }
}

function Set-UserPathRaw {
    param([string]$Value)
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    if ($null -eq $key) { Die 'could not open HKCU\Environment to update PATH' }
    try {
        # ExpandString keeps %USERPROFILE%-style entries working; writing the
        # expanded form back would bake this machine's paths into the registry.
        $key.SetValue('Path', $Value, [Microsoft.Win32.RegistryValueKind]::ExpandString)
    } finally {
        $key.Close()
    }
}

function Send-EnvironmentChange {
    try {
        if (-not ('OpenCrabs.Installer.NativeMethods' -as [type])) {
            Add-Type -Namespace 'OpenCrabs.Installer' -Name 'NativeMethods' -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
'@
        }
        $result = [UIntPtr]::Zero
        [void][OpenCrabs.Installer.NativeMethods]::SendMessageTimeout([IntPtr]0xffff, 0x1A, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)
    } catch {
        # A missing broadcast only means new terminals learn about the PATH
        # change at the next logon instead of immediately.
    }
}

# ---------------------------------------------------------------- main

$tag = Resolve-ReleaseTag
$asset = "$BinName-$tag-windows-$AssetArch.zip"
$baseUrl = "https://github.com/$Repo/releases/download/$tag"

if (-not $Dir) {
    $Dir = Join-Path $env:LOCALAPPDATA 'OpenCrabs\bin'
}

Write-Say "OpenCrabs $tag (windows/$AssetArch)"
Write-Say "  install to  $Dir"
Write-Say "  archive     $asset"

if ($DryRun) {
    Write-Say 'dry run: nothing downloaded, nothing installed'
    return
}

$workDir = Join-Path $env:TEMP ("opencrabs-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $workDir -Force | Out-Null

try {
    $archivePath = Join-Path $workDir $asset
    $sumsPath = Join-Path $workDir 'SHA256SUMS'

    Write-Say 'Downloading...'
    Get-RemoteFile -Url "$baseUrl/$asset" -Path $archivePath
    try {
        Get-RemoteFile -Url "$baseUrl/SHA256SUMS" -Path $sumsPath
    } catch {
        Die 'could not download SHA256SUMS, refusing to install unverified'
    }

    $expected = Get-ExpectedHash -SumsPath $sumsPath -AssetName $asset
    if (-not $expected) { Die "SHA256SUMS has no entry for $asset" }

    $actual = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        Die "checksum mismatch for $asset`n  expected $expected`n  actual   $actual`nRefusing to install. Re-run, or download it yourself from $baseUrl"
    }
    Write-Say 'Checksum OK.'

    $extractDir = Join-Path $workDir 'extract'
    Expand-Archive -LiteralPath $archivePath -DestinationPath $extractDir -Force

    $binSource = Join-Path $extractDir "$BinName.exe"
    if (-not (Test-Path -LiteralPath $binSource)) {
        Die "$asset does not contain $BinName.exe"
    }

    # Everything the archive ships, not a hand-picked subset. The MSVC runtime
    # DLLs sit beside the exes because Windows loads them from the directory the
    # exe lives in, so an install that copies only the exes produces a binary
    # that cannot start on a machine without the redistributable (#2260).
    $shipped = @(Get-ChildItem -LiteralPath $extractDir -File | Where-Object {
        $_.Extension -ieq '.exe' -or $_.Extension -ieq '.dll'
    })
    if ($shipped.Count -eq 0) { Die "$asset carries nothing to install" }

    New-Item -ItemType Directory -Path $Dir -Force | Out-Null
    $binTarget = Join-Path $Dir "$BinName.exe"
    $installed = @()
    try {
        foreach ($item in $shipped) {
            Copy-Item -LiteralPath $item.FullName -Destination (Join-Path $Dir $item.Name) -Force
            $installed += $item.Name
        }
    } catch {
        Die "could not write to $Dir - is opencrabs running? close it and re-run`n  $($_.Exception.Message)"
    }

    # Copying is not proof of arrival. Compare against the archive's own list so
    # a short install fails here, with the names, instead of in the user's first
    # run on a machine where the missing file is the whole problem.
    $missing = @($shipped | Where-Object { -not (Test-Path -LiteralPath (Join-Path $Dir $_.Name)) })
    if ($missing.Count -gt 0) {
        $names = ($missing | ForEach-Object { $_.Name }) -join ', '
        Die "$asset ships $($shipped.Count) files but the install does not have it: $names"
    }

    if (-not (Test-Path -LiteralPath (Join-Path $Dir "$RtkName.exe"))) {
        Write-Warn "this release ships no $RtkName.exe, so rebase tooling will be unavailable"
    }

    # Invoke-WebRequest sets the mark-of-the-web, which Expand-Archive can carry
    # onto the extracted files and SmartScreen would then block on first launch.
    # A blocked DLL fails exactly like a blocked exe, so unblock every one.
    foreach ($name in $installed) {
        Unblock-File -LiteralPath (Join-Path $Dir $name) -ErrorAction SilentlyContinue
    }
} finally {
    Remove-Item -LiteralPath $workDir -Recurse -Force -ErrorAction SilentlyContinue
}

$onPath = $false
foreach ($entry in ($env:Path -split ';')) {
    if ($entry -and $entry.TrimEnd('\') -ieq $Dir.TrimEnd('\')) { $onPath = $true }
}

if (-not $NoPathUpdate) {
    $userPath = Get-UserPathRaw
    $entries = @($userPath -split ';' | Where-Object { $_ -and $_.Trim() -ne '' })
    $alreadyListed = $false
    foreach ($entry in $entries) {
        if ($entry.Trim().TrimEnd('\') -ieq $Dir.TrimEnd('\')) { $alreadyListed = $true }
    }
    if (-not $alreadyListed) {
        Set-UserPathRaw -Value ((@($entries) + $Dir) -join ';')
        Send-EnvironmentChange
        Write-Say "Added $Dir to your user PATH (open a new terminal to pick it up)."
    }
    if (-not $onPath) { $env:Path = "$env:Path;$Dir" }
} elseif (-not $onPath) {
    Write-Warn "$Dir is not on your PATH. Add it:"
    Write-Warn "  `$env:Path = `"$Dir;`$env:Path`""
}

Write-Say ''
Write-Say "OpenCrabs $tag installed at $binTarget"
Write-Say "Run: $BinName"
Write-Say 'It opens straight into the chat with the built-in provider already live,'
Write-Say 'so you can start talking without a key or an account.'
Write-Say "Setup wizard: $BinName --onboard"
