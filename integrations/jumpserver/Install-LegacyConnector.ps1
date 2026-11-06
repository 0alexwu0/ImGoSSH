#Requires -Version 5.1
# JumpServer Client 2.1.4 uses resources/bin/config.json, not indexed plugins.
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [Parameter(Mandatory = $true)][string]$LegacyConfigPath,
    [string]$ImGoSSHPath,
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'
$utf8 = New-Object System.Text.UTF8Encoding($false)
function Read-Json([string]$Path) { Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json }
function Write-Json([string]$Path, $Value) { [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 50) + "`n", $utf8) }

$LegacyConfigPath = (Resolve-Path -LiteralPath $LegacyConfigPath).Path
$config = Read-Json $LegacyConfigPath
if ($config._plugins.enabled) { throw 'Plugin mode is enabled: use -PluginRoot instead of the legacy adapter.' }
if ($config.windows -isnot [pscustomobject] -or $config.windows.terminal -isnot [array]) {
    throw 'Invalid legacy config: expected windows.terminal array.'
}
$root = Split-Path -Parent $LegacyConfigPath
$restorePath = Join-Path $root 'imgossh-legacy-restore.json'
# v2 metadata records both icon slots; v1 SSH-only installations are upgraded.
$groups = @(
    @{ field = 'terminal'; slot = 'xshell'; definition = 'terminal.json' },
    @{ field = 'filetransfer'; slot = 'winscp'; definition = 'filetransfer.json' }
)
$installed = Test-Path -LiteralPath $restorePath -PathType Leaf
$restore = if ($installed) { Read-Json $restorePath } else {
    [pscustomobject]@{ version = 2; config_path = $LegacyConfigPath; original = @() }
}
if ($restore.version -notin @(1, 2) -or $restore.config_path -ne $LegacyConfigPath) { throw 'Restore metadata does not match this configuration.' }
$originalVersion = $restore.version
if ($originalVersion -eq 1) {
    foreach ($item in @($restore.original)) { $item | Add-Member NoteProperty field 'terminal' -Force }
    $restore.version = 2
}
if (-not $Uninstall) {
    if (-not $ImGoSSHPath) { throw 'Specify -ImGoSSHPath with the new imgossh.exe.' }
    $ImGoSSHPath = (Resolve-Path -LiteralPath $ImGoSSHPath).Path
    if (-not (Test-Path -LiteralPath $ImGoSSHPath -PathType Leaf)) { throw 'ImGoSSHPath must be a file.' }
} elseif (-not $installed) { throw 'Legacy connector is not installed by this script.' }
foreach ($group in $groups) {
    $field = $group.field
    if ($config.windows.$field -isnot [array]) { throw "Invalid legacy config: expected windows.$field array." }
    $items = @($config.windows.$field)
    $owned = @($items | Where-Object { $_._imgossh_connector -eq 1 })
    $managed = $installed -and ($originalVersion -eq 2 -or $field -eq 'terminal')
    if ($owned.Count -gt 1 -or ($owned.Count -gt 0 -and -not $managed)) {
        throw "Unmanaged or duplicate ImGoSSH $field connector; restore the original configuration."
    }
    if ($managed -and (($owned.Count -eq 1 -and $owned[0].name -ne $group.slot) -or
        @($items | Where-Object { $_.name -eq $group.slot -and $_._imgossh_connector -ne 1 }).Count -gt 0 -or
        (-not $Uninstall -and $owned.Count -ne 1))) {
        throw "The $field connector slot was replaced externally; restore its configuration."
    }
    if (-not $Uninstall) {
        if (-not $managed) {
            for ($i = 0; $i -lt $items.Count; $i++) {
                if ($items[$i].name -eq $group.slot) {
                    $restore.original = @($restore.original) + @([pscustomobject]@{ field = $field; index = $i; entry = $items[$i] })
                }
            }
        }
        $entry = Read-Json (Join-Path $PSScriptRoot ('legacy\' + $group.definition))
        $entry.path = $ImGoSSHPath
        $config.windows.$field = @($entry) + @($items | Where-Object { $_.name -ne $group.slot })
    } elseif ($managed) {
        $remaining = [System.Collections.Generic.List[object]]::new()
        foreach ($item in $items) { if ($item._imgossh_connector -ne 1) { $remaining.Add($item) } }
        foreach ($original in @($restore.original | Where-Object field -eq $field | Sort-Object index)) {
            $remaining.Insert([Math]::Min([int]$original.index, $remaining.Count), $original.entry)
        }
        $config.windows.$field = @($remaining.ToArray())
    }
}
$action = if ($Uninstall) { 'Remove legacy ImGoSSH connector' } else { 'Install legacy ImGoSSH SSH/SFTP connectors' }
if (-not $PSCmdlet.ShouldProcess($LegacyConfigPath, $action)) { return }

$backupDir = Join-Path $root ('imgossh-backups\' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $backupDir -Force | Out-Null
Copy-Item -LiteralPath $LegacyConfigPath -Destination (Join-Path $backupDir 'config.json')
if ($installed) { Copy-Item -LiteralPath $restorePath -Destination (Join-Path $backupDir 'restore.json') }
try {
    if (-not $Uninstall) { Write-Json $restorePath $restore }
    Write-Json $LegacyConfigPath $config
    if ($Uninstall) { Remove-Item -LiteralPath $restorePath -Force }
} catch {
    Copy-Item -LiteralPath (Join-Path $backupDir 'config.json') -Destination $LegacyConfigPath -Force
    if ($installed) { Copy-Item -LiteralPath (Join-Path $backupDir 'restore.json') -Destination $restorePath -Force }
    elseif (Test-Path -LiteralPath $restorePath) { Remove-Item -LiteralPath $restorePath -Force }
    throw
}
Write-Output "$action complete. Restart JumpServer Client. Backup: $backupDir"
