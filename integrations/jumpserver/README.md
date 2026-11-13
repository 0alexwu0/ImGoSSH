# JumpServer 集成

最低支持 **JumpServer Client 2.1.4 / Windows**，并保留 v4.1.9 的新版插件接入方式。
这里的版本指本机客户端；JumpServer 服务端仍需允许 SSH/SFTP 客户端连接，并为用户授权目标资产。
浏览器 `jms://` 继续交给已安装的官方客户端处理。

连接路径：JumpServer 网页 → 官方客户端 → ImGoSSH → KoKo SSH 网关 → 目标资产。
两代客户端都传入网关 endpoint、`JMS-<token.id>` 用户名和 `token.value` 临时凭据，
权限和审计由 JumpServer 按部署配置执行。

## 打开 ImGoSSH 自动配置

Windows 下正常打开新版 ImGoSSH 就会在后台检查客户端配置，无需手动运行安装脚本：

1. 从 `jms://` 注册项、安装注册表、运行中的桌面客户端、常见安装位置和用户配置目录定位文件。
2. 检查是否有指向当前 ImGoSSH 程序、分别包含 SSH 和 SFTP 参数模板的连接配置。
3. 已存在时不关闭客户端、不改写默认项、不重复备份。
4. 缺少时关闭对应 JumpServer **桌面客户端**，先请求正常退出；4 秒未退出时结束该桌面进程。
   随后重新读取磁盘上的最新配置，先备份再写入，保留客户端退出时保存的设置。
5. 在 ImGoSSH 中提示结果。重新打开 JumpServer 即可连接。

检查不会结束 `resources/bin` 下承载 SSH 会话的 Go 启动器。
由 JumpServer 通过 `--jumpserver` 唤起的 ImGoSSH 跳过自动配置，避免干扰当前连接。
没有找到客户端时不创建配置文件、不显示提示；格式错误、路径歧义或权限不足会显示失败原因，
不会自动提权。Program Files 下无写权限时可用管理员身份重新打开 ImGoSSH。

便携或自定义安装无法定位时，可在启动前设置实际配置文件路径：

```powershell
$env:IMGOSSH_JUMPSERVER_CONFIG = 'D:\JumpServerClient\resources\bin\config.json'
.\imgossh.exe
```

新版索引插件模式下仍需能唯一定位安装目录中的插件索引。
自动配置所需脚本和插件资源已嵌入可执行文件，单独复制 `imgossh.exe` 也可使用。
下文手动安装与卸载脚本保留用于明确指定路径、恢复配置和排查问题。

已安装此前 SSH 连接器的用户，正常打开新版 ImGoSSH 会自动补齐 SFTP 配置，无需先卸载。

## 兼容方式

| 客户端配置 | 安装方式 | 核对依据 |
| --- | --- | --- |
| 2.1.4：安装目录 `resources/bin/config.json` | 修改 `windows.terminal` 和 `windows.filetransfer` 中的工具配置 | 官方 v2.1.4 配置、加载器、Windows 启动器及界面代码 |
| 3.x：用户目录 `jumpserver-client/config.json` | 同一旧版工具配置适配器，使用 `match_first` 选择 | 官方 v3.0.7 加载器和启动器 |
| v4.1.9 索引插件 | 更新平台 `index.json` 与用户 `plugins-state.json` | 官方 v4.1.9 Tauri 与 Go 插件加载器 |

兼容判断依据实际文件格式，不是仅修改插件的最低版本声明。新版插件的
`min_client_version` 仍为 4.1.9，因为 2.1.4 不识别该插件格式。
其他中间版本使用相同格式时可采用对应适配器，但未逐版本进行真实环境联调。

## 安装：JumpServer Client 2.1.4（Windows）

1. 使用 v0.2.2 或更新发布包，或构建：`cargo build --release --locked`。
   原始 ImGoSSH 0.2.1 发布包尚不包含新增连接参数。
2. 完全退出 JumpServer 客户端及正在唤起的连接进程，避免它覆盖配置。
3. 在 PowerShell 中执行，将安装目录和程序路径替换为实际路径：

```powershell
.\integrations\jumpserver\Install-JumpServerPlugin.ps1 `
  -LegacyConfigPath 'C:\Program Files\JumpServerClient\resources\bin\config.json' `
  -ImGoSSHPath 'C:\Tools\ImGoSSH\imgossh.exe'
```

`-LegacyConfigPath` 必须指向 **官方客户端实际读取的 config.json 文件**。
2.1.4 的 Go 启动器从自身所在目录的上一级读取配置；默认 Windows 安装中即
`resources\bin\config.json`，不是 `%APPDATA%`。3.x 通常改为
`%APPDATA%\jumpserver-client\config.json`，可显式传入该文件。
写入 Program Files 可能需要管理员 PowerShell。

可先加 `-WhatIf` 查看目标而不修改文件。省略 `-LegacyConfigPath` 和 `-PluginRoot` 时，
安装器尝试从卸载注册表、`jms://` 注册项、常见安装位置及用户配置定位；
发现多个候选会要求显式指定，避免修改错误的客户端配置。

4. 重启客户端，SSH 设置页会出现 **ImGoSSH**。2.1.4 使用编译时绑定的图标，
   因此安装器复用 XShell 的 `name=xshell` 槽位，显示名称改为 ImGoSSH；SFTP 复用 WinSCP 的 `name=winscp` 槽位，显示名称为 ImGoSSH SFTP。
   **原 XShell 和 WinSCP 条目在安装期间由 ImGoSSH 替代**，卸载会恢复。
5. 在 JumpServer 网页选择已授权资产，以 **SSH 客户端** 方式连接终端，或以 **SFTP 客户端** 方式打开文件浏览器并上传、下载。
   第一次连接需要核对并确认 **KoKo 网关** 的主机密钥。

2.1.4 根据 `is_default && is_set` 和列表顺序选择工具；适配器将 ImGoSSH 放在首位，
同时保留其余工具的配置。3.x 的 `match_first` 选择规则也由同一条目支持。
旧版界面手动启用另一工具后可能清除默认标记，导致 ImGoSSH 不再被调用；可重新运行安装器。
此集成支持 SSH 和 SFTP，2.1.4 上游把 Telnet 请求映射为 SSH 的行为仍存在，不提供原生 Telnet 支持。

## 安装：新版索引插件（v4.1.9）

先退出官方客户端，再执行：

```powershell
.\integrations\jumpserver\Install-JumpServerPlugin.ps1 `
  -PluginRoot 'C:\Program Files\JumpServer\resources\plugins\windows' `
  -ImGoSSHPath 'C:\Tools\ImGoSSH\imgossh.exe'
```

`-PluginRoot` 是**包含 `index.json` 的 Windows 平台插件目录**。
安装器添加 `io.github.imgossh`（SSH）和 `io.github.imgossh.sftp`（SFTP）插件及索引项，在用户目录 `plugins-state.json` 配置程序路径，
并选为 `terminal:ssh` 与 `filetransfer:sftp` 默认工具。`-ConfigRoot` 默认为当前用户的
`%APPDATA%\jumpserver-client`；使用另一个管理员账户执行时应显式指定实际使用者的目录。
重启官方客户端后，在「设置 → 应用 → SSH」确认 ImGoSSH 的路径和选择状态，并在 SFTP 页面确认 ImGoSSH SFTP。

## 卸载、备份与升级

旧版卸载：

```powershell
.\integrations\jumpserver\Install-JumpServerPlugin.ps1 `
  -LegacyConfigPath 'C:\Program Files\JumpServerClient\resources\bin\config.json' -Uninstall
```

新版卸载：

```powershell
.\integrations\jumpserver\Install-JumpServerPlugin.ps1 `
  -PluginRoot 'C:\Program Files\JumpServer\resources\plugins\windows' -Uninstall
```

操作前退出客户端。每次写入前都会备份到 `imgossh-backups/<唯一编号>/`：
旧版位于 config.json 同目录，新版位于用户配置目录。备份中保留操作前完整配置；
旧版额外使用 `imgossh-legacy-restore.json` 记录最初 XShell、WinSCP 条目及其位置。
重复安装保留最初的恢复信息；卸载只恢复占用的槽位，保留其余条目上后续的用户修改。
若用户已在外部替换该槽位，卸载会停止并说明冲突。
新版卸载也保留安装后手动选择的其他工具。备份目录保留供人工恢复。

客户端升级可能替换安装目录配置或插件资源，升级后需重新确认连接器配置；
若更换配置格式，重新运行对应安装命令。Windows 下已有 ImGoSSH 窗口时，JumpServer 的 SSH/SFTP 连接会在现有窗口中新建并选中标签页，同时恢复最小化窗口；没有窗口时正常启动。
连接参数仅通过本机命名管道转交，不写入磁盘。窗口按当前用户和配置目录（`IMGOSSH_HOME`）区分；手动启动仍可打开多个窗口，JumpServer 复用接收窗口。该窗口关闭后，其他已打开窗口会接管后续连接。
临时令牌过期后请从 JumpServer 重新发起连接。

## 适配依据及限制

2.1.4 的实际实现：

- [配置加载器与激活规则](https://github.com/jumpserver/client/blob/v2.1.4/go-client/pkg/config/config.go)
- [Windows 工具选择及参数传递](https://github.com/jumpserver/client/blob/v2.1.4/go-client/pkg/awaken/awaken_windows.go)
- [客户端配置读写](https://github.com/jumpserver/client/blob/v2.1.4/interface/src/background.js)
- [界面图标按内置名称加载](https://github.com/jumpserver/client/blob/v2.1.4/interface/src/renderer/components/ListTable.vue)

v4.1.9 的[开发文档](https://github.com/jumpserver/client/blob/v4.1.9/docs/plugins/DEVELOPER.md)
描述了用户目录插件和 `JMS_CONNECT_JSON` 脚本接口，但其
[Tauri 加载器](https://github.com/jumpserver/client/blob/v4.1.9/src-tauri/src/service/plugin.rs)
和 [Go 加载器](https://github.com/jumpserver/client/blob/v4.1.9/go-client/pkg/plugin/loader.go)
实际只遍历平台资源的 `index.json`，所以新版安装器同时修改索引和用户选择。

SSH 的两种接入方式都使用同一参数模板，不通过 shell 执行，不把凭据拼成 URL：

```text
--jumpserver --host={host} --port={port} --username={username} --password={value}
```

上游启动器替换模板后按空格拆分参数，并对 `*` 特殊处理，因此无法可靠传递含空格或 `*`
的字段。SFTP 模板在 `--jumpserver` 后增加 `--sftp`，其余参数一致。模板只用于网关地址、用户名和临时令牌，不传资产名称，也不适合传任意真实账户密码。
临时令牌会出现在 ImGoSSH 进程命令行中，本机有相应权限的进程可能读取；这些版本的
官方启动接口没有环境变量或标准输入交接支持。

ImGoSSH 临时连接不会写入会话文件，也不会记录启动参数或将令牌用于自动重启。
主机密钥验证保留；JumpServer 模式关闭 agent、自动隧道和自动监控。SSH 模式默认不打开 SFTP 面板；SFTP 模式直接打开文件浏览器，不请求终端或 shell。
macOS/Linux 安装及复用既有窗口暂不支持。

## 验证

```powershell
cargo fmt --all --check
cargo test --workspace --locked
powershell -NoProfile -File .\integrations\jumpserver\Test-Install.ps1
powershell -NoProfile -File .\integrations\jumpserver\Test-AutoConfigure.ps1
```

安装测试以官方 2.1.4 原始配置作为 fixture，覆盖默认工具选择、重复安装、完整恢复、
保留其他协议与平台设置、保留后续修改、路径空格和 `-WhatIf`；同样验证新版插件安装。
SSH 测试分别使用旧版 `arg_format` 和新版插件模板，验证网关临时令牌认证、PTY 和终端收发。
SFTP 测试验证两代模板的临时令牌认证、目录列表、上传和下载，并确认不请求 PTY 或 shell。
自动启动测试模拟桌面客户端退出时写回设置，验证关闭后重读、最新备份、已有配置不动、
损坏或锁定文件不触发关闭、正常关闭超时后的结束处理，以及保留会话 Go 进程。
这些测试仅使用临时目录和本地 SSH 服务器，不修改已安装的官方客户端。
真实验收还需部署的 JumpServer、已授权资产和官方客户端：连接资产并执行命令，
再到 JumpServer 审计中核对会话、命令和录像。
