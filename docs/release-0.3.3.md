本版本改进终端配色、系统等宽字体和反色文字显示，并使原生 Windows 标题栏跟随应用主题。

## 改动

- **Classic 配色**：新增深灰背景和高饱和 ANSI 颜色，可在设置中选择。终端监控程序的青色菜单、绿色表头和黄色曲线更清晰。
- **系统等宽字体**：新配置默认采用 Windows 的 Consolas、macOS 的 Menlo 或 Linux 的 DejaVu Sans Mono；已有字体、字号和行距设置继续保留。缺少字体时使用回退，框线、方块和中文字符继续保持终端网格对齐。
- **修正反色**：先解析原始前景、背景和粗体颜色，再交换前景与背景，避免反色菜单把默认背景误画成浅色文字。
- **原生标题栏主题**：继续使用系统绘制的窗口按钮、拖动和缩放。Win11 通过 DWM 同步标题栏、文字与边框颜色；Win10 使用系统深色或浅色标题栏。关闭窗口库的 DarkMode_Explorer 样式覆盖，主题改变时才刷新框架，避免逐帧更新。

## 下载与升级

提供 Windows x86-64、Linux x86-64、macOS Apple Silicon、macOS Intel 压缩包及 `SHA256SUMS.txt`。每个包包含图形客户端、`igssh`、`igscp`、`igsftp`、使用文档和集成脚本。解压替换程序即可，已有配置与会话继续使用。

Linux 需要 glibc 2.35 或更新版本；macOS 程序未签名和公证。

## 验证与限制

本地 Windows 格式检查、128 项测试以及 JumpServer 安装与自动配置脚本测试通过；9 项需要额外环境的测试按条件跳过。回归检查覆盖 ANSI 反色、字体回退和网格对齐、SSH 焦点及输入法、主题切换、菜单操作和连续输入时的布局稳定性。

Win10 无法通过所用接口精确设置自定义标题栏颜色；其标题栏点击、输入及高 DPI 表现仍需实机确认，自动化布局检查不代表所有 Win10 环境的抖动和绘制问题均已解决。

使用说明：[README](https://github.com/0alexwu0/ImGoSSH/blob/v0.3.3/README.md) · [反向工作区](https://github.com/0alexwu0/ImGoSSH/blob/v0.3.3/docs/reverse-workspace.md)。
