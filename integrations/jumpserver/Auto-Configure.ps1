#Requires -Version 5.1
# Invoked by ImGoSSH with an embedded copy of the connector installers.
param([Parameter(Mandatory = $true)][string]$ImGoSSHPath)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
function Read-Json([string]$Path) {
    try { Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json }
    catch { throw "Cannot read valid JSON from $Path. The file was not changed." }
}
function Same-Path([string]$Left, [string]$Right) {
    if (-not $Left -or -not $Right) { return $false }
    [IO.Path]::GetFullPath($Left).TrimEnd('\').Equals([IO.Path]::GetFullPath($Right).TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)
}
function Emit([string]$Status, [string]$Path = '', [string]$Detail = '') {
    @{ status = $Status; path = $Path; detail = $Detail } | ConvertTo-Json -Compress
}
$mutex = New-Object System.Threading.Mutex($false, 'Local\ImGoSSH-JumpServer-Configure')
$locked = $false
try {
    try { $locked = $mutex.WaitOne(15000) } catch [System.Threading.AbandonedMutexException] { $locked = $true }
    if (-not $locked) { throw 'Another ImGoSSH instance is configuring JumpServer; try again after it finishes.' }
    $ImGoSSHPath = (Resolve-Path -LiteralPath $ImGoSSHPath).Path
    $roots = @()
    $handlerRoot = $null
    $handler = (Get-ItemProperty -LiteralPath 'Registry::HKEY_CLASSES_ROOT\jms\shell\open\command' -ErrorAction SilentlyContinue).'(default)'
    if ($handler -match '^"([^"]+\\resources\\bin\\windows\\JumpServerClient(?:32)?\.exe)"') {
        $handlerRoot = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $Matches[1])))
        $roots += $handlerRoot
    }
    foreach ($key in @('HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*')) {
        Get-ItemProperty -Path $key -ErrorAction SilentlyContinue |
            Where-Object { $_.DisplayName -like '*JumpServer*' -and $_.InstallLocation } |
            ForEach-Object { $roots += $_.InstallLocation }
    }
    foreach ($base in @($env:ProgramFiles, ${env:ProgramFiles(x86)}, (Join-Path $env:LOCALAPPDATA 'Programs'), $env:LOCALAPPDATA)) {
        if ($base) { foreach ($name in @('JumpServer', 'JumpServerClient', 'jumpserver-client')) { $roots += Join-Path $base $name } }
    }
    # Only desktop UI processes count as clients. Go launchers under resources/bin
    # own active SSH sessions and must never be stopped by this integration.
    $clients = @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
        $_.ProcessName -in @('JumpServer', 'JumpServerClient', 'jumpserver-client') -and
        $_.Path -and $_.Path -notmatch '[\\/]resources[\\/]bin[\\/]'
    })
    foreach ($client in $clients) { $roots += Split-Path -Parent $client.Path }
    $candidates = @(
        foreach ($root in ($roots | Select-Object -Unique)) {
            $path = Join-Path $root 'resources\bin\config.json'
            if (Test-Path -LiteralPath $path -PathType Leaf) { (Resolve-Path -LiteralPath $path).Path }
        }
        $userPath = Join-Path $env:APPDATA 'jumpserver-client\config.json'
        if (Test-Path -LiteralPath $userPath -PathType Leaf) { (Resolve-Path -LiteralPath $userPath).Path }
    ) | Select-Object -Unique
    # Explicit override for portable/custom installations; never invent a config.
    if ($env:IMGOSSH_JUMPSERVER_CONFIG) {
        $candidates = @((Resolve-Path -LiteralPath $env:IMGOSSH_JUMPSERVER_CONFIG).Path)
    } elseif ($handlerRoot) {
        $activeConfig = Join-Path $handlerRoot 'resources\bin\config.json'
        if (Test-Path -LiteralPath $activeConfig -PathType Leaf) { $candidates = @((Resolve-Path -LiteralPath $activeConfig).Path) }
    }
    if (@($candidates).Count -eq 0) { Emit 'not_found'; exit 0 }
    if (@($candidates).Count -ne 1) { throw 'Multiple JumpServer configurations found. Set IMGOSSH_JUMPSERVER_CONFIG to the active config.json.' }
    $path = @($candidates)[0]
    $config = Read-Json $path
    $template = (Read-Json (Join-Path $PSScriptRoot 'legacy\terminal.json')).arg_format
    $sftpTemplate = (Read-Json (Join-Path $PSScriptRoot 'legacy\filetransfer.json')).arg_format
    if ($config._plugins.enabled) {
        # Retain indexed-plugin support through the existing installer.
        $pluginRoots = @(
            foreach ($root in ($roots | Select-Object -Unique)) {
                foreach ($relative in @('resources\plugins\windows', 'plugins\windows', 'windows')) {
                    $candidate = Join-Path $root $relative
                    if (Test-Path -LiteralPath (Join-Path $candidate 'index.json') -PathType Leaf) { (Resolve-Path -LiteralPath $candidate).Path }
                }
            }
        ) | Select-Object -Unique
        if (@($pluginRoots).Count -ne 1) { throw 'Cannot identify one indexed plugin directory; use Install-JumpServerPlugin.ps1 -PluginRoot.' }
        $pluginRoot = @($pluginRoots)[0]
        $statePath = Join-Path (Split-Path -Parent $path) 'plugins-state.json'
        $index = Read-Json (Join-Path $pluginRoot 'index.json')
        $present = Test-Path -LiteralPath $statePath -PathType Leaf
        if ($present) {
            $state = Read-Json $statePath
            foreach ($definition in @(@{ id = 'io.github.imgossh'; template = $template }, @{ id = 'io.github.imgossh.sftp'; template = $sftpTemplate })) {
                $id = $definition.id
                $entryPath = Join-Path $pluginRoot ($id + '\connect.json')
                if (-not (Test-Path -LiteralPath $entryPath -PathType Leaf)) { $present = $false; continue }
                $entry = Read-Json $entryPath
                if (-not (Same-Path $state.plugins.$id.path $ImGoSSHPath) -or
                    $entry.launch.template -ne $definition.template -or @($index.plugins | Where-Object { $_.id -eq $id }).Count -ne 1) {
                    $present = $false
                }
            }
        }
        if ($present) { Emit 'present' $path; exit 0 }
        $install = @{ PluginRoot = $pluginRoot; ConfigRoot = (Split-Path -Parent $path); ImGoSSHPath = $ImGoSSHPath }
    } else {
        if ($config.windows.terminal -isnot [array]) { throw 'Invalid JumpServer config: windows.terminal must be an array.' }
        $existing = @($config.windows.terminal | Where-Object {
            $_.arg_format -eq $template -and $_.protocol -contains 'ssh' -and
            (Same-Path $_.path $ImGoSSHPath) -and -not $_.is_internal
        })
        if ($config.windows.filetransfer -isnot [array]) { throw 'Invalid JumpServer config: windows.filetransfer must be an array.' }
        $existingSftp = @($config.windows.filetransfer | Where-Object {
            $_.arg_format -eq $sftpTemplate -and $_.protocol -contains 'sftp' -and
            (Same-Path $_.path $ImGoSSHPath) -and -not $_.is_internal
        })
        if ($existing.Count -gt 0 -and $existingSftp.Count -gt 0) { Emit 'present' $path; exit 0 }
        $restorePath = Join-Path (Split-Path -Parent $path) 'imgossh-legacy-restore.json'
        $owned = @($config.windows.terminal | Where-Object { $_._imgossh_connector -eq 1 })
        if (Test-Path -LiteralPath $restorePath -PathType Leaf) {
            $restore = Read-Json $restorePath
            if ($restore.version -notin @(1, 2) -or -not (Same-Path $restore.config_path $path) -or $owned.Count -ne 1 -or
                @($config.windows.terminal | Where-Object { $_.name -eq 'xshell' -and $_._imgossh_connector -ne 1 }).Count -gt 0) {
                throw 'Existing ImGoSSH restore metadata conflicts with this configuration; restore it before trying again.'
            }
            if ($restore.version -eq 2 -and (@($config.windows.filetransfer | Where-Object { $_._imgossh_connector -eq 1 }).Count -ne 1 -or
                @($config.windows.filetransfer | Where-Object { $_.name -eq 'winscp' -and $_._imgossh_connector -ne 1 }).Count -gt 0)) {
                throw 'Existing SFTP restore metadata conflicts with this configuration; restore it before trying again.'
            }
        } elseif ($owned.Count -gt 0) { throw 'ImGoSSH restore metadata is missing; restore the original configuration before trying again.' }
        $install = @{ LegacyConfigPath = $path; ImGoSSHPath = $ImGoSSHPath }
    }

    # Check write access before closing a user's client. Never self-elevate.
    $probe = [IO.File]::Open($path, 'Open', 'ReadWrite', 'ReadWrite')
    $probe.Dispose()
    $targetRoot = if ($install.PluginRoot) { $install.PluginRoot } else {
        # resources/bin/config.json -> installation root; v3 config -> no install root.
        $parent = Split-Path -Parent $path
        if ($parent -match '[\\/]resources[\\/]bin$') { Split-Path -Parent (Split-Path -Parent $parent) }
    }
    $targets = @($clients | Where-Object {
        -not $targetRoot -or (Same-Path (Split-Path -Parent $_.Path) $targetRoot) -or
        ($install.PluginRoot -and $install.PluginRoot.StartsWith((Split-Path -Parent $_.Path) + '\', [StringComparison]::OrdinalIgnoreCase))
    })
    if (-not $targetRoot -and @($targets | ForEach-Object { Split-Path -Parent $_.Path } | Select-Object -Unique).Count -gt 1) {
        throw 'Multiple running JumpServer installations found; cannot identify which client owns the user configuration.'
    }
    foreach ($client in $targets) { if (-not $client.HasExited) { $null = $client.CloseMainWindow() } }
    foreach ($client in $targets) {
        if (-not $client.HasExited -and -not $client.WaitForExit(4000)) {
            # The Process object/handle identifies the verified UI process, not a name wildcard.
            $client.Kill()
            if (-not $client.WaitForExit(4000)) { throw 'JumpServer Client did not exit; configuration was not changed.' }
        }
    }
    # UI settings must not be flushed by a replacement desktop process mid-write.
    $replacement = @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
        $_.ProcessName -in @('JumpServer', 'JumpServerClient', 'jumpserver-client') -and
        $_.Path -and $_.Path -notmatch '[\\/]resources[\\/]bin[\\/]' -and
        $_.Id -notin @($targets | ForEach-Object { $_.Id }) -and
        (-not $targetRoot -or (Same-Path (Split-Path -Parent $_.Path) $targetRoot) -or
            ($install.PluginRoot -and $install.PluginRoot.StartsWith((Split-Path -Parent $_.Path) + '\', [StringComparison]::OrdinalIgnoreCase)))
    })
    if ($replacement.Count -gt 0) { throw 'JumpServer Client restarted while configuring. Configuration was not changed; try again.' }
    # Installer re-reads after client exit and backs up that latest on-disk version.
    $output = & (Join-Path $PSScriptRoot 'Install-JumpServerPlugin.ps1') @install
    Emit 'installed' $path ($output -join "`n")
} catch {
    Emit 'error' '' $_.Exception.Message
    exit 1
} finally {
    if ($locked) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}
