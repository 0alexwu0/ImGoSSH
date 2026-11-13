本版本改进 Windows 下的 JumpServer 唤起行为：已有 ImGoSSH 窗口时，新的 SSH/SFTP 连接在现有窗口中新建标签页。

## 改动

- **复用现有窗口**：JumpServer 发起连接时，新建并选中 SSH 终端或 SFTP 标签页，同时恢复最小化窗口；没有运行窗口时正常启动应用。
- **启动与并发处理**：窗口尚在初始化时也能接收连接；并发唤起按请求创建标签页。接收窗口关闭后，其他已打开窗口可以接管后续连接。
- **临时凭据转交**：通过本机命名管道传递连接参数，不写入磁盘，保留 JumpServer 临时连接模式。窗口按当前用户和配置目录（`IMGOSSH_HOME`）区分。
- **保留手动多窗口**：手动启动仍可打开多个窗口；应用内更新重启前释放接收端，允许新版程序正常启动。

## 下载与升级

提供 Windows x86-64、Linux x86-64、macOS Apple Silicon、macOS Intel 压缩包及 `SHA256SUMS.txt`。每个包包含图形客户端、`igssh`、`igscp`、`igsftp`、使用文档和集成脚本。解压替换程序即可，已有配置与会话继续使用。

**升级后请关闭并重新打开已有 ImGoSSH 窗口**，使其具备接收 JumpServer 连接的能力。已有 JumpServer 参数模板无需修改。临时令牌过期后，请从 JumpServer 重新发起连接。

Linux 需要 glibc 2.35 或更新版本；macOS 程序未签名和公证。本次窗口复用仅支持 Windows。

## 验证与限制

本地 Windows 格式检查、133 项自动测试及 JumpServer 安装与自动配置脚本测试通过；9 项需要额外环境的测试按条件跳过。新增检查覆盖跨进程参数转交、初始化期间排队、并发请求、窗口接管、SSH/SFTP 标签页创建与选中，以及恢复窗口指令。

自动测试使用本机命名管道和本地 SSH 网关；实际 JumpServer 资产连接、审计及系统前台焦点行为仍需在部署环境中确认。

使用说明：[README](https://github.com/0alexwu0/ImGoSSH/blob/v0.3.4/README.md) · [JumpServer 集成](https://github.com/0alexwu0/ImGoSSH/blob/v0.3.4/integrations/jumpserver/README.md)。
