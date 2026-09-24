# =============================================================================
# File:        scripts/install/install.ps1
# Description: Installs TentaFlow on Windows x86_64 as a Windows service that
#              starts with the system - the counterpart of install.sh.
#
# Usage (PowerShell started as Administrator):
#   irm https://raw.githubusercontent.com/Slyb00ts/TentaFlow/main/scripts/install/install.ps1 | iex
#
# Layout (the same contract as install.sh and `tentaflow update`):
#   %ProgramFiles%\TentaFlow\versions\<ver>   binaries + bundled libraries
#   %ProgramFiles%\TentaFlow\current          junction to the live version
#   %ProgramData%\TentaFlow\config.toml       configuration, never overwritten
#   %ProgramData%\TentaFlow\install-receipt.json  what was installed and how
#   %ProgramData%\TentaFlow\data              TENTAFLOW_HOME (logs in data\logs)
#
# Environment overrides:
#   TENTAFLOW_EDITION=full|slim           skip the interactive question
#   TENTAFLOW_VARIANT=vulkan|cuda13       GPU backend of the full edition
#   TENTAFLOW_VERSION=v0.3.0              install a specific version
#   TENTAFLOW_BIND=0.0.0.0:8090           listen address (default 0.0.0.0:8090)
#   TENTAFLOW_PREFIX=C:\Program Files\TentaFlow
#   TENTAFLOW_ASSET_FILE=C:\path\x.zip    install a local archive (CI / offline)
#   TENTAFLOW_NO_AUTOSTART=1              do not register or start the service
#   TENTAFLOW_SKIP_DEPS=1                 do not install the GStreamer runtime
# =============================================================================

$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 still offers TLS 1.0 first; GitHub requires 1.2.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
# The progress bar makes Invoke-WebRequest in PowerShell 5.1 many times slower.
$ProgressPreference = 'SilentlyContinue'

$Repo        = 'Slyb00ts/TentaFlow'
$Target      = 'x86_64-pc-windows-msvc'
$ServiceName = 'TentaFlow'
$Version     = if ($env:TENTAFLOW_VERSION) { $env:TENTAFLOW_VERSION } else { 'latest' }
$Edition     = "$env:TENTAFLOW_EDITION"
$Variant     = "$env:TENTAFLOW_VARIANT"
$Bind        = if ($env:TENTAFLOW_BIND) { $env:TENTAFLOW_BIND } else { '0.0.0.0:8090' }
$Prefix      = if ($env:TENTAFLOW_PREFIX) { $env:TENTAFLOW_PREFIX } else { Join-Path $env:ProgramFiles 'TentaFlow' }
$AssetFile   = "$env:TENTAFLOW_ASSET_FILE"
$NoAutostart = $env:TENTAFLOW_NO_AUTOSTART -eq '1'
$SkipDeps    = $env:TENTAFLOW_SKIP_DEPS -eq '1'
$DataRoot    = Join-Path $env:ProgramData 'TentaFlow'
$Config      = Join-Path $DataRoot 'config.toml'
$HomeDir     = Join-Path $DataRoot 'data'
$Receipt     = Join-Path $DataRoot 'install-receipt.json'
$Current     = Join-Path $Prefix 'current'
$Exe         = Join-Path $Current 'tentaflow.exe'

function Log($msg)  { Write-Host "==> $msg" -ForegroundColor Cyan }
function Ok($msg)   { Write-Host "  ok $msg" -ForegroundColor Green }
function Warn($msg) { Write-Host "  !! $msg" -ForegroundColor Yellow }
function Die($msg)  { Write-Host "  xx $msg" -ForegroundColor Red; throw $msg }

# Windows PowerShell 5.1 turns every stderr line of a native command whose
# stderr is redirected into an error record, and $ErrorActionPreference='Stop'
# makes that fatal even when the tool exits 0 - net.exe alone complains about a
# group the account is already in. These tools answer through their exit code
# ($LASTEXITCODE survives), so their stderr is dropped with the preference
# relaxed for the call.
function Invoke-Native([string]$File, [string[]]$Arguments) {
    $saved = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $File @Arguments 2>$null } finally { $ErrorActionPreference = $saved }
}

# =============================================================================
# Preconditions
# =============================================================================
function Assert-Supported {
    if (-not [Environment]::Is64BitOperatingSystem -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
        Die 'TentaFlow for Windows is built for x86_64 only.'
    }
    # Junctions, the NetSecurity cmdlets and the service environment the
    # installer relies on are all there from Windows 10 1809 / Server 2019.
    if ([Environment]::OSVersion.Version.Build -lt 17763) {
        Die 'Windows 10 1809 / Windows Server 2019 or newer is required.'
    }
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        Die ('The installer writes to Program Files, registers a service and firewall rules: ' +
             'start PowerShell with "Run as administrator" and run it again.')
    }
}

# =============================================================================
# Edition and GPU variant
# =============================================================================
# Hardware detection PROPOSES; the user decides, exactly as in install.sh.
# Windows ships one CUDA build, on the CUDA 13 line: it needs sm_75+ and a
# driver of the 580 series or newer. Anything else NVIDIA runs on Vulkan.
function Get-Proposal {
    $smi = Get-Command 'nvidia-smi.exe' -ErrorAction SilentlyContinue
    if ($smi) {
        # Collected before it is cut: Select-Object -First stops the pipeline
        # upstream, and the exit code of a stopped call is not the tool's.
        $rows = @(Invoke-Native $smi.Source @('--query-gpu=name,compute_cap,driver_version', '--format=csv,noheader'))
        $line = $rows | Select-Object -First 1
        if ($LASTEXITCODE -eq 0 -and $line) {
            $parts = $line -split ',\s*'
            $cc = [double]::Parse($parts[1], [Globalization.CultureInfo]::InvariantCulture)
            $driver = [int](($parts[2] -split '\.')[0])
            $variant = 'vulkan'
            if ($cc -ge 7.5 -and $driver -ge 580) { $variant = 'cuda13' }
            return @{ Gpu = "NVIDIA: $($parts[0])"; Edition = 'full'; Variant = $variant }
        }
    }
    $adapters = @(Get-CimInstance Win32_VideoController -ErrorAction SilentlyContinue |
        Where-Object { $_.AdapterCompatibility -and $_.AdapterCompatibility -notmatch 'Microsoft' })
    if ($adapters.Count -gt 0) {
        return @{ Gpu = "$($adapters[0].Name) (Vulkan)"; Edition = 'full'; Variant = 'vulkan' }
    }
    return @{ Gpu = 'no GPU'; Edition = 'slim'; Variant = 'none' }
}

function Select-Edition($proposal) {
    if ($Edition) {
        if ($Edition -notin @('full', 'slim')) { Die "Unknown edition '$Edition' - set TENTAFLOW_EDITION=full or slim." }
        Ok "Edition from TENTAFLOW_EDITION: $Edition"
        return $Edition
    }
    # A CI step or a scheduled job has no one to answer; guessing an edition
    # for them would install something nobody chose.
    if ([Console]::IsInputRedirected -or -not [Environment]::UserInteractive) {
        Die 'No console to choose the edition in. Set TENTAFLOW_EDITION=full or slim.'
    }
    Write-Host ''
    Write-Host "  Detected: $($proposal.Gpu)"
    Write-Host ''
    Write-Host '  full  - llama.cpp, whisper, vision, TTS and the local engines.'
    Write-Host '  slim  - gateway, mesh, flows and dashboard, no local engines.'
    Write-Host '          Keeps cloud providers and the tool containers.'
    Write-Host ''
    Write-Host "  Hardware proposal: $($proposal.Edition) (variant: $($proposal.Variant)). The choice is yours."
    while ($true) {
        $answer = (Read-Host '  Type full or slim (required)').Trim()
        if ($answer -in @('full', 'slim')) { break }
        if ($answer) { Write-Host "  Unknown edition '$answer' (full or slim)." }
        else { Write-Host '  A choice is required; an empty Enter does not start the installation.' }
    }
    Ok "Chosen edition: $answer"
    return $answer
}

function Select-Variant($chosenEdition, $proposal) {
    if ($chosenEdition -eq 'slim') { return 'none' }
    $chosen = $Variant
    if (-not $chosen) {
        $chosen = $proposal.Variant
        if ($chosen -eq 'none') { $chosen = 'vulkan' }
    }
    if ($chosen -notin @('vulkan', 'cuda13')) { Die "Unknown variant '$chosen' (Windows: vulkan or cuda13)." }
    Ok "Variant: $chosen"
    return $chosen
}

# =============================================================================
# Download and verify
# =============================================================================
function Resolve-Version {
    if ($Version -ne 'latest') { return $Version }
    Log 'Resolving the newest version'
    # /releases, not /releases/latest: the latter hides pre-releases, and every
    # tag so far carries a pre-release suffix.
    $releases = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases?per_page=10" -UseBasicParsing
    $tag = @($releases | Where-Object { -not $_.draft } | Select-Object -First 1).tag_name
    if (-not $tag) { Die 'Could not resolve a version from the GitHub API (60 requests/h per IP?).' }
    Ok "Version: $tag"
    return $tag
}

function Get-FileSha256($path) {
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
}

function Get-Archive($work, $chosenEdition, $chosenVariant) {
    $archive = Join-Path $work 'tentaflow.zip'
    $sumFile = "$archive.sha256"
    if ($AssetFile) {
        Log "Using a local archive: $AssetFile"
        Copy-Item -LiteralPath $AssetFile -Destination $archive
        if (Test-Path -LiteralPath "$AssetFile.sha256") { Copy-Item -LiteralPath "$AssetFile.sha256" -Destination $sumFile }
    } else {
        $tag = Resolve-Version
        if ($chosenEdition -eq 'slim') { $name = "tentaflow-$tag-$Target-slim.zip" }
        else { $name = "tentaflow-$tag-$Target-full-$chosenVariant.zip" }
        $url = "https://github.com/$Repo/releases/download/$tag/$name"
        Log "Downloading $name"
        Invoke-WebRequest -Uri $url -OutFile $archive -UseBasicParsing
        try { Invoke-WebRequest -Uri "$url.sha256" -OutFile $sumFile -UseBasicParsing }
        catch { Die "No .sha256 file for $name." }
    }
    # Unverified would mean trusting every later update to the network, too.
    if (-not (Test-Path -LiteralPath $sumFile)) { Die 'The archive has no checksum.' }
    $expected = ((Get-Content -LiteralPath $sumFile -Raw) -split '\s+')[0].ToLowerInvariant()
    $actual = Get-FileSha256 $archive
    if ($expected -ne $actual) { Die "Checksum mismatch (expected $expected, got $actual)." }
    Ok 'Checksum OK'
    return $archive
}

# =============================================================================
# GStreamer runtime (full edition)
# =============================================================================
# The camera and video pipeline links GStreamer, so a full binary does not even
# start without it. The archive names the exact runtime it was built against
# (gstreamer.json); it is installed machine-wide, runtime only, because the
# service account cannot see a per-user install.
# The Inno Setup AppId of the GStreamer MSVC x86_64 installer. Its uninstall
# entry records where the runtime really is: an upgrade keeps the directory of
# the installation it replaces, whatever /DIR asks for.
$GstreamerUninstallKey = 'SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\c20a66dc-b249-4e6d-a68a-d0f836b2b3cf_is1'

function Get-GstreamerRoot {
    $entry = Get-ItemProperty -LiteralPath "HKLM:\$GstreamerUninstallKey" -ErrorAction SilentlyContinue
    if ($entry -and $entry.InstallLocation) { return $entry.InstallLocation.TrimEnd('\') }
    return (Join-Path $env:ProgramFiles 'gstreamer\1.0\msvc_x86_64')
}

# The GStreamer DLLs carry no version resource, so the version is the one its
# installer registered, trusted only while the files are where it says.
function Get-GstreamerVersion($root) {
    $entry = Get-ItemProperty -LiteralPath "HKLM:\$GstreamerUninstallKey" -ErrorAction SilentlyContinue
    if (-not $entry -or -not (Test-Path -LiteralPath (Join-Path $root 'bin\gstreamer-1.0-0.dll'))) { return $null }
    return $entry.DisplayVersion
}

function Install-Gstreamer($releaseDir) {
    $spec = Join-Path $releaseDir 'gstreamer.json'
    if (-not (Test-Path -LiteralPath $spec)) { return $null }
    $want = Get-Content -LiteralPath $spec -Raw | ConvertFrom-Json
    $root = Get-GstreamerRoot
    $have = Get-GstreamerVersion $root
    # Within 1.x the ABI only grows, so the minor the build linked against, or
    # a newer one, runs it.
    if ($have) {
        $haveParts = $have -split '\.'
        $wantParts = "$($want.version)" -split '\.'
        if ([int]$haveParts[0] -eq [int]$wantParts[0] -and [int]$haveParts[1] -ge [int]$wantParts[1]) {
            Ok "GStreamer runtime $have already installed"
            return $root
        }
    }
    if ($SkipDeps) {
        Die "GStreamer $($want.version) runtime is required and TENTAFLOW_SKIP_DEPS=1 forbids installing it."
    }
    Log "Installing the GStreamer $($want.version) runtime (MSVC x86_64)"
    $setup = Join-Path $env:TEMP "gstreamer-$($want.version)-$PID.exe"
    $setupLog = Join-Path $env:TEMP "gstreamer-$($want.version)-$PID.log"
    Invoke-WebRequest -Uri $want.url -OutFile $setup -UseBasicParsing
    try {
        $actual = Get-FileSha256 $setup
        if ($actual -ne "$($want.sha256)".ToLowerInvariant()) {
            Die "GStreamer installer checksum mismatch (expected $($want.sha256), got $actual)."
        }
        # /TASKS="" leaves the machine environment alone: the installer below
        # puts GStreamer on PATH itself and on the service's own PATH.
        # Start-Process joins these with spaces and quotes nothing, so a path
        # with a space carries its own quotes.
        $proc = Start-Process -FilePath $setup -Wait -PassThru -ArgumentList @(
            '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/ALLUSERS', '/TYPE=runtime',
            "/DIR=`"$root`"", '/TASKS=""', "/LOG=`"$setupLog`"")
        if ($proc.ExitCode -ne 0) { Die "The GStreamer installer failed (exit $($proc.ExitCode)); log: $setupLog" }
    } finally {
        Remove-Item -LiteralPath $setup -Force -ErrorAction SilentlyContinue
    }
    $root = Get-GstreamerRoot
    if (-not (Get-GstreamerVersion $root)) {
        # Where the installer says it went is the one fact worth showing.
        $where = @(Select-String -LiteralPath $setupLog -Pattern 'install mode|Dest filename: .*gstreamer-1\.0-0\.dll' -ErrorAction SilentlyContinue |
            ForEach-Object { $_.Line.Substring([Math]::Min(24, $_.Line.Length)) }) -join '; '
        Die "GStreamer is not in $root after its installer finished ($where). Log: $setupLog"
    }
    Remove-Item -LiteralPath $setupLog -Force -ErrorAction SilentlyContinue
    Ok "GStreamer $($want.version) runtime installed in $root"
    return $root
}

# =============================================================================
# Files
# =============================================================================
function Add-MachinePath($dir) {
    $path = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $entries = @($path -split ';' | Where-Object { $_ })
    if ($entries -notcontains $dir) {
        [Environment]::SetEnvironmentVariable('Path', (($entries + $dir) -join ';'), 'Machine')
    }
    if (-not (($env:Path -split ';') -contains $dir)) { $env:Path = "$env:Path;$dir" }
}

function Stop-TentaflowService {
    $service = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
    if ($service -and $service.Status -ne 'Stopped') {
        Log 'Stopping the running service before the swap'
        # A running server holds its DLLs open; the version directory must not
        # change underneath it.
        Stop-Service -Name $ServiceName -Force
        $service.WaitForStatus('Stopped', [TimeSpan]::FromMinutes(3))
    }
}

function Install-Files($archive, $work) {
    Log 'Unpacking'
    $unpacked = Join-Path $work 'unpacked'
    Expand-Archive -LiteralPath $archive -DestinationPath $unpacked
    $inner = Get-ChildItem -LiteralPath $unpacked -Directory | Where-Object { $_.Name -like 'tentaflow-*' } | Select-Object -First 1
    if (-not $inner) { Die 'The archive has an unexpected structure.' }
    return $inner.FullName
}

function Publish-Version($releaseDir) {
    # The version comes from the binary itself, so a local archive (CI,
    # offline) lands in a correctly named directory without trusting the file
    # name. The binary starts only once its runtime DLLs are reachable.
    $versionLine = Invoke-Native (Join-Path $releaseDir 'tentaflow.exe') @('--version')
    if ($LASTEXITCODE -ne 0 -or -not $versionLine) {
        Die ("Cannot read the version from the binary (exit {0}; 0xC0000135 means a DLL it links is missing)." -f $LASTEXITCODE)
    }
    $ver = ("$versionLine".Trim() -split '\s+')[-1]
    $versionDir = Join-Path $Prefix "versions\$ver"
    New-Item -ItemType Directory -Force -Path (Join-Path $Prefix 'versions') | Out-Null
    if (Test-Path -LiteralPath $versionDir) { Remove-Item -LiteralPath $versionDir -Recurse -Force }
    Move-Item -LiteralPath $releaseDir -Destination $versionDir
    # A move keeps the access rules of %TEMP% in the installing admin's
    # profile, which the service account cannot read ("Access is denied" at
    # start). Reset makes the tree inherit what Program Files grants.
    & icacls $versionDir /reset /T /Q | Out-Null
    if ($LASTEXITCODE -ne 0) { Die "Could not reset the access rules of $versionDir (icacls exit $LASTEXITCODE)." }

    # Windows cannot rename a directory entry over another, so the junction is
    # exchanged by two renames (the same as `tentaflow update`), with the old
    # one put back if the second fails. The service is stopped meanwhile.
    $staged = Join-Path $Prefix 'current.new'
    $retired = Join-Path $Prefix 'current.old'
    foreach ($stale in @($staged, $retired)) {
        if (Test-Path -LiteralPath $stale) { (Get-Item -LiteralPath $stale -Force).Delete() }
    }
    New-Item -ItemType Junction -Path $staged -Target $versionDir | Out-Null
    $hadCurrent = Test-Path -LiteralPath $Current
    if ($hadCurrent) { Rename-Item -LiteralPath $Current -NewName 'current.old' }
    try {
        Rename-Item -LiteralPath $staged -NewName 'current'
    } catch {
        if ($hadCurrent) { Rename-Item -LiteralPath $retired -NewName 'current' }
        throw
    }
    # Deleting the junction item removes the link, never the version it names.
    if ($hadCurrent) { (Get-Item -LiteralPath $retired -Force).Delete() }
    Add-MachinePath $Current
    Ok "Installed $ver in $versionDir"
    return $ver
}

function Write-Config {
    New-Item -ItemType Directory -Force -Path $DataRoot, $HomeDir | Out-Null
    if (Test-Path -LiteralPath $Config) {
        Ok "Keeping the existing configuration: $Config"
        Warn 'TENTAFLOW_BIND does not change an existing file.'
        return
    }
    Log "Writing the configuration ($Bind, mesh on by default)"
    # The binary owns the config schema; composing TOML here would drift.
    & $Exe init-config --output $Config --bind $Bind
    if ($LASTEXITCODE -ne 0) { Die "init-config failed (exit $LASTEXITCODE)." }
}

# Ports come from the configuration the service will actually read, which may
# be an older file than this run's TENTAFLOW_BIND.
function Get-ConfigPorts {
    $ports = @{ Api = ($Bind -split ':')[-1]; Mesh = $null; MeshOn = $false }
    $section = ''
    foreach ($line in Get-Content -LiteralPath $Config) {
        $l = $line.Trim()
        if ($l -match '^\[(.+)\]$') { $section = $Matches[1]; continue }
        if ($section -eq 'protocols.openai_api' -and $l -match '^bind\s*=\s*"([^"]+)"') { $ports.Api = ($Matches[1] -split ':')[-1] }
        if ($section -eq 'mesh' -and $l -match '^port\s*=\s*(\d+)') { $ports.Mesh = $Matches[1] }
        if ($section -eq 'mesh' -and $l -match '^enabled\s*=\s*(true|false)') { $ports.MeshOn = $Matches[1] -eq 'true' }
    }
    return $ports
}

function Set-Firewall($ports) {
    # Port rules, as install.sh opens on Linux: a program rule would name the
    # binary through the `current` junction, and the path Windows Filtering
    # Platform matches is not guaranteed to be the one before the junction.
    # The service also has no desktop for the server's own UAC-driven check.
    Log 'Windows Firewall rules'
    Get-NetFirewallRule -Group 'TentaFlow' -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    $rules = @(
        @{ Name = "TentaFlow HTTPS (TCP $($ports.Api))"; Protocol = 'TCP'; Port = $ports.Api },
        @{ Name = "TentaFlow QUIC (UDP $($ports.Api))"; Protocol = 'UDP'; Port = $ports.Api }
    )
    if ($ports.MeshOn -and $ports.Mesh -and $ports.Mesh -ne $ports.Api) {
        $rules += @{ Name = "TentaFlow mesh (UDP $($ports.Mesh))"; Protocol = 'UDP'; Port = $ports.Mesh }
    }
    foreach ($rule in $rules) {
        New-NetFirewallRule -DisplayName $rule.Name -Group 'TentaFlow' -Direction Inbound -Action Allow `
            -Protocol $rule.Protocol -LocalPort $rule.Port -Profile Any -Enabled True | Out-Null
        Ok $rule.Name
    }
}

function Write-Receipt($ver, $chosenEdition, $chosenVariant, $scope) {
    # Field names are the wire contract with tentaflow/src/receipt.rs.
    $data = [ordered]@{
        version       = $ver
        edition       = $chosenEdition
        variant       = $chosenVariant
        target        = $Target
        prefix        = $Prefix
        config        = $Config
        home          = $HomeDir
        service_scope = $scope
    }
    [IO.File]::WriteAllText($Receipt, ($data | ConvertTo-Json), (New-Object Text.UTF8Encoding($false)))
    Ok "Receipt: $Receipt"
}

# A new directory under ProgramData lets every user read it and create files
# in it; the database, the TLS key and the stored secrets belong to the service.
# Only the config and the receipt stay readable to Users, as /etc is on Linux,
# so `tentaflow status` works without elevation. Built-in principals go by SID:
# their names are localized.
function Protect-DataRoot($account) {
    $grants = @('*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F')
    if ($account) { $grants += "${account}:(OI)(CI)M" }
    & icacls $DataRoot /inheritance:r /grant:r @grants /Q | Out-Null
    if ($LASTEXITCODE -ne 0) { Die "Could not set the access rules of $DataRoot (icacls exit $LASTEXITCODE)." }
    foreach ($readable in @($Config, $Receipt)) {
        & icacls $readable /grant '*S-1-5-32-545:R' /Q | Out-Null
        if ($LASTEXITCODE -ne 0) { Die "Could not make $readable readable (icacls exit $LASTEXITCODE)." }
    }
}

# =============================================================================
# Service
# =============================================================================
# A membership only widens what the service can see, so a refusal is reported
# with its consequence instead of aborting an otherwise working installation.
# The listing decides, not the exit code: net.exe exits 2 for every failure,
# "already a member" on a reinstall included.
function Add-GroupMember($group, $account, $consequence) {
    Invoke-Native 'net.exe' @('localgroup', $group, $account, '/add') | Out-Null
    $members = @(Invoke-Native 'net.exe' @('localgroup', $group))
    if ($members -notcontains $account) { Warn "Could not add $account to '$group': $consequence." }
}

function Register-TentaflowService {
    $account = "NT SERVICE\$ServiceName"
    $binPath = "`"$Exe`" --windows-service --config `"$Config`" --home `"$HomeDir`""
    if (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue) {
        & sc.exe config $ServiceName binPath= $binPath start= auto | Out-Null
    } else {
        New-Service -Name $ServiceName -BinaryPathName $binPath -DisplayName 'TentaFlow' `
            -Description 'TentaFlow node: API gateway, mesh, flows and local inference.' -StartupType Automatic | Out-Null
    }
    # A virtual account: its own identity with no password to manage and none
    # of LocalSystem's rights, the Windows counterpart of the Linux service user.
    & sc.exe config $ServiceName obj= $account | Out-Null
    if ($LASTEXITCODE -ne 0) { Die "Could not set the service account (sc.exe exit $LASTEXITCODE)." }
    # Restart after a crash and after a stop reported as a failure.
    & sc.exe failure $ServiceName reset= 86400 actions= restart/5000/restart/15000/restart/60000 | Out-Null
    & sc.exe failureflag $ServiceName 1 | Out-Null

    Protect-DataRoot $account
    # GPU and disk counters (PDH) are readable by Performance Monitor Users;
    # the group is named by SID because its name is localized.
    $perfGroup = (New-Object Security.Principal.SecurityIdentifier('S-1-5-32-558')).Translate([Security.Principal.NTAccount]).Value.Split('\')[-1]
    Add-GroupMember $perfGroup $account 'GPU and disk metrics will be missing'
    # Docker Desktop hands its engine pipe to this group only.
    if (Get-LocalGroup -Name 'docker-users' -ErrorAction SilentlyContinue) {
        Add-GroupMember 'docker-users' $account 'Docker deployments will be refused'
    }

    # The Service Control Manager keeps the environment it read at boot, so the
    # PATH entries this run added (GStreamer, current) would reach the service
    # only after a reboot. The service gets its own copy of the machine PATH.
    $servicePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    Set-ItemProperty -Path "HKLM:\SYSTEM\CurrentControlSet\Services\$ServiceName" -Name 'Environment' `
        -Type MultiString -Value @("PATH=$servicePath")
    Ok "Service $ServiceName registered (account $account, automatic start)"
}

function Wait-Healthy($port) {
    Log 'Starting the service'
    try {
        Start-Service -Name $ServiceName
    } catch {
        # Start-Service says only "cannot start"; the reason (logon refused,
        # access denied, the process ending early) is the innermost exception.
        $reason = $_.Exception
        while ($reason.InnerException) { $reason = $reason.InnerException }
        Die "The service did not start: $($reason.Message) Log: $(Join-Path $HomeDir 'logs')"
    }
    (Get-Service -Name $ServiceName).WaitForStatus('Running', [TimeSpan]::FromMinutes(1))
    # First start generates the TLS identity and the database before the socket
    # opens. curl.exe ships with Windows and takes the self-signed certificate.
    for ($i = 0; $i -lt 90; $i++) {
        Invoke-Native 'curl.exe' @('-fsk', "https://127.0.0.1:$port/health") | Out-Null
        if ($LASTEXITCODE -eq 0) { Ok "The server answers on port $port"; return }
        if ((Get-Service -Name $ServiceName).Status -ne 'Running') { break }
        Start-Sleep -Seconds 2
    }
    Die "The service does not answer /health. Log: $(Join-Path $HomeDir 'logs')"
}

# =============================================================================
# Run
# =============================================================================
Write-Host ''
Write-Host 'TentaFlow installer' -ForegroundColor White
Write-Host "  system:  $((Get-CimInstance Win32_OperatingSystem).Caption)"
Write-Host "  prefix:  $Prefix"
Write-Host "  data:    $DataRoot"

Assert-Supported
$proposal = Get-Proposal
$chosenEdition = Select-Edition $proposal
$chosenVariant = Select-Variant $chosenEdition $proposal

$work = Join-Path $env:TEMP "tentaflow-install-$PID"
New-Item -ItemType Directory -Force -Path $work | Out-Null
try {
    $archive = Get-Archive $work $chosenEdition $chosenVariant
    $releaseDir = Install-Files $archive $work
    # Before GStreamer: its installer cannot replace DLLs a running server
    # holds open.
    Stop-TentaflowService
    $gstreamerRoot = Install-Gstreamer $releaseDir
    if ($gstreamerRoot) {
        $gstBin = Join-Path $gstreamerRoot 'bin'
        # On PATH for the CLI; the service gets its own copy (see above).
        Add-MachinePath $gstBin
    }
    $installed = Publish-Version $releaseDir
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Config
$ports = Get-ConfigPorts
Set-Firewall $ports
$scope = 'system'
if ($NoAutostart) { $scope = 'none' }
Write-Receipt $installed $chosenEdition $chosenVariant $scope
if ($NoAutostart) {
    Warn 'Skipping service registration (TENTAFLOW_NO_AUTOSTART=1).'
    Protect-DataRoot $null
} else {
    Register-TentaflowService
    Wait-Healthy $ports.Api
}

$port = $ports.Api
Write-Host ''
Write-Host 'Done.' -ForegroundColor Green
Write-Host "  binary:    $Exe"
Write-Host "  version:   $installed ($chosenEdition/$chosenVariant)"
Write-Host "  dashboard: https://localhost:$port"
Write-Host "  config:    $Config"
Write-Host "  logs:      $(Join-Path $HomeDir 'logs')"
Write-Host ''
Write-Host '  First login: admin / admin - change the password right after signing in.' -ForegroundColor White
if ((Get-Content -LiteralPath $Config -Raw) -match 'bind\s*=\s*"0\.0\.0\.0:') {
    Warn 'The server listens on every interface with the default password - change it NOW.'
}
Write-Host ''
Write-Host '  tentaflow status     service state, autostart, health'
Write-Host '  tentaflow stop|start stop / start the service (as Administrator)'
Write-Host '  tentaflow update     update from GitHub Releases (as Administrator)'
Write-Host '  open a NEW terminal for `tentaflow` to be on PATH'
Write-Host ''
