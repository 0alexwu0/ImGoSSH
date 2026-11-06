#Requires -Version 5.1
# Isolated fixture with a mocked desktop process. Never stops installed clients.
$ErrorActionPreference = 'Stop'
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('imgossh-auto-test-' + [guid]::NewGuid().ToString('N'))
$utf8 = New-Object System.Text.UTF8Encoding($false)
function Assert($Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
try {
    New-Item -ItemType Directory -Path $fixture | Out-Null
    $config = Join-Path $fixture 'client with spaces\resources\bin\config.json'
    New-Item -ItemType Directory -Path (Split-Path -Parent $config) -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'fixtures\client-2.1.4-config.json') -Destination $config
    $exe = Join-Path $fixture 'ImGoSSH.exe'
    [IO.File]::WriteAllText($exe, 'fixture', $utf8)
    $wrapper = Join-Path $fixture 'run.ps1'
    $wrapperSource = @'
param($Integration, $Config, $Exe, $Fixture, $ForceClose)
$ErrorActionPreference = 'Stop'
$env:IMGOSSH_JUMPSERVER_CONFIG = $Config
$global:fixtureConfig = $Config
$global:fixturePath = $Fixture
$global:forceClose = $ForceClose -eq 'force'
function Get-Process {
    $client = [pscustomobject]@{ Id = 111; HasExited = $false; ProcessName = 'JumpServerClient'; Path = (Join-Path $Fixture 'client with spaces\JumpServerClient.exe') }
    $client | Add-Member ScriptMethod CloseMainWindow {
        # Emulate the client flushing its UI settings when it exits.
        $value = Get-Content -LiteralPath $global:fixtureConfig -Raw -Encoding UTF8 | ConvertFrom-Json
        $value | Add-Member NoteProperty flushed_on_exit $true -Force
        [IO.File]::WriteAllText($global:fixtureConfig, ($value | ConvertTo-Json -Depth 50), (New-Object System.Text.UTF8Encoding($false)))
        [IO.File]::WriteAllText((Join-Path $global:fixturePath 'closed'), 'closed')
        return $true
    }
    $client | Add-Member ScriptMethod WaitForExit { param($Timeout) return -not $global:forceClose -or (Test-Path -LiteralPath (Join-Path $global:fixturePath 'killed')) }
    $client | Add-Member ScriptMethod Kill { [IO.File]::WriteAllText((Join-Path $global:fixturePath 'killed'), 'killed') }
    $client
    # A live Go launcher must not be closed; unlike a desktop UI, it owns SSH sessions.
    $launcher = [pscustomobject]@{ ProcessName = 'JumpServerClient'; Path = (Join-Path $Fixture 'client with spaces\resources\bin\windows\JumpServerClient.exe') }
    $launcher | Add-Member ScriptMethod CloseMainWindow { throw 'Active Go launcher was closed' }
    $launcher
}
& (Join-Path $Integration 'Auto-Configure.ps1') -ImGoSSHPath $Exe
'@
    [IO.File]::WriteAllText($wrapper, $wrapperSource, $utf8)
    function Run-Auto([string]$Mode = '') {
        $result = & powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $wrapper $PSScriptRoot $config $exe $fixture $Mode
        $result | ConvertFrom-Json
    }
    $result = Run-Auto
    Assert ($result.status -eq 'installed') "Auto installation failed: $($result.detail)"
    Assert (Test-Path -LiteralPath (Join-Path $fixture 'closed')) 'Client was not closed before installation'
    $value = Get-Content -LiteralPath $config -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert $value.flushed_on_exit 'Client changes made during exit were lost'
    Assert ($value.windows.terminal[0].path -eq $exe) 'Wrong executable registered'
    $backups = @(Get-ChildItem -LiteralPath (Join-Path (Split-Path -Parent $config) 'imgossh-backups') -Directory)
    Assert ($backups.Count -eq 1) 'Initial configuration backup missing'
    $backup = Get-Content -LiteralPath (Join-Path $backups[0].FullName 'config.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert $backup.flushed_on_exit 'Backup predates client exit'
    Remove-Item -LiteralPath (Join-Path $fixture 'closed')
    $before = [IO.File]::ReadAllBytes($config)
    $result = Run-Auto
    Assert ($result.status -eq 'present') 'Existing configuration was not detected'
    Assert (-not (Test-Path -LiteralPath (Join-Path $fixture 'closed'))) 'Configured client was closed again'
    Assert ([Convert]::ToBase64String($before) -eq [Convert]::ToBase64String([IO.File]::ReadAllBytes($config))) 'Repeated startup changed config'
    Assert (@(Get-ChildItem -LiteralPath (Join-Path (Split-Path -Parent $config) 'imgossh-backups') -Directory).Count -eq 1) 'Repeated startup created unnecessary backup'
    # An earlier SSH-only install must be detected and upgraded automatically.
    $value = Get-Content -LiteralPath $config -Raw -Encoding UTF8 | ConvertFrom-Json
    $original = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'fixtures\client-2.1.4-config.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    $value.windows.filetransfer = $original.windows.filetransfer
    [IO.File]::WriteAllText($config, ($value | ConvertTo-Json -Depth 50), $utf8)
    $restorePath = Join-Path (Split-Path -Parent $config) 'imgossh-legacy-restore.json'
    $restore = Get-Content -LiteralPath $restorePath -Raw -Encoding UTF8 | ConvertFrom-Json
    $restore.version = 1
    $restore.original = @($restore.original | Where-Object field -eq 'terminal' | ForEach-Object { $_.PSObject.Properties.Remove('field'); $_ })
    [IO.File]::WriteAllText($restorePath, ($restore | ConvertTo-Json -Depth 50), $utf8)
    $result = Run-Auto
    Assert ($result.status -eq 'installed') "SSH-only upgrade failed: $($result.detail)"
    $value = Get-Content -LiteralPath $config -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert ($value.windows.filetransfer[0].path -eq $exe) 'SFTP was not registered'
    Remove-Item -LiteralPath (Join-Path $fixture 'closed')
    Assert ((Run-Auto).status -eq 'present') 'Upgraded configuration not detected'
    # A malformed config must neither be overwritten nor cause the client to close.
    [IO.File]::WriteAllText($config, '{invalid-json', $utf8)
    $result = Run-Auto
    Assert ($result.status -eq 'error') 'Invalid config was accepted'
    Assert (-not (Test-Path -LiteralPath (Join-Path $fixture 'closed'))) 'Client closed for an invalid config'
    Assert ([IO.File]::ReadAllText($config) -eq '{invalid-json') 'Invalid config was overwritten'
    # A read/write-locked file must fail before closing the client.
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'fixtures\client-2.1.4-config.json') -Destination $config -Force
    Remove-Item -LiteralPath (Join-Path (Split-Path -Parent $config) 'imgossh-legacy-restore.json')
    $handle = [IO.File]::Open($config, 'Open', 'Read', 'Read')
    try { $result = Run-Auto } finally { $handle.Dispose() }
    Assert ($result.status -eq 'error') 'Locked configuration accepted'
    Assert (-not (Test-Path -LiteralPath (Join-Path $fixture 'closed'))) 'Client closed before detecting write failure'
    $result = Run-Auto 'force'
    Assert ($result.status -eq 'installed') "Forced close failed: $($result.detail)"
    Assert (Test-Path -LiteralPath (Join-Path $fixture 'killed')) 'Unresponsive client was not stopped'
    # Indexed clients require independent SSH and SFTP entries and templates.
    $pluginRoot = Join-Path $fixture 'client with spaces\resources\plugins\windows'
    New-Item -ItemType Directory -Path $pluginRoot -Force | Out-Null
    [IO.File]::WriteAllText((Join-Path $pluginRoot 'index.json'), '{"plugins":[]}', $utf8)
    [IO.File]::WriteAllText($config, '{"_plugins":{"enabled":true}}', $utf8)
    Remove-Item -LiteralPath (Join-Path $fixture 'closed')
    $result = Run-Auto
    Assert ($result.status -eq 'installed') "Indexed auto configuration failed: $($result.detail)"
    $configRoot = Split-Path -Parent $config
    $state = Get-Content -LiteralPath (Join-Path $configRoot 'plugins-state.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert ($state.selections.'filetransfer:sftp' -eq 'io.github.imgossh.sftp') 'Indexed SFTP selection missing'
    Remove-Item -LiteralPath (Join-Path $fixture 'closed')
    Assert ((Run-Auto).status -eq 'present') 'Indexed SSH/SFTP configuration not detected'
    Assert (-not (Test-Path -LiteralPath (Join-Path $fixture 'closed'))) 'Configured indexed client closed again'
    Remove-Item -LiteralPath (Join-Path $pluginRoot 'io.github.imgossh.sftp\connect.json')
    $result = Run-Auto
    Assert ($result.status -eq 'installed') "Missing indexed SFTP template not repaired: $($result.detail)"
    Write-Output 'Automatic configuration tests passed (close before reread, latest backup, no-op when present, invalid/locked config, forced close, preserve Go launcher, SSH-only upgrade, indexed SSH/SFTP repair).'
} finally {
    $resolved = [IO.Path]::GetFullPath($fixture)
    $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if (-not $resolved.StartsWith($temp, [StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetFileName($resolved) -notlike 'imgossh-auto-test-*') { throw 'Invalid temporary test directory' }
    if (Test-Path -LiteralPath $resolved) { Remove-Item -LiteralPath $resolved -Recurse -Force }
}
