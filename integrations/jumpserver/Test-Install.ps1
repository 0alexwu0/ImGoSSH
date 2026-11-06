#Requires -Version 5.1
# Offline fixture test; never reads or changes the installed JumpServer configuration.
$ErrorActionPreference = 'Stop'
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('imgossh-plugin-test-' + [guid]::NewGuid().ToString('N'))
$plugins = Join-Path $fixture 'client with spaces\plugins\windows'
$config = Join-Path $fixture 'config'
$exe = Join-Path $fixture 'ImGoSSH with spaces\imgossh.exe'
$utf8 = New-Object System.Text.UTF8Encoding($false)

function Assert($Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
function Read-Json([string]$Path) { Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json }

try {
    New-Item -ItemType Directory -Path $plugins, $config, (Split-Path -Parent $exe) -Force | Out-Null
    [IO.File]::WriteAllText($exe, 'fixture executable; never run', $utf8)
    $indexPath = Join-Path $plugins 'index.json'
    $statePath = Join-Path $config 'plugins-state.json'
    $originalIndex = '{"plugins":[{"id":"windows.putty","name":"putty","category":"terminal"}]}'
    $originalState = '{"version":1,"selections":{"terminal:ssh":"windows.putty","filetransfer:sftp":"windows.winscp"},"plugins":{"windows.putty":{"enabled":true,"path":"putty.exe"}},"unrelated":{"keep":true}}'
    [IO.File]::WriteAllText($indexPath, $originalIndex, $utf8)
    [IO.File]::WriteAllText($statePath, $originalState, $utf8)
    $installer = Join-Path $PSScriptRoot 'Install-JumpServerPlugin.ps1'
    & $installer -PluginRoot $plugins -ConfigRoot $config -ImGoSSHPath $exe -WhatIf
    Assert ((Get-Content -LiteralPath $indexPath -Raw) -eq $originalIndex) 'WhatIf changed index'
    Assert ((Get-Content -LiteralPath $statePath -Raw) -eq $originalState) 'WhatIf changed state'
    & $installer -PluginRoot $plugins -ConfigRoot $config -ImGoSSHPath $exe
    $state = Read-Json $statePath
    Assert ($state.selections.'terminal:ssh' -eq 'io.github.imgossh') 'SSH selection missing'
    Assert ($state.plugins.'io.github.imgossh'.path -eq $exe) 'Executable path with spaces changed'
    Assert ($state.unrelated.keep -eq $true) 'Unrelated configuration changed'
    Assert ($state.selections.'filetransfer:sftp' -eq 'io.github.imgossh.sftp') 'SFTP selection missing'
    Assert ($state.plugins.'io.github.imgossh.sftp'.path -eq $exe) 'SFTP executable missing'
    Assert (Test-Path -LiteralPath (Join-Path $plugins 'io.github.imgossh\icon.png')) 'Plugin icon missing'
    & $installer -PluginRoot $plugins -ConfigRoot $config -ImGoSSHPath $exe
    $index = Read-Json $indexPath
    Assert (@($index.plugins | Where-Object { $_.id -eq 'io.github.imgossh' }).Count -eq 1) 'Repeated install duplicated index'
    & $installer -PluginRoot $plugins -ConfigRoot $config -Uninstall
    $state = Read-Json $statePath
    Assert ($state.selections.'terminal:ssh' -eq 'windows.putty') 'Uninstall did not restore previous SSH selection'
    Assert ($state.selections.'filetransfer:sftp' -eq 'windows.winscp') 'Uninstall did not restore previous SFTP selection'
    Assert ($null -eq $state.plugins.PSObject.Properties['io.github.imgossh']) 'Plugin state left behind'
    Assert (@((Read-Json $indexPath).plugins).Count -eq 1) 'Uninstall did not restore index'
    Assert (-not (Test-Path -LiteralPath (Join-Path $plugins 'io.github.imgossh'))) 'Plugin directory left behind'
    Assert ($state.unrelated.keep -eq $true) 'Uninstall changed unrelated configuration'
    # A user selection made after installation must survive removal.
    & $installer -PluginRoot $plugins -ConfigRoot $config -ImGoSSHPath $exe
    $state = Read-Json $statePath
    $state.selections.'terminal:ssh' = 'windows.xshell'
    $state.selections.'filetransfer:sftp' = 'windows.xftp'
    [IO.File]::WriteAllText($statePath, ($state | ConvertTo-Json -Depth 50), $utf8)
    & $installer -PluginRoot $plugins -ConfigRoot $config -Uninstall
    Assert ((Read-Json $statePath).selections.'terminal:ssh' -eq 'windows.xshell') 'Uninstall overwrote a later user selection'
    Assert ((Read-Json $statePath).selections.'filetransfer:sftp' -eq 'windows.xftp') 'Uninstall overwrote a later SFTP selection'
    # Use the actual v2.1.4 config, including its compiled-in XShell icon name.
    $legacyPath = Join-Path $fixture 'client 2.1.4\resources\bin\config.json'
    New-Item -ItemType Directory -Path (Split-Path -Parent $legacyPath) -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'fixtures\client-2.1.4-config.json') -Destination $legacyPath
    $originalLegacy = Read-Json $legacyPath
    $originalFiletransfer = $originalLegacy.windows.filetransfer | ConvertTo-Json -Depth 50 -Compress
    $originalTerminal = $originalLegacy.windows.terminal | ConvertTo-Json -Depth 50 -Compress
    $unrelated = @($originalLegacy.windows.databases,
        $originalLegacy.windows.remotedesktop, $originalLegacy.macos, $originalLegacy.linux) | ConvertTo-Json -Depth 50 -Compress
    $legacyText = Get-Content -LiteralPath $legacyPath -Raw
    & $installer -LegacyConfigPath $legacyPath -ImGoSSHPath $exe -WhatIf
    Assert ((Get-Content -LiteralPath $legacyPath -Raw) -eq $legacyText) 'Legacy WhatIf changed config'
    Assert (-not (Test-Path -LiteralPath (Join-Path (Split-Path -Parent $legacyPath) 'imgossh-legacy-restore.json'))) 'Legacy WhatIf created metadata'
    & $installer -LegacyConfigPath $legacyPath -ImGoSSHPath $exe
    $legacy = Read-Json $legacyPath
    # Reproduce v2.1.4 handleSSH: first is_default && is_set && supports ssh.
    $chosen = @($legacy.windows.terminal | Where-Object { $_.is_default -and $_.is_set -and $_.protocol -contains 'ssh' })[0]
    Assert ($chosen.display_name -eq 'ImGoSSH' -and $chosen.path -eq $exe) '2.1.4 would not select ImGoSSH'
    Assert ($chosen.name -eq 'xshell' -and -not $chosen.is_internal) 'Legacy icon or executable mode incompatible'
    Assert ($chosen.arg_format -eq (Read-Json (Join-Path $PSScriptRoot 'plugin\connect.json')).launch.template) 'Legacy template differs from current connector'
    Assert (@($legacy.windows.terminal | Where-Object { $_.name -eq 'xshell' }).Count -eq 1) 'XShell slot duplicated'
    $chosenSftp = @($legacy.windows.filetransfer | Where-Object { $_.is_default -and $_.is_set -and $_.protocol -contains 'sftp' })[0]
    Assert ($chosenSftp.display_name -eq 'ImGoSSH SFTP' -and $chosenSftp.path -eq $exe) '2.1.4 SFTP selection failed'
    Assert ($chosenSftp.name -eq 'winscp' -and -not $chosenSftp.is_internal) 'SFTP icon slot incompatible'
    Assert ($chosenSftp.arg_format -eq (Read-Json (Join-Path $PSScriptRoot 'plugin-sftp\connect.json')).launch.template) 'SFTP template differs'
    $currentUnrelated = @($legacy.windows.databases,
        $legacy.windows.remotedesktop, $legacy.macos, $legacy.linux) | ConvertTo-Json -Depth 50 -Compress
    Assert ($currentUnrelated -eq $unrelated) 'Legacy installation changed other protocols or platforms'
    & $installer -LegacyConfigPath $legacyPath -ImGoSSHPath $exe
    Assert (@((Read-Json $legacyPath).windows.terminal | Where-Object { $_._imgossh_connector -eq 1 }).Count -eq 1) 'Legacy repeat install duplicated connector'
    & $installer -LegacyConfigPath $legacyPath -Uninstall
    Assert (((Read-Json $legacyPath).windows.terminal | ConvertTo-Json -Depth 50 -Compress) -eq $originalTerminal) 'Legacy uninstall did not restore original entries and ordering'
    Assert (((Read-Json $legacyPath).windows.filetransfer | ConvertTo-Json -Depth 50 -Compress) -eq $originalFiletransfer) 'Legacy uninstall did not restore SFTP entries'
    # Upgrade a previous SSH-only installation with v1 restore metadata.
    & $installer -LegacyConfigPath $legacyPath -ImGoSSHPath $exe
    $legacy = Read-Json $legacyPath
    $legacy.windows.filetransfer = $originalLegacy.windows.filetransfer
    [IO.File]::WriteAllText($legacyPath, ($legacy | ConvertTo-Json -Depth 50), $utf8)
    $restorePath = Join-Path (Split-Path -Parent $legacyPath) 'imgossh-legacy-restore.json'
    $restore = Read-Json $restorePath
    $restore.version = 1
    $restore.original = @($restore.original | Where-Object field -eq 'terminal' | ForEach-Object { $_.PSObject.Properties.Remove('field'); $_ })
    [IO.File]::WriteAllText($restorePath, ($restore | ConvertTo-Json -Depth 50), $utf8)
    & $installer -LegacyConfigPath $legacyPath -ImGoSSHPath $exe
    Assert ((Read-Json $legacyPath).windows.filetransfer[0]._imgossh_connector -eq 1) 'SSH-only upgrade did not install SFTP'
    & $installer -LegacyConfigPath $legacyPath -Uninstall
    Assert (((Read-Json $legacyPath).windows.filetransfer | ConvertTo-Json -Depth 50 -Compress) -eq $originalFiletransfer) 'Upgrade lost original SFTP entry'
    # A later user choice and unrelated path edit must survive uninstall.
    & $installer -LegacyConfigPath $legacyPath -ImGoSSHPath $exe
    $legacy = Read-Json $legacyPath
    foreach ($item in $legacy.windows.terminal) { $item.is_default = $item.name -eq 'putty' }
    ($legacy.windows.terminal | Where-Object { $_.name -eq 'putty' }).path = 'updated-putty.exe'
    [IO.File]::WriteAllText($legacyPath, ($legacy | ConvertTo-Json -Depth 50), $utf8)
    & $installer -LegacyConfigPath $legacyPath -Uninstall
    $putty = (Read-Json $legacyPath).windows.terminal | Where-Object { $_.name -eq 'putty' }
    Assert ($putty.is_default -and $putty.path -eq 'updated-putty.exe') 'Legacy uninstall discarded later user changes'
    # v3 uses the flat config in the user's directory and match_first selection.
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'fixtures\client-2.1.4-config.json') -Destination (Join-Path $config 'config.json')
    & $installer -LegacyConfigPath (Join-Path $config 'config.json') -ImGoSSHPath $exe
    $flatConfig = Read-Json (Join-Path $config 'config.json')
    $chosen = @($flatConfig.windows.terminal | Where-Object { $_.is_set -and $_.match_first -contains 'ssh' })[0]
    Assert ($chosen._imgossh_connector -eq 1) 'v3 match_first selection failed'
    $chosenSftp = @($flatConfig.windows.filetransfer | Where-Object { $_.is_set -and $_.match_first -contains 'sftp' })[0]
    Assert ($chosenSftp._imgossh_connector -eq 1) 'v3 SFTP match_first selection failed'
    & $installer -LegacyConfigPath (Join-Path $config 'config.json') -Uninstall
    # Never apply the legacy adapter to an active indexed-plugin configuration.
    $flatConfig | Add-Member -MemberType NoteProperty -Name '_plugins' -Value ([pscustomobject]@{ enabled = $true }) -Force
    [IO.File]::WriteAllText((Join-Path $config 'config.json'), ($flatConfig | ConvertTo-Json -Depth 50), $utf8)
    $rejected = $false
    try { & $installer -LegacyConfigPath (Join-Path $config 'config.json') -ImGoSSHPath $exe }
    catch { $rejected = $true }
    Assert $rejected 'Legacy adapter accepted active plugin mode'
    Assert (-not (Test-Path -LiteralPath (Join-Path $config 'imgossh-legacy-restore.json'))) 'Rejected installation wrote restore metadata'
    Write-Output 'Installer tests passed (2.1.4 defaults, v3 selection, indexed plugins, WhatIf, repeat install, backups, uninstall, paths with spaces, unrelated settings).'
} finally {
    $resolvedFixture = [IO.Path]::GetFullPath($fixture)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if (-not $resolvedFixture.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetFileName($resolvedFixture) -notlike 'imgossh-plugin-test-*') {
        throw 'Refusing to remove a test directory outside the temporary workspace.'
    }
    if (Test-Path -LiteralPath $resolvedFixture) { Remove-Item -LiteralPath $resolvedFixture -Recurse -Force }
}
