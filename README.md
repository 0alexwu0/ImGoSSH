# ImGoSSH

[![Release](https://img.shields.io/github/v/release/0alexwu0/ImGoSSH?label=release)](https://github.com/0alexwu0/ImGoSSH/releases/latest)
[![Build](https://img.shields.io/github/actions/workflow/status/0alexwu0/ImGoSSH/release.yml?label=build)](https://github.com/0alexwu0/ImGoSSH/actions/workflows/release.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-4b7dbe.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.95%2B-orange.svg)](https://www.rust-lang.org/)

**ImGoSSH 是一个用 Rust 编写的原生 SSH / SFTP / SCP 工作站。**

它把多标签终端、会话管理、SFTP 文件浏览器、传输队列和 SSH 隧道放在一个桌面应用里，同时提供三个 OpenSSH 风格的命令行工具：

- `imgossh`：图形客户端
- `igssh`：交互式 SSH shell 或单条远程命令
- `igscp`：本地与远程之间的文件复制
- `igsftp`：交互式 SFTP 命令行

![ImGoSSH GUI](docs/gui.png)

GUI 与 CLI 共用 `src/ssh`、`src/terminal` 和配置代码。连接、认证、主机密钥和文件传输因此保持一致。

[下载](#下载) · [开始使用](#开始使用) · [GUI 功能](#gui-功能) · [命令行工具](#命令行工具) · [源码构建](#从源码构建) · [配置](#配置与数据) · [开发与测试](#测试)

## 下载

最新版本：[v0.1.6](https://github.com/0alexwu0/ImGoSSH/releases/tag/v0.1.6)

| 平台 | 安装包 | 运行方式 |
| --- | --- | --- |
| Windows x86-64 | [ZIP](https://github.com/0alexwu0/ImGoSSH/releases/download/v0.1.6/ImGoSSH-0.1.6-windows-x64.zip) | 解压后运行 `imgossh.exe` |
| Linux x86-64 | [tar.gz](https://github.com/0alexwu0/ImGoSSH/releases/download/v0.1.6/ImGoSSH-0.1.6-linux-x86_64.tar.gz) | 解压后执行 `./imgossh` |
| macOS Apple Silicon | [ZIP](https://github.com/0alexwu0/ImGoSSH/releases/download/v0.1.6/ImGoSSH-0.1.6-macos-aarch64.zip) | 解压后执行 `./imgossh` |
| macOS Intel | [ZIP](https://github.com/0alexwu0/ImGoSSH/releases/download/v0.1.6/ImGoSSH-0.1.6-macos-x86_64.zip) | 解压后执行 `./imgossh` |

每个压缩包都包含图形客户端、`igssh`、`igscp`、`igsftp`、`LICENSE` 和 `README.md`。Release 页面同时提供 SHA-256 校验值。

Linux 版本面向 glibc 2.35 及以上环境。macOS 版本目前没有开发者签名和公证，首次运行时可能需要在「系统设置 → 隐私与安全性」中允许打开，或执行：

```bash
xattr -dr com.apple.quarantine <解压目录>
```

## 开始使用

1. 解压对应平台的安装包，启动 `imgossh`。
2. 点击「新建 SSH 会话」，填写主机、端口、用户名及认证方式，保存并连接。
3. 首次连接时核对服务器的主机密钥指纹，再决定是否信任。
4. 在终端执行命令，通过右侧 SFTP 浏览器管理文件；需要时打开传输队列查看任务。

也可以从命令行启动图形客户端：

```bash
imgossh --local
imgossh user@example.com
imgossh --session "Production"
```

`--session` 按已保存会话的名称查找。Windows PowerShell 中，在解压目录运行程序时需要使用 `./imgossh.exe`；三个 CLI 工具也可以加入 `PATH` 后使用。

## GUI 功能

### 会话与终端

- 保存 SSH、SFTP、SCP 和本地终端会话。
- 用文件夹组织会话，支持搜索、重命名、删除和右键操作。
- 多标签终端，支持远程 shell、本地 PTY / Windows ConPTY、窗口标题、响铃、滚动回看和鼠标上报。
- 鼠标拖动选中文字时实时显示选区，支持反向拖选和跨行选择。
- 终端右键菜单在窗口边缘自动调整位置，不会被底部或右侧裁切。
- 支持 16 色、256 色、24 位真彩色、VT/ANSI 查询、OSC 颜色设置和 DEC 特殊图形字符集。

### SFTP 文件浏览器

- 终端右侧停靠远程 / 本地双栏文件浏览器；窄窗口自动切换为单栏模式。
- 浏览目录、前进后退、返回上级、刷新、新建目录、重命名、删除和属性查看。
- 文件上传、下载、另存为和拖放操作。
- 上传文件夹时递归保留目录结构；文件夹中的每个文件单独显示大小、速率、状态和进度。
- 空目录、空文件和嵌套目录都会保留；传输可以单独取消，进度列表支持滚动查看。
- 属性窗口支持查看大小、时间和权限，并可直接修改远端权限。

### 传输、隧道与监控

- SFTP 传输队列提供精确字节数、进度条、速率、状态和取消操作。
- 支持本地转发 `-L`、远端转发 `-R` 和动态 SOCKS5 转发 `-D`。
- 支持密码、公钥、ssh-agent、Windows Pageant 和键盘交互认证。
- 使用 OpenSSH `known_hosts` 格式校验主机密钥，并兼容 `~/.ssh/known_hosts`。
- 可选的服务器状态栏：CPU、内存、交换分区、网络速率、运行时间、登录用户和磁盘使用率。
- 内置更新检查，下载 Release 后校验 SHA-256，再替换本地程序。

### 外观与更新

支持简体中文和英文界面，语言默认跟随系统。设置中可切换 ImGo、Dark、Light 三套界面主题，以及 ImGo Dark、Charcoal、ImGo Light、Xterm 四套终端调色板，调整字体、字号、行距和回滚行数。终端内置 Hack 字体，并为框线、方块等字符提供字体回退。

「帮助 → 检查更新」可以手动检查 GitHub Releases。自动检查默认开启，成功检查后的间隔为 6 小时，可在设置中关闭；发现更新后由用户确认下载和安装，完成后重启生效。

### 常用快捷键

| 快捷键 | 操作 |
| --- | --- |
| `Ctrl+Shift+T` | 新建本地终端 |
| `Ctrl+Shift+N` | 新建会话 |
| `Ctrl+Shift+W` | 关闭当前标签 |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | 复制 / 粘贴 |
| `Ctrl+Shift+B` / `Ctrl+Shift+H` | 显示或隐藏 SFTP 浏览器 / 会话侧栏 |
| `Ctrl+Shift+R` | 重新连接或重新打开当前会话 |
| `Ctrl+Shift+S` / `Ctrl+Shift+U` | 设置 / 传输队列 |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | 下一个 / 上一个标签 |
| `Alt+1` 至 `Alt+9`、`Alt+0` | 切换到第 1 至第 10 个标签 |

常用的 `Ctrl+C`、`Ctrl+A`、`Ctrl+L` 等组合键交给终端中的程序处理。

## 命令行工具

### `igssh`

打开远程 shell：

```bash
igssh user@example.com
igssh -p 2222 -i ~/.ssh/id_ed25519 user@example.com
```

运行单条命令：

```bash
igssh user@example.com -e "uname -a"
igssh user@example.com --exec -e "uptime"
```

SSH 隧道：

```bash
igssh -L 8080:localhost:80 user@example.com
igssh -R 9000:localhost:22 user@example.com
igssh -D 1080 user@example.com
```

常用选项包括 `-l`、`-p`、`-i`、`-w`、`-a`、`--keepalive` 和 `--term`。

### `igscp`

上传文件或目录：

```bash
igscp -r ./build user@example.com:/tmp/
igscp -i ~/.ssh/id_ed25519 app.tar.gz user@example.com:/tmp/
```

下载文件或目录：

```bash
igscp user@example.com:/var/log/app.log ./logs/
igscp -r user@example.com:/var/log/app ./logs/
```

`-q` 可关闭进度输出。当前不支持两个远程主机之间的直接复制。

### `igsftp`

```bash
igsftp user@example.com
igsftp user@example.com -o "ls -l" -o "get app.log ./app.log"
igsftp user@example.com --batch commands.txt
```

交互提示符支持 `pwd`、`lpwd`、`cd`、`lcd`、`ls`、`get`、`put`、`mkdir`、`rmdir`、`rm`、`rename`、`stat` 和 `quit`。输入 `help` 查看命令说明；各工具的启动参数可通过 `--help` 查看。CLI 采用 OpenSSH 风格的常用参数，但不保证支持 OpenSSH 的全部选项和配置语法。

## 从源码构建

要求：

- Rust 1.95 或更新版本
- Windows：stable MSVC 工具链
- Linux：Rust 工具链和 C 编译器
- macOS：Xcode Command Line Tools

```bash
git clone https://github.com/0alexwu0/ImGoSSH.git
cd ImGoSSH

cargo run
cargo run -- --local
cargo run -- user@example.com

cargo build --release
```

Release 构建会在 `target/release/` 生成四个程序。Windows 构建会尝试使用 Windows SDK 的 `rc.exe` 将 `assets/icon.png` 编译进程序图标；找不到时会保留默认图标，不会让构建失败。

## 配置与数据

默认数据目录：

| 系统 | 目录 |
| --- | --- |
| Windows | `%APPDATA%/ImGoSSH/` |
| Linux | `~/.config/ImGoSSH/` |
| macOS | `~/Library/Application Support/ImGoSSH/` |

主要文件：

| 文件 | 内容 |
| --- | --- |
| `config.toml` | 主题、字体、回滚、默认会话、窗口状态和更新设置 |
| `sessions.toml` | 会话、文件夹、启动脚本和隧道 |
| `known_hosts` | ImGoSSH 使用的主机密钥库 |

设置 `IMGOSSH_HOME` 可以把整个数据目录移动到其他位置，例如 U 盘。调试时设置 `RUST_LOG=debug`。

会话文件示例：

```toml
[[session]]
name = "Production"
folder = "Clients"
protocol = "ssh"
host = "192.0.2.10"
port = 22
username = "root"
private_key = "C:/Users/me/.ssh/id_ed25519"
use_agent = true
sftp_panel = true
keepalive = 60
startup_script = "tmux attach || tmux"

[[session.tunnels]]
kind = "local"
listen_port = 8080
remote_host = "localhost"
remote_port = 80
```

## 技术架构

ImGoSSH 不依赖本机 OpenSSH、Electron 或 WebView。SSH 协议、SFTP、终端仿真和 GUI 都由 Rust 组件完成。

主要技术栈：`russh` / `russh-sftp` 负责 SSH 与 SFTP，`tokio` 负责异步任务，`vt100` 负责终端仿真，`portable-pty` 提供本地伪终端，`egui` / `eframe` 绘制原生界面，`clap` 解析命令行，`serde` / `toml` 保存配置。

| 模块 | 作用 |
| --- | --- |
| `src/ssh/client.rs` | SSH 连接 actor、认证、shell、exec、SFTP 和隧道 |
| `src/ssh/sftp.rs` | 远程目录、属性、权限和文件操作 |
| `src/ssh/transfer.rs` | 上传下载队列、逐文件进度、速率和取消 |
| `src/ssh/hostkeys.rs` | OpenSSH 主机密钥读写与匹配 |
| `src/ssh/local.rs` | 本地 PTY，Windows 使用 ConPTY |
| `src/ssh/monitor.rs` | 服务器实时指标 |
| `src/terminal.rs` | 终端仿真、按键、鼠标、剪贴板和颜色查询 |
| `src/ui/` | 会话侧栏、终端、SFTP 浏览器、设置和对话框 |
| `src/update.rs` | GitHub Release 检查、下载、校验和更新 |
| `src/bin/` | `igssh`、`igscp` 和 `igsftp` |

每条 SSH 连接由一个 Tokio actor 持有底层句柄，GUI 和 CLI 通过可复制的 `Connection` 发送操作；终端仿真与远程 SSH、本地 PTY 使用同一套逻辑。

## 测试

运行本地单元、GUI 几何和本地终端测试：

```bash
cargo fmt --all --check
cargo test --workspace
```

[Release 工作流](.github/workflows/release.yml) 配置了 Windows、Linux 和两种 macOS 架构的构建与打包，并为原生目标配置工作区测试；macOS Intel 使用交叉编译，不运行测试。

需要真实 SSH 服务器的测试默认忽略。配置环境变量后运行：

```powershell
$env:IMGOSSH_TEST_HOST = "your-server.example.com"
$env:IMGOSSH_TEST_USER = "root"
$env:IMGOSSH_TEST_PASS = "your-password"
$env:IMGOSSH_TEST_PORT = "22"
cargo test --test live_server -- --ignored --nocapture --test-threads=1
```

## 安全与限制

- `sessions.toml` 可能明文保存密码。共享机器上建议使用私钥、ssh-agent 或 Pageant，并关闭密码保存。
- 首次连接会校验主机密钥；指纹变化时会阻断连接并要求确认。
- 主机密钥会从应用数据目录和 `~/.ssh/known_hosts` 读取，新接受的密钥写入应用数据目录；目前不匹配 OpenSSH 的哈希主机名条目。
- 文件传输使用 SFTP 子系统，服务器需要启用 SFTP 服务。
- 当前不包含 X11、RDP、VNC、FTP、Telnet 或串口功能。
- 文件浏览器不会自动跟随终端中的 `cd`。
- Linux 和 macOS 已通过自动化构建与测试，但不同桌面环境的字体、剪贴板和文件对话框仍可能存在差异。

## 参与贡献

欢迎提交 Issue 和 Pull Request。提交前建议运行：

```bash
cargo fmt --all --check
cargo clippy --all-targets
cargo test --workspace
```

新增功能或修复终端、主机密钥、路径和传输逻辑时，请同时补充回归测试；配置格式、快捷键或界面行为变化时，请同步更新 README。

## 开源协议

ImGoSSH 使用 [Apache License 2.0](LICENSE) 发布。第三方依赖继续遵循各自的许可证。

Copyright 2026 Alex.Wu.
