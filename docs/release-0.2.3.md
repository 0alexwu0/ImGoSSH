本版本新增反向工作区：在 Linux 服务器访问客户端指定目录、修改代码，并将程序交给客户端电脑运行，日志和退出码返回服务器终端。

## 新增与改进

- **多个目录映射**：在会话属性的“反向工作区”中分别配置客户端目录与服务器挂载位置，例如 `E:\AI\Strata-data` → `/data/sss`，并保存文件修改及本机执行权限。
- **真实目录挂载**：通过 SSH 反向转发与 sshfs/FUSE 挂载，服务器编辑器、终端命令可直接访问客户端目录。界面按配置显示“已挂载 本机目录:服务器目录”。缺少 sshfs 或单个目录挂载失败时保持 SSH 连接，连接后提示安装依赖或显示错误。
- **客户端程序执行**：提供 GUI“在本机运行”和服务器端 `igworkspace run` 入口，支持工作目录、超时、实时标准输出/标准错误、退出码、后台任务、日志跟踪与取消；断开连接后取消运行中的任务。
- **服务器无需 Python**：默认初始化入口改为 Shell + curl，只依赖常见系统工具，无需 Python 或 jq。原 Python 工具与 JSON API 保持兼容，可按需使用。
- **中文与日志读取**：命令和工作目录按 UTF-8 传输；Shell 工具使用字节偏移读取新增日志，日志缓存滚动后提示丢失的旧输出。
- **远程转发修正**：按请求的服务器监听端口，将连接路由到已配置的客户端目标端口，支持工作区控制通道和普通远程隧道。
- **CLI 与文档**：新增 `igssh --share`、`--share-write`、`--share-exec` 和 `--share-run` 等选项；安装包包含反向工作区脚本、使用文档和原有 JumpServer 连接器。

## 使用

1. 编辑会话 →“反向工作区”，启用共享、添加目录映射；需要真实挂载时勾选“在服务器上挂载目录”。
2. 需要编辑文件或运行程序时，分别启用对应权限，保存并重新连接。
3. 在服务器的挂载目录中使用编辑器修改代码。点击终端上方“复制服务器端配置命令”，在服务器 POSIX shell 执行一次。
4. 在同一服务器终端输入以下命令，程序实际在 Windows 客户端执行：

```sh
igworkspace run 'hostname; Get-Location' --cwd /data/sss
igworkspace run 'python main.py' --cwd /data/sss --timeout 600
igworkspace run 'python main.py' --cwd /data/sss --detach
igworkspace logs 1 --follow
igworkspace cancel 1
```

将 `1` 替换为返回的任务 ID。Windows 客户端使用 PowerShell，Unix 客户端使用 `/bin/sh`。Python 等运行时只需安装在实际执行程序的客户端电脑上。重新连接或打开新 shell 后需要重新执行配置命令。

服务器的 sshd 必须允许远程 TCP 转发。真实挂载还要求 Linux、sshfs、FUSE 及当前用户的挂载权限；运行控制通道不依赖挂载成功。初始化与运行命令中的 `curl --noproxy '*'` 只对本次工作区请求绕过代理，不修改服务器的系统代理或代理环境变量。

详细说明：[反向工作区](https://github.com/0alexwu0/ImGoSSH/blob/v0.2.3/docs/reverse-workspace.md) · [服务器终端运行客户端程序](https://github.com/0alexwu0/ImGoSSH/blob/v0.2.3/docs/server-client-execution.md)。

## 升级与权限

升级保留现有会话；反向工作区默认关闭，文件写入与程序执行分别启用。每次连接生成新的访问凭据。执行程序使用客户端当前用户的系统权限，工作目录不构成进程沙箱，只应对可信服务器授权。

任务默认超时 300 秒，可设置 1–3600 秒；最多同时运行 4 个任务，保留 64 条任务记录。stdout 和 stderr 各保留最后 1 MiB；单文件 API 请求上限 16 MiB。

## 下载与验证

提供 Windows x86-64、Linux x86-64、macOS Apple Silicon 和 macOS Intel 安装包及 SHA-256 校验值。Linux 需要 glibc 2.35 或更新版本；macOS 程序未签名和公证。

本地已通过格式检查、构建检查及 118 项回归测试，并单独验证原 Python 辅助工具兼容性。反向工作区测试通过本地模拟 SSH 服务器验证真实 SSH 转发、文件修改、多目录映射、权限、任务输出、退出码、取消、超时和断线清理；服务器侧找不到 Python、Python3 或 jq 时的 Shell 初始化与执行流程已通过测试。

真实 Linux sshfs/FUSE 挂载仍需部署环境验证。发布流程会为 Windows、Linux 和 macOS 两种架构构建安装包，并对原生目标运行测试。
