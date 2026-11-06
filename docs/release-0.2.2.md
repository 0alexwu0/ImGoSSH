本版本新增 JumpServer SSH / SFTP 接入和启动自动配置，并修正 Windows 标题栏主题同步。

## 新增与改进

- **JumpServer SSH / SFTP**：最低支持 Windows 上的 JumpServer Client 2.1.4，兼容旧版 config.json 和 v4.1.9 索引插件。SSH 打开交互终端，SFTP 直接打开文件浏览器，可浏览目录、上传和下载。
- **启动自动配置**：正常打开 ImGoSSH 时检查 SSH 与 SFTP 配置；缺少时关闭对应 JumpServer 桌面客户端，重新读取退出时保存的配置，先备份再写入。已有完整配置时不关闭、不重复写入；此前仅有 SSH 的安装会自动补齐 SFTP。
- **临时连接**：使用网关地址、JMS 用户名和临时令牌连接；不写入已保存会话，关闭 SSH agent、自动隧道和监控。由 JumpServer 唤起时跳过自动配置，避免干扰当前连接。
- **GUI 启动参数**：新增外部客户端使用的主机、端口、用户名、密码、私钥及 SFTP 模式参数。
- **Windows 标题栏**：同步原生窗口主题，增加旧版 Win10 深色接口回退、启动后有限重试及恢复焦点时刷新，处理偶发白色标题栏和深色界面不一致。

## 使用与升级

解压后正常打开新版 imgossh.exe，即可自动检查 JumpServer 配置。写入成功后重新打开 JumpServer，再选择 SSH 或 SFTP 客户端连接资产。配置在受保护目录且无写权限时会提示失败原因，可用管理员身份重新打开 ImGoSSH。

关闭 JumpServer 时先请求正常退出，4 秒后仍未退出会结束对应桌面进程；不会结束 resources/bin 下承载连接的 Go 启动器。旧版客户端借用 XShell 和 WinSCP 图标槽位，卸载脚本可恢复原条目。配置备份、手动指定路径和卸载方法见 [JumpServer 集成说明](https://github.com/0alexwu0/ImGoSSH/blob/v0.2.2/integrations/jumpserver/README.md)。

JumpServer 临时令牌通过客户端支持的进程参数传递，本机有相应权限的进程可能读取。主机密钥验证保留。

## 下载与运行

每个包包含 GUI、igssh、igscp、igsftp、LICENSE、README.md 以及 JumpServer 集成文件。

- Windows x86-64：解压后运行 imgossh.exe；JumpServer 自动配置仅支持 Windows。
- Linux x86-64：需要 glibc 2.35 或更新版本；自动解锁需要可用的 D-Bus / Secret Service。
- macOS Apple Silicon / Intel：按架构选择压缩包，程序未签名和公证。

## 验证与限制

已运行格式检查、工作区测试、连接器安装和自动配置隔离测试。SSH 测试验证临时凭据认证、PTY 和终端收发；SFTP 测试验证两代模板的目录列表、上传和下载，以及不请求 shell。发布流程为 Windows、Linux 和两种 macOS 架构构建安装包，运行原生目标测试并提供 SHA-256 校验值。

真实 JumpServer 资产权限、文件传输与审计仍需部署环境联调。Win10 标题栏修复在 Win11 开发机上完成回归测试，尚需在发生问题的 Win10 机器上确认。
