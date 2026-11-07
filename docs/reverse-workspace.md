# 反向工作区：在服务器修改代码，在客户端运行

客户端连接 SSH 服务器后，可以通过同一条 SSH 连接共享多个本机目录，并在客户端电脑执行命令、查看标准输出、标准错误和退出码。无需在客户端安装 SSH 服务或开放入站端口。

## 启用

GUI：编辑会话 → **反向工作区**，启用共享并配置目录映射，例如 `E:\AI\Strata-data` → `/data/sss`。需要真实挂载时勾选“在服务器上挂载目录”，需要修改文件时勾选“允许修改文件”，需要执行程序时勾选“允许在本机运行程序”。保存后重新连接。

真实目录挂载要求 Linux 服务器安装 sshfs、支持 FUSE，并且当前用户有挂载权限。缺少 sshfs 或单个目录挂载失败时，SSH 连接保持可用，界面显示相应提示。程序执行通过独立控制通道运行，不依赖 sshfs 挂载成功。

服务器的 sshd 必须允许远程 TCP 转发（`AllowTcpForwarding yes` 或 `remote`，必要时配置 `PermitListen`）。`GatewayPorts no` 可保持服务器端口只监听回环地址。

CLI 示例（在客户端 PowerShell 中运行）：

```powershell
igssh user@server --share 'C:\work\project' --share-write --share-exec
```

CLI 将目录挂载到服务器的 `/workspace`；省略 `--share-write` 为只读，省略 `--share-exec` 则禁止执行程序。`--share-port 9876` 指定控制端口，默认由服务器自动分配。

## 从服务器运行客户端程序，无需 Python

在 GUI 终端上方点击 **复制服务器端配置命令**，将命令粘贴到服务器的 POSIX shell 执行一次。它设置本次连接的访问凭据、下载 Shell 辅助脚本并在当前 shell 定义 `igworkspace` 命令。

服务器只需要 shell、curl 和常见系统工具（mktemp、cat、sed、tail、sleep、rm、rmdir），不需要 Python 或 jq。重新连接或打开新 shell 后需要重新执行配置命令。

下面的命令在 **服务器 SSH 终端** 输入，程序实际运行在 **客户端电脑**：

```bash
igworkspace info
# Windows 客户端：确认执行主机与工作目录
igworkspace run 'hostname; Get-Location' --cwd /data/sss
igworkspace run 'python main.py' --cwd /data/sss --timeout 600

# 后台运行，记录返回的任务 ID
igworkspace run 'cargo test' --cwd /data/sss --detach
igworkspace logs 3
igworkspace logs 3 --follow
igworkspace cancel 3
```

`--cwd` 可以使用配置的服务器挂载路径或其子目录，省略时选择第一个配置目录。Windows 客户端使用 PowerShell，Unix 客户端使用 `/bin/sh`。命令语法、环境变量及运行时以客户端为准；例如运行 Python 程序时，只需要客户端安装 Python。

前台运行和 `logs --follow` 会持续显示日志并返回任务退出码。Ctrl+C 停止等待后任务继续运行，可以使用任务 ID 取消。断开 SSH 连接会取消运行中的客户端任务。

详细操作见 [从服务器 SSH 终端运行客户端程序](server-client-execution.md)。

## 文件访问与可选 Python 工具

挂载成功后可直接使用服务器编辑器及命令读写 `/data/sss`，修改会作用到对应的客户端目录。

未挂载时仍可使用 HTTP 文件 API。原 Python 辅助工具保持兼容，但属于可选工具，默认初始化流程不会调用它。服务器安装 Python 3 后可手动下载：

```bash
curl --noproxy '*' --fail -H "Authorization: Bearer $IMGOSSH_WORKSPACE_TOKEN" \
  "$IMGOSSH_WORKSPACE_URL/client.py" -o ./igworkspace.py
python3 ./igworkspace.py ls
python3 ./igworkspace.py get src/main.py ./main.py
vim ./main.py
python3 ./igworkspace.py put ./main.py src/main.py
```

文件 API 的相对路径以第一个共享目录为根，也支持配置的服务器挂载路径。单文件 API 请求上限为 16 MiB。

## 权限与任务生命周期

- 共享默认关闭，文件写入与程序执行分别启用。每次连接生成新令牌，不写入会话配置；只应向可信服务器用户提供令牌。
- 文件 API 拒绝越界路径及跨目录符号链接/Windows junction，上传时原子替换文件。共享目录及其父目录应由可信本机用户管理。
- 程序使用客户端当前用户的权限执行，工作目录不是进程沙箱。即使文件 API 为只读，启用执行权限后，程序仍可修改文件和访问其他本机资源。
- 取消、超时及断开连接会终止进程树；主动脱离进程树或会话的程序不适合作为此类任务运行。
- 最多同时运行 4 个任务，保留 64 条任务记录。默认超时 300 秒，可设置 1–3600 秒。stdout 和 stderr 各保留最后 1 MiB；日志滚动后 Shell 工具使用字节偏移继续读取，并提示丢失的旧日志。

## HTTP API

所有请求使用 `Authorization: Bearer $IMGOSSH_WORKSPACE_TOKEN`。URL 为服务器回环地址；路径使用 UTF-8 URL 编码。

| 请求 | 用途 |
| --- | --- |
| `GET /info` | 客户端系统、目录映射及权限 |
| `GET /list?path=src` | 列出目录，返回 JSON `entries` |
| `GET /file?path=src/main.py` | 返回原始文件字节 |
| `PUT /file?path=src/main.py` | 用请求体替换文件，父目录必须存在 |
| `POST /mkdir?path=src/new` | 新建一层目录 |
| `POST /run` | JSON：`{"command":"python main.py","cwd":"src","timeout_secs":300}`，返回 `{"id":1}` |
| `GET /jobs/1` | JSON：状态、退出码、日志及截断标记 |
| `DELETE /jobs/1` | 请求取消任务 |
| `GET /client.sh` | 获取默认 Shell 工具 |
| `GET /client.py` | 获取可选 Python 工具 |
| `POST /shell/run` | 表单字段 `command`、`cwd`、`timeout_secs`，返回纯数字任务 ID；空 cwd 使用第一个目录 |
| `GET /shell/jobs/1/status` | 两行文本：状态、退出码（未退出时为空） |
| `GET /shell/jobs/1/stdout?path=0` | 两行元数据：下次字节偏移、旧日志丢失标记（0/1），之后为原始日志字节 |
| `GET /shell/jobs/1/stderr?path=0` | 与 stdout 相同，偏移分别计算 |

任务状态为 `running`、`finished`、`failed`、`timed_out` 或 `cancelled`。非零退出码仍属于 `finished`，启动失败为 `failed`。
