# 从服务器 SSH 终端运行客户端程序

在 ImGoSSH GUI 中登录服务器 A 后，可以在 A 的 SSH 终端提交命令，让程序在客户端电脑执行，输出也返回 A 的终端。

1. 会话属性 → 反向工作区：启用共享，并勾选“允许在本机运行程序”。例如配置 `E:\AI\Strata-data` → `/data/sss`。
2. 保存并重新连接。在终端上方点击“复制服务器端配置命令”，将复制的命令粘贴到 A 的 SSH 终端执行一次。服务器只需要 POSIX shell、curl 和常见系统工具（mktemp、cat、sed、tail、sleep、rm、rmdir），不需要 Python 或 jq。该命令设置本次连接的访问凭据、下载 Shell 脚本并在当前 shell 定义 `igworkspace` 命令；重新连接或打开新 shell 后需要重新执行。
3. 在同一终端中运行：

```bash
igworkspace run 'python main.py' --cwd /data/sss
```

这里的工作目录 `/data/sss` 对应客户端的 `E:\AI\Strata-data`，所以 `main.py` 使用客户端安装的 Python 运行，终端会显示实时输出和退出码。客户端是 Windows 时，命令按 PowerShell 语法解释。运行 Python 程序时，只有实际执行它的客户端电脑需要安装 Python；也可以运行 Node.js、Cargo 或其他本机程序。省略 `--cwd` 时使用第一个配置目录。

先用下面的命令验证执行位置（Windows 客户端）：

```bash
igworkspace run 'hostname; Get-Location' --cwd /data/sss
```

后台运行和查看日志：

```bash
igworkspace run 'python main.py' --cwd /data/sss --detach
igworkspace logs 1 --follow
igworkspace cancel 1
```

将 `1` 替换为提交任务后返回的 ID。长任务可以指定 `--timeout 600`（秒）。`igworkspace info` 查看配置；`logs 1` 查看当前日志，`logs 1 --follow` 持续跟踪。Ctrl+C 停止等待后任务会继续运行，可以用任务 ID 取消。保持 GUI 中的 SSH 连接打开，断开连接会取消客户端任务。

真实目录挂载和程序执行共享同一条 SSH 连接，执行命令通过独立控制端口转发。即使服务器缺少 sshfs，控制通道仍可执行客户端程序；服务器必须允许 SSH 远程 TCP 转发。

原 `igworkspace.py` 仍保留为可选工具，文件 API 和 JSON 任务 API 保持兼容。默认初始化命令使用 Shell 工具，不会调用 Python。
