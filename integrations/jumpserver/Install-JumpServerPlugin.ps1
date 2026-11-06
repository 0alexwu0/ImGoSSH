#Requires -Version 5.1
<#
.SYNOPSIS
Install/remove the ImGoSSH SSH/SFTP connectors for JumpServer Client 2.1.4+ on Windows.
.DESCRIPTION
Close JumpServer Client before running. LegacyConfigPath points to the flat
config.json used by 2.1.4/3.x. PluginRoot is the indexed platform resource
directory containing index.json used by the newer plugin implementation.
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [string]$PluginRoot,
    [string]$LegacyConfigPath,
    [string]$ImGoSSHPath,
    [string]$ConfigRoot = (Join-Path $env:APPDATA 'jumpserver-client'),
    [switch]$Uninstall,
    [ValidateSet('All', 'Ssh', 'Sftp')][string]$Connector = 'All'
)

$ErrorActionPreference = 'Stop'
$pluginId = 'io.github.imgossh'
$utf8 = New-Object System.Text.UTF8Encoding($false)

function Read-Json([string]$Path) {
    Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json
}

function Set-Property($Object, [string]$Name, $Value) {
    $Object | Add-Member -MemberType NoteProperty -Name $Name -Value $Value -Force
}

function Write-Json([string]$Path, $Value) {
    [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 50) + "`n", $utf8)
}

if ($PluginRoot -and $LegacyConfigPath) { throw 'Specify either PluginRoot or LegacyConfigPath, not both.' }
if (-not $PluginRoot -and -not $LegacyConfigPath) {
    $installRoots = @(
        (Join-Path $env:LOCALAPPDATA 'Programs\JumpServer'),
        (Join-Path $env:LOCALAPPDATA 'JumpServer'),
        (Join-Path $env:LOCALAPPDATA 'Programs\JumpServerClient'),
        (Join-Path $env:ProgramFiles 'JumpServer'),
        (Join-Path $env:ProgramFiles 'JumpServerClient')
    )
    if (${env:ProgramFiles(x86)}) {
        $installRoots += Join-Path ${env:ProgramFiles(x86)} 'JumpServerClient'
        $installRoots += Join-Path ${env:ProgramFiles(x86)} 'JumpServer'
    }
    foreach ($key in @(
        'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*'
    )) {
        Get-ItemProperty -Path $key -ErrorAction SilentlyContinue |
            Where-Object { $_.DisplayName -like '*JumpServer*' -and $_.InstallLocation } |
            ForEach-Object { $installRoots += $_.InstallLocation }
    }
    # v2.1.4 registers its Go launcher directly as the jms:// handler.
    $handler = (Get-ItemProperty -LiteralPath 'Registry::HKEY_CLASSES_ROOT\jms\shell\open\command' -ErrorAction SilentlyContinue).'(default)'
    if ($handler -match '^"([^"]+\\resources\\bin\\windows\\JumpServerClient(?:32)?\.exe)"') {
        $installRoots += Split-Path -Parent (Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $Matches[1])))
    }
    $candidates = @(
        foreach ($installRoot in $installRoots) {
            foreach ($relative in @('resources\plugins\windows', 'plugins\windows', 'windows')) {
                $candidate = Join-Path $installRoot $relative
                if (Test-Path -LiteralPath (Join-Path $candidate 'index.json') -PathType Leaf) {
                    (Resolve-Path -LiteralPath $candidate).Path
                }
            }
        }
    ) | Select-Object -Unique
    $legacyCandidates = @(
        foreach ($installRoot in $installRoots) {
            $candidate = Join-Path $installRoot 'resources\bin\config.json'
            if (Test-Path -LiteralPath $candidate -PathType Leaf) { (Resolve-Path -LiteralPath $candidate).Path }
        }
        # v3 moved the same config format to the user configuration directory.
        $userConfigPath = Join-Path $ConfigRoot 'config.json'
        if (Test-Path -LiteralPath $userConfigPath -PathType Leaf) {
            $userConfig = Read-Json $userConfigPath
            if ($userConfig.windows.terminal -is [array] -and -not $userConfig._plugins.enabled) {
                (Resolve-Path -LiteralPath $userConfigPath).Path
            }
        }
    ) | Select-Object -Unique
    if (@($candidates).Count -eq 0 -and @($legacyCandidates).Count -eq 1) {
        $LegacyConfigPath = @($legacyCandidates)[0]
    } elseif (@($legacyCandidates).Count -gt 0 -or @($candidates).Count -ne 1) {
        throw 'Cannot identify one client configuration. Specify -LegacyConfigPath (2.1.4 config.json) or -PluginRoot (indexed plugins).'
    } else { $PluginRoot = @($candidates)[0] }
}

if ($LegacyConfigPath) {
    $legacyArguments = @{ LegacyConfigPath = $LegacyConfigPath; ImGoSSHPath = $ImGoSSHPath; Uninstall = $Uninstall }
    if ($WhatIfPreference) { $legacyArguments.WhatIf = $true }
    & (Join-Path $PSScriptRoot 'Install-LegacyConnector.ps1') @legacyArguments
    return
}

if ($Connector -eq 'All') {
    foreach ($kind in @('Ssh', 'Sftp')) {
        $arguments = @{ PluginRoot = $PluginRoot; ConfigRoot = $ConfigRoot; ImGoSSHPath = $ImGoSSHPath; Uninstall = $Uninstall; Connector = $kind }
        if ($WhatIfPreference) { $arguments.WhatIf = $true }
        & $PSCommandPath @arguments
    }
    return
}
$sourceDir = 'plugin'
$selectionKey = 'terminal:ssh'
$pluginCategory = 'terminal'
$pluginName = 'imgossh'
if ($Connector -eq 'Sftp') {
    $pluginId = 'io.github.imgossh.sftp'
    $sourceDir = 'plugin-sftp'
    $selectionKey = 'filetransfer:sftp'
    $pluginCategory = 'filetransfer'
    $pluginName = 'imgossh_sftp'
}
$PluginRoot = (Resolve-Path -LiteralPath $PluginRoot).Path
$ConfigRoot = [IO.Path]::GetFullPath($ConfigRoot)
$indexPath = Join-Path $PluginRoot 'index.json'
$index = Read-Json $indexPath
if (-not $index.PSObject.Properties['plugins'] -or $index.plugins -isnot [array]) {
    throw 'Invalid platform plugin index: expected a plugins array.'
}
$statePath = Join-Path $ConfigRoot 'plugins-state.json'
$state = if (Test-Path -LiteralPath $statePath -PathType Leaf) {
    Read-Json $statePath
} else {
    $defaults = Join-Path $PluginRoot 'plugins-state.defaults.json'
    if (Test-Path -LiteralPath $defaults -PathType Leaf) { Read-Json $defaults }
    else { [pscustomobject]@{ version = 1; selections = [pscustomobject]@{}; plugins = [pscustomobject]@{} } }
}
foreach ($field in @('selections', 'plugins')) {
    if (-not $state.PSObject.Properties[$field]) { Set-Property $state $field ([pscustomobject]@{}) }
    if ($state.$field -isnot [pscustomobject]) { throw "Invalid plugins-state.json: $field must be an object." }
}

$pluginDir = [IO.Path]::GetFullPath((Join-Path $PluginRoot $pluginId))
# Verify the exact deletion/copy target before any recursive filesystem operation.
if ([IO.Path]::GetDirectoryName($pluginDir) -ne $PluginRoot.TrimEnd('\', '/')) {
    throw 'Plugin destination must be a direct child of PluginRoot.'
}
$restorePath = Join-Path $pluginDir 'imgossh-restore.json'
$installed = Test-Path -LiteralPath $pluginDir -PathType Container
if ($installed -and -not (Test-Path -LiteralPath $restorePath -PathType Leaf)) {
    throw 'An unmanaged plugin already exists at the destination; refusing to overwrite or remove it.'
}
$restore = if ($installed) { Read-Json $restorePath } else {
    [pscustomobject]@{
        selection_exists = [bool]$state.selections.PSObject.Properties[$selectionKey]
        selection = $state.selections.$selectionKey
        plugin_exists = [bool]$state.plugins.PSObject.Properties[$pluginId]
        plugin = $state.plugins.$pluginId
    }
}

if (-not $Uninstall) {
    if (-not $ImGoSSHPath) {
        throw 'Specify -ImGoSSHPath with the new imgossh.exe (build this source before installing).'
    }
    $ImGoSSHPath = (Resolve-Path -LiteralPath $ImGoSSHPath).Path
    if (-not (Test-Path -LiteralPath $ImGoSSHPath -PathType Leaf)) { throw 'ImGoSSHPath must be a file.' }
    # Parse and validate all inputs before writing any client files.
    $manifest = Read-Json (Join-Path $PSScriptRoot ($sourceDir + '\manifest.json'))
    $connect = Read-Json (Join-Path $PSScriptRoot ($sourceDir + '\connect.json'))
    if ($manifest.id -ne $pluginId -or $connect.platform -ne 'windows') { throw 'Unexpected connector definition.' }
} elseif (-not $installed) {
    if ($Connector -eq 'Sftp') { return }
    throw 'ImGoSSH connector is not installed by this script.'
}

$action = if ($Uninstall) { 'Remove ImGoSSH connector' } else { "Install ImGoSSH connector and select it for $Connector" }
if (-not $PSCmdlet.ShouldProcess("$PluginRoot; $ConfigRoot", $action)) { return }

# Backups also cover partial failure (e.g. insufficient access to Program Files).
$backupDir = Join-Path $ConfigRoot ('imgossh-backups\' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $backupDir -Force | Out-Null
Copy-Item -LiteralPath $indexPath -Destination (Join-Path $backupDir 'index.json')
if (Test-Path -LiteralPath $statePath -PathType Leaf) {
    Copy-Item -LiteralPath $statePath -Destination (Join-Path $backupDir 'plugins-state.json')
}
if ($installed) { Copy-Item -LiteralPath $pluginDir -Destination (Join-Path $backupDir 'plugin') -Recurse }

try {
    $entries = @($index.plugins | Where-Object { $_.id -ne $pluginId })
    if ($Uninstall) {
        if ($state.selections.$selectionKey -eq $pluginId) {
            if ($restore.selection_exists) { Set-Property $state.selections $selectionKey $restore.selection }
            else { $state.selections.PSObject.Properties.Remove($selectionKey) }
        }
        if ($restore.plugin_exists) { Set-Property $state.plugins $pluginId $restore.plugin }
        else { $state.plugins.PSObject.Properties.Remove($pluginId) }
    } else {
        New-Item -ItemType Directory -Path $pluginDir -Force | Out-Null
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot ($sourceDir + '\manifest.json')) -Destination $pluginDir
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot ($sourceDir + '\connect.json')) -Destination $pluginDir
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'plugin\icon.png') -Destination $pluginDir
        Write-Json $restorePath $restore
        $entries += [pscustomobject]@{ id = $pluginId; name = $pluginName; category = $pluginCategory }
        Set-Property $state.plugins $pluginId ([pscustomobject]@{ enabled = $true; path = $ImGoSSHPath })
        Set-Property $state.selections $selectionKey $pluginId
    }
    Set-Property $index 'plugins' @($entries)
    Write-Json $indexPath $index
    Write-Json $statePath $state
    if ($Uninstall) { Remove-Item -LiteralPath $pluginDir -Recurse -Force }
} catch {
    Copy-Item -LiteralPath (Join-Path $backupDir 'index.json') -Destination $indexPath -Force
    $stateBackup = Join-Path $backupDir 'plugins-state.json'
    if (Test-Path -LiteralPath $stateBackup) { Copy-Item -LiteralPath $stateBackup -Destination $statePath -Force }
    elseif (Test-Path -LiteralPath $statePath) { Remove-Item -LiteralPath $statePath -Force }
    if (Test-Path -LiteralPath $pluginDir) { Remove-Item -LiteralPath $pluginDir -Recurse -Force }
    if ($installed) { Copy-Item -LiteralPath (Join-Path $backupDir 'plugin') -Destination $pluginDir -Recurse }
    throw
}
Write-Output "$action complete. Restart JumpServer Client. Backup: $backupDir"
