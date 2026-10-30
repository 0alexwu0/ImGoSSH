//! A very small translation layer.
//!
//! English is the source language: every UI string in the codebase is written
//! as `t!("Save")`, and the English literal doubles as the lookup key. When the
//! active language is Chinese the literal is mapped through [`tr`]; anything
//! missing from the table simply falls back to English, so adding a new string
//! never breaks the interface.
//!
//! Templates that carry values use explicit placeholders - `t!("{0} files", n)`
//! - because a translated template has to reorder them freely.

use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};

/// Translate a UI string: `t!("Save")` or `t!("{0} of {1}", done, total)`.
#[macro_export]
macro_rules! t {
    ($key:literal) => { $crate::i18n::tr($key) };
    ($key:literal, $($arg:expr),+ $(,)?) => {
        $crate::i18n::trf($key, &[$(&$arg as &dyn ::core::fmt::Display),+])
    };
}
/// A user interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Lang {
    #[default]
    /// English.
    En = 0,
    /// Simplified Chinese.
    Zh = 1,
}

impl Lang {
    /// The value stored in the settings file.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Zh => "zh",
        }
    }

    /// The name shown in the language picker, always in its own language.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Zh => "简体中文",
        }
    }

    /// Parse a stored setting; unknown values mean "follow the system".
    pub fn parse(name: &str) -> Option<Lang> {
        match name.trim().to_ascii_lowercase().as_str() {
            "en" | "english" => Some(Lang::En),
            "zh" | "zh-cn" | "zh-hans" | "chinese" => Some(Lang::Zh),
            _ => None,
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(Lang::En as u8);

/// Set the language used by every widget.
pub fn set(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

/// The language currently in effect.
pub fn current() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Lang::Zh,
        _ => Lang::En,
    }
}

/// True when Chinese strings should be shown.
pub fn is_zh() -> bool {
    current() == Lang::Zh
}

/// The language of the current user's desktop, used by the "auto" setting.
pub fn detect() -> Lang {
    from_env().unwrap_or_else(|| {
        #[cfg(windows)]
        {
            windows_ui_language().unwrap_or(Lang::En)
        }
        #[cfg(not(windows))]
        {
            Lang::En
        }
    })
}

/// `IMGOSSH_LANG` lets a test pin the language; otherwise the usual locale
/// variables decide.
fn from_env() -> Option<Lang> {
    for name in ["IMGOSSH_LANG", "LC_ALL", "LANGUAGE", "LANG"] {
        let Ok(value) = std::env::var(name) else {
            continue;
        };
        let lower = value.to_lowercase();
        if lower.contains("zh") {
            return Some(Lang::Zh);
        }
        if lower.starts_with("en") {
            return Some(Lang::En);
        }
    }
    None
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetUserDefaultUILanguage() -> u16;
}

/// The user's UI locale. The primary language id is the low ten bits and
/// `0x04` is Chinese.
#[cfg(windows)]
fn windows_ui_language() -> Option<Lang> {
    let lang = unsafe { GetUserDefaultUILanguage() };
    if lang == 0 {
        return None;
    }
    Some(match lang & 0x03ff {
        0x04 => Lang::Zh,
        _ => Lang::En,
    })
}

/// Translate one English literal.
pub fn tr(key: &'static str) -> &'static str {
    if !is_zh() {
        return key;
    }
    zh(key)
}

/// Translate a template containing `{0}`, `{1}`, ... and fill in the values.
pub fn trf(key: &'static str, args: &[&dyn Display]) -> String {
    let template = tr(key);
    let mut out = String::with_capacity(template.len() + args.len() * 8);
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        match rest[start + 1..start + end].parse::<usize>() {
            Ok(index) => {
                if let Some(value) = args.get(index) {
                    use std::fmt::Write;
                    let _ = write!(out, "{value}");
                }
            }
            // Not an index: a `{}`-style or named placeholder we leave alone.
            Err(_) => out.push_str(&rest[start..=start + end]),
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

/// The Chinese side of the table. Anything absent stays English.
fn zh(key: &'static str) -> &'static str {
    match key {
        // --- generic verbs and nouns -------------------------------------
        "OK" => "确定",
        "Cancel" => "取消",
        "Close" => "关闭",
        "Connect" => "连接",
        "Open" => "打开",
        "Delete" => "删除",
        "Rename" => "重命名",
        "Duplicate" => "创建副本",
        "Browse" => "浏览",
        "Refresh" => "刷新",
        "Reject" => "拒绝",
        "Accept once" => "本次接受",
        "Accept and store" => "接受并保存",
        "Keep" => "保留",
        "Go" => "前往",
        "Name" => "名称",
        "Name:" => "名称：",
        "Size" => "大小",
        "Modified" => "修改时间",
        "Host" => "主机",
        "Port" => "端口",
        "Username" => "用户名",
        "Password" => "密码",
        "Folder" => "文件夹",
        "Protocol" => "协议",
        "Theme" => "主题",
        "Language" => "语言",
        "System default (auto)" => "跟随系统",
        "Fingerprint" => "指纹",
        "Key type" => "密钥类型",
        "Select all" => "全选",
        "Remote" => "远程",
        "Local" => "本地",

        // --- menus and toolbars ------------------------------------------
        "Session" => "会话",
        "Sessions" => "会话",
        "Terminal" => "终端",
        "Transfers" => "传输",
        "Settings" => "设置",
        "Help" => "帮助",
        "Configuration..." => "配置...",
        "New local terminal" => "新建本地终端",
        "New local terminal (Ctrl+Shift+T)" => "新建本地终端 (Ctrl+Shift+T)",
        "New SSH session" => "新建 SSH 会话",
        "New SSH session..." => "新建 SSH 会话...",
        "New SSH session here" => "在此新建 SSH 会话",
        "New SFTP session" => "新建 SFTP 会话",
        "New SFTP browser" => "新建 SFTP 浏览器",
        "New session" => "新建会话",
        "Session properties" => "会话属性",
        "Edit session" => "编辑会话",
        "Reconnect" => "重新连接",
        "Reopen" => "重新打开",
        "Reconnect\tCtrl+Shift+R" => "重新连接\tCtrl+Shift+R",
        "Close tab" => "关闭标签页",
        "Rename tab" => "重命名标签页",
        "Copy\tCtrl+Shift+C" => "复制\tCtrl+Shift+C",
        "Paste\tCtrl+Shift+V" => "粘贴\tCtrl+Shift+V",
        "Toggle SFTP browser" => "显示/隐藏 SFTP 浏览器",
        "Toggle SFTP browser\tCtrl+Shift+B" => "显示/隐藏 SFTP 浏览器\tCtrl+Shift+B",
        "Toggle sidebar\tCtrl+Shift+H" => "显示/隐藏侧边栏\tCtrl+Shift+H",
        "Show transfer queue" => "显示传输队列",
        "Clear finished" => "清除已完成",
        "About ImGoSSH" => "关于 ImGoSSH",
        "Open settings folder" => "打开设置目录",
        "Session log" => "会话日志",

        // --- sidebar ------------------------------------------------------
        "Search sessions" => "搜索会话",
        "+ folder" => "+ 新建文件夹",
        "New folder" => "新建文件夹",
        "New folder in {0}" => "在 {0} 中新建文件夹",
        "Rename {0}" => "重命名 {0}",
        "Delete folder" => "删除文件夹",
        "New subfolder" => "新建子文件夹",
        "Default session" => "默认会话",
        "Create a session with the + buttons at the bottom of the sessions panel." =>
            "点击会话面板底部的 + 按钮创建会话。",
        "No session - double-click a session on the left, or press Ctrl+Shift+T" =>
            "暂无会话——双击左侧会话，或按 Ctrl+Shift+T",

        // --- session editor ----------------------------------------------
        "Session name" => "会话名称",
        "Remote host" => "远程主机",
        "Remote path" => "远程路径",
        "Local path" => "本地路径",
        "SCP direction" => "SCP 方向",
        "Save password" => "保存密码",
        "Uncheck to be asked for the password on every connect" =>
            "取消勾选将在每次连接时询问密码",
        "Passwords are stored in clear text in sessions.toml." =>
            "密码以明文保存在 sessions.toml 里。",
        "Show / hide the password" => "显示/隐藏密码",
        "Private key (.pem / id_*)" => "私钥文件 (.pem / id_*)",
        "Select a private key" => "选择私钥文件",
        "Authentication" => "认证",
        "Terminal type (TERM)" => "终端类型 (TERM)",
        "Window size" => "窗口大小",
        "Keepalive (seconds, 0 = off)" => "保活间隔（秒，0 表示关闭）",
        "Run command instead of shell" => "运行命令而非 shell",
        "Execute script on start" => "启动时执行脚本",
        "SFTP browser" => "SFTP 浏览器",
        "optional - groups sessions in the sidebar" => "可选——用于在侧边栏中分组会话",
        "leave empty to be asked on connect" => "留空则在连接时询问",
        "+ Add a tunnel" => "+ 添加隧道",
        "Local (L): listen here, forward to the remote target.\nRemote (R): listen on the server, forward back here.\nDynamic (D): SOCKS5 proxy on the local port." =>
            "本地 (L)：在本机监听并转发到远程目标。\n远程 (R)：在服务器上监听并转发回本机。\n动态 (D)：在本机端口提供 SOCKS5 代理。",

        // --- host key -----------------------------------------------------
        "The server's identity has not been verified yet." => "服务器身份尚未验证。",
        "The host key changed since the last connection." => "主机密钥与上次连接时不同。",
        "This can mean a man-in-the-middle attack. Only accept the new key if you\nexpect the server to have been reinstalled." =>
            "这可能意味着存在中间人攻击。仅当您确认服务器已重装时才接受新密钥。",

        // --- prompts ------------------------------------------------------
        "Password for {0}" => "{0} 的密码",
        "Passphrase for {0}" => "{0} 的私钥口令",
        "Two-factor authentication - {0}" => "双因素认证 - {0}",
        "The private key {0} is encrypted." => "私钥 {0} 已加密。",
        "Wrong passphrase." => "口令错误。",
        "Access denied, try again." => "验证失败，请重试。",
        "Close this session?" => "关闭此会话？",
        "The connection is still open. Close the tab anyway?" =>
            "连接仍处于打开状态，确定要关闭标签页吗？",

        // --- browser ------------------------------------------------------
        "Filter by name" => "按名称筛选",
        "Back to the previous folder" => "返回上一个文件夹",
        "Forward to the next folder" => "前往下一个文件夹",
        "Parent folder" => "上级文件夹",
        "Clear the path" => "清空路径",
        "New remote folder" => "新建远程文件夹",
        "Open the local folder in Explorer" => "在资源管理器中打开本地文件夹",
        "Download" => "下载",
        "Upload" => "上传",
        "Download the selected remote item" => "下载所选远程项目",
        "Upload the selected local item" => "上传所选本地项目",
        "Rename the selected item" => "重命名所选项目",
        "Delete the selected item" => "删除所选项目",
        "not a folder: {0}" => "不是文件夹：{0}",
        "Upload file" => "上传文件",
        "Select files to upload" => "选择要上传的文件",
        "Choose where to save {0}" => "选择保存 {0} 的位置",
        "upload failed: {0}" => "上传失败：{0}",
        "download failed: {0}" => "下载失败：{0}",
        "create folder failed: {0}" => "新建文件夹失败：{0}",
        "+{0} more" => "还有 {0} 项",

        // --- properties --------------------------------------------------
        "Properties" => "属性",
        "{0} - Properties" => "{0} 属性",
        "Type" => "类型",
        "Location" => "位置",
        "File" => "文件",
        "Symbolic link" => "符号链接",
        "Special file" => "特殊文件",
        "Created" => "创建时间",
        "Accessed" => "访问时间",
        "Owner" => "所有者",
        "Group" => "组",
        "Others" => "其他人",
        "Permissions" => "权限",
        "Octal" => "八进制",
        "Read only" => "只读",
        "Apply" => "应用",
        "Loading" => "读取中",
        "Permissions updated" => "权限已更新",
        "Use one to four octal digits." => "请输入 1-4 位八进制数字。",
        "This file system has no permission bits." => "此文件系统没有权限位。",
        "On the server it is {0} ({1})" => "服务器上的权限为 {0}（{1}）",
        "Permissions are set with a single SFTP request." => "权限通过一次 SFTP 请求完成设置。",
        "The server reports a birth time only when it can." => "只有服务器允许时才能读到创建时间。",
        "Cancel this transfer" => "取消该传输",

        // --- transfers ----------------------------------------------------
        "No transfers." => "没有传输任务。",
        "Nothing queued. Fill in the remote and local paths in the session properties." =>
            "队列为空。请在会话属性中填写远程路径和本地路径。",
        "queued" => "排队中",
        "transferring" => "传输中",
        "done" => "已完成",
        "cancelled" => "已取消",
        "failed: {0}" => "失败：{0}",
        "{0} transfer(s)" => "{0} 个传输任务",

        // --- status bar / monitor ----------------------------------------
        "connecting..." => "正在连接...",
        "connected" => "已连接",
        "closed" => "已断开",
        "exit code {0}" => "退出码 {0}",
        "The connection was closed" => "连接已断开",
        "The session ended - exit code {0}" => "会话已结束，退出码 {0}",
        "The shell has exited" => "Shell 已退出",
        "reading server stats…" => "正在读取服务器状态…",
        "Logged-in accounts" => "已登录账户",
        "Uptime" => "运行时间",
        "Sent since the previous sample" => "自上次采样以来发送",
        "Received since the previous sample" => "自上次采样以来接收",
        "CPU {0}%" => "CPU {0}%",
        "{0}% of RAM in use" => "内存使用率 {0}%",
        "{0} tunnel(s)" => "{0} 个隧道",
        "SFTP browser - {0}" => "SFTP 浏览器 - {0}",
        "Swap in use" => "已用交换空间",

        // --- settings -----------------------------------------------------
        "Colour scheme" => "配色方案",
        "Palette: {0}" => "配色：{0}",
        "Terminal font size" => "终端字号",
        "LOCAL" => "\u{672c}\u{5730}",
        "Double-click to connect" => "\u{53cc}\u{51fb}\u{8fde}\u{63a5}",
        "Font size" => "\u{5b57}\u{53f7}",
        "Line spacing" => "\u{884c}\u{8ddd}",
        "Extra air between terminal rows. Larger values suit Chinese." =>
            "\u{7ec8}\u{7aef}\u{6bcf}\u{884c}\u{6587}\u{5b57}\u{4e0a}\u{4e0b}\u{7684}\u{989d}\u{5916}\u{7559}\u{767d}\u{ff0c}\u{4e2d}\u{6587}\u{8f83}\u{591a}\u{65f6}\u{53ef}\u{9002}\u{5f53}\u{52a0}\u{5927}",
        "Scrollback lines" => "回滚行数",
        "Keepalive" => "保活",

        // --- online updates ----------------------------------------------
        "Updates" => "更新",
        "Check for updates..." => "检查更新…",
        "Check GitHub for a newer version" => "向 GitHub 检查新版本",
        "Checked when ImGoSSH starts and again every six hours." =>
            "启动时检查一次，运行期间每六小时再检查一次。",
        "Last checked: {0}" => "上次检查：{0}",
        "Never checked" => "尚未检查过",
        "New version {0}" => "新版本 {0}",
        "Update available" => "发现新版本",
        "Click to see what is new" => "点击查看更新内容",
        "Click to watch the download" => "点击查看下载进度",
        "Click to watch the update" => "点击查看更新进度",
        "Downloading {0}%" => "下载中 {0}%",
        "Restart to finish" => "重启完成更新",
        "The new version is installed" => "新版本已经装好，重启即可生效",
        "Finish or cancel the transfers first" => "请先结束或取消传输任务",
        "Software update" => "软件更新",
        "Current version" => "当前版本",
        "Newest version" => "最新版本",
        "Checking GitHub..." => "正在检查 GitHub…",
        "not checked yet" => "尚未检查",
        "This build is already the newest one." => "当前已经是最新版本。",
        "No package for this platform was published." => "这个版本没有发布适合当前平台的安装包。",
        "Published {0}" => "发布于 {0}",
        "Install location: {0}" => "安装目录：{0}",
        "{0} left" => "剩余 {0}",
        "What is new" => "更新内容",
        "No release notes were published." => "这个版本没有填写发布说明。",
        "Checksum verified" => "SHA-256 校验通过",
        "This package has no published checksum." => "这个安装包没有公开的 SHA-256 校验值。",
        "Checking the checksum" => "正在校验 SHA-256",
        "Installing the update" => "正在安装更新",
        "ImGoSSH has to start again before the new version runs." =>
            "需要重新启动 ImGoSSH，新版本才会生效。",
        "{0} file(s) replaced" => "已替换 {0} 个文件",
        "Could not replace: {0}" => "未能替换：{0}",
        "Update now" => "立即更新",
        "Check now" => "立即检查",
        "Restart now" => "立即重启",
        "Open release page" => "打开发布页",
        "Skip this version" => "跳过这个版本",
        "Stop announcing it until a newer one appears" => "在更新的版本发布之前不再提示",
        "Later" => "稍后",
        "Retry" => "重试",
        "Working..." => "处理中…",
        "Could not reach GitHub" => "无法连接 GitHub",
        "GitHub answered" => "GitHub 返回错误",
        "GitHub is answering too many requests right now" =>
            "GitHub 暂时限制了请求次数，请稍后再试",
        "No published release was found" => "没有找到已发布的版本",
        "The release list could not be read" => "无法解析版本信息",
        "The update could not start" => "更新任务无法启动",
        "The download failed" => "下载失败",
        "The download stopped" => "下载中断",
        "The download stopped halfway through" => "下载没有完成",
        "The download did not match the published checksum" =>
            "下载内容与发布的 SHA-256 校验值不一致",
        "This package cannot be unpacked" => "无法解开这个安装包",
        "The package could not be unpacked" => "安装包内容已损坏",
        "The package does not hold a build of this program" => "安装包里没有当前程序的新版本",
        "The running program could not be replaced" => "正在运行的 ImGoSSH 没能被替换",
        "Could not install" => "安装失败",

        // --- about / welcome ---------------------------------------------
        "SSH / SFTP / SCP workstation written in Rust." =>
            "使用 Rust 编写的 SSH / SFTP / SCP 工作站。",
        "Terminal, file browser and tunnels in one native app." =>
            "终端、文件浏览器与隧道，集成在一个原生应用里。",
        "Terminal: vt100 + ConPTY" => "终端：vt100 + ConPTY",
        "Transport: russh (SSH2) + russh-sftp" => "传输层：russh (SSH2) + russh-sftp",
        "UI: egui / eframe" => "界面：egui / eframe",
        "Settings: {0}" => "设置目录：{0}",
        "ImGoSSH {0}" => "ImGoSSH {0}",
        "{0} (copy)" => "{0} 副本",

        "New local terminal\tCtrl+Shift+T" => "新建本地终端\tCtrl+Shift+T",
        "New SSH session...\tCtrl+Shift+N" => "新建 SSH 会话...\tCtrl+Shift+N",
        "Close tab\tCtrl+Shift+W" => "关闭标签页\tCtrl+Shift+W",
        "Scroll to bottom" => "滚动到底部",
        "Tunnel" => "隧道",
        "Advanced SSH settings" => "高级 SSH 设置",
        "Host key" => "主机密钥",
        "Keep it in the session list" => "保存到会话列表",
        "Open the tunnels when the session starts" => "会话启动时打开这些隧道",
        "Use ssh-agent / Pageant" => "使用 ssh-agent / Pageant",
        "Open the file browser with this session" => "随此会话打开文件浏览器",
        "Blinking cursor" => "光标闪烁",
        "Copy on select" => "选中即复制",
        "Open the SFTP browser with new sessions" => "新会话自动打开 SFTP 浏览器",
        "Show live server stats in the status bar" => "在状态栏显示服务器实时状态",
        "Ask before closing a live tab" => "关闭活动标签页前询问",
        "Save passwords in sessions.toml" => "将密码保存到 sessions.toml",
        "Rename folder" => "重命名文件夹",
        "Show the sidebar" => "显示侧边栏",
        "Show the toolbar" => "显示工具栏",
        "Show the status bar" => "显示状态栏",
        "\u{1f4e4} Transfers" => "\u{1f4e4} \u{4f20}\u{8f93}",
        "\u{2699} Settings" => "\u{2699} \u{8bbe}\u{7f6e}",
        "\u{25b6} Local" => "\u{25b6} \u{672c}\u{5730}",
        "\u{25b6} New local terminal" => "\u{25b6} \u{65b0}\u{5efa}\u{672c}\u{5730}\u{7ec8}\u{7aef}",
        "\u{1f5a5} New SSH session" => "\u{1f5a5} \u{65b0}\u{5efa} SSH \u{4f1a}\u{8bdd}",
        "{0} saved session(s) - double-click one in the sidebar to connect." =>
            "{0} \u{4e2a}\u{5df2}\u{4fdd}\u{5b58}\u{4f1a}\u{8bdd}\u{2014}\u{2014}\u{53cc}\u{51fb}\u{5de6}\u{4fa7}\u{4f1a}\u{8bdd}\u{5373}\u{53ef}\u{8fde}\u{63a5}\u{3002}",
        "{0} free of {1} ({2}% used)" =>
            "{1} \u{4e2d}\u{5269}\u{4f59} {0}\u{ff08}\u{5df2}\u{7528} {2}%\u{ff09}",

        // --- the redesigned shell ---------------------------------------
        "A local shell tab, ready in a blink." => "\u{672c}\u{5730}\u{7ec8}\u{7aef}\u{6807}\u{7b7e}\u{9875}\u{ff0c}\u{70b9}\u{4e00}\u{4e0b}\u{5c31}\u{597d}\u{3002}",
        "Password, key or agent - the SFTP pane follows." => "\u{5bc6}\u{7801}\u{3001}\u{5bc6}\u{94a5}\u{6216}\u{4ee3}\u{7406}\u{ff0c}\u{53ef}\u{8fde}\u{5e26} SFTP \u{9762}\u{677f}\u{3002}",
        "Theme, colours, fonts and session defaults." => "\u{4e3b}\u{9898}\u{3001}\u{914d}\u{8272}\u{3001}\u{5b57}\u{4f53}\u{4e0e}\u{4f1a}\u{8bdd}\u{9ed8}\u{8ba4}\u{503c}\u{3002}",
        "Ctrl+Shift+T new terminal - Ctrl+Shift+N new session - Ctrl+Shift+B file browser" =>
            "Ctrl+Shift+T \u{65b0}\u{5efa}\u{7ec8}\u{7aef} \u{b7} Ctrl+Shift+N \u{65b0}\u{5efa}\u{4f1a}\u{8bdd} \u{b7} Ctrl+Shift+B \u{6587}\u{4ef6}\u{6d4f}\u{89c8}\u{5668}",
        "Write the session to the log" => "\u{5c06}\u{4f1a}\u{8bdd}\u{5199}\u{5165}\u{65e5}\u{5fd7}",
        "Terminal colour scheme" => "\u{7ec8}\u{7aef}\u{914d}\u{8272}\u{65b9}\u{6848}",
        "New {0} session" => "\u{65b0}\u{5efa} {0} \u{4f1a}\u{8bdd}",
        "\u{2315} Search sessions" => "\u{2315} \u{641c}\u{7d22}\u{4f1a}\u{8bdd}",
        "\u{2315} Filter by name" => "\u{2315} \u{6309}\u{540d}\u{79f0}\u{8fc7}\u{6ee4}",

        // --- a delete the user has to confirm, and who wrote this ---------
        "Delete this item?" =>
            "\u{786e}\u{5b9a}\u{5220}\u{9664}\u{8be5}\u{6761}\u{76ee}\u{ff1f}",
        "The file will be deleted. This cannot be undone." =>
            "\u{8be5}\u{6587}\u{4ef6}\u{5c06}\u{88ab}\u{5220}\u{9664}\u{ff0c}\u{6b64}\u{64cd}\u{4f5c}\u{65e0}\u{6cd5}\u{64a4}\u{9500}\u{3002}",
        "The folder and everything inside it will be deleted. This cannot be undone." =>
            "\u{8be5}\u{6587}\u{4ef6}\u{5939}\u{53ca}\u{5176}\u{4e2d}\u{7684}\u{5168}\u{90e8}\u{5185}\u{5bb9}\u{90fd}\u{4f1a}\u{88ab}\u{5220}\u{9664}\u{ff0c}\u{6b64}\u{64cd}\u{4f5c}\u{65e0}\u{6cd5}\u{64a4}\u{9500}\u{3002}",
        "Author: {0}" => "\u{4f5c}\u{8005}\u{ff1a}{0}",
        "License: Apache License 2.0" =>
            "\u{5f00}\u{6e90}\u{534f}\u{8bae}\u{ff1a}Apache License 2.0",
        "Free to use, change and redistribute: see the LICENSE file for the full terms." =>
            "\u{53ef}\u{81ea}\u{7531}\u{4f7f}\u{7528}\u{3001}\u{4fee}\u{6539}\u{4e0e}\u{518d}\u{5206}\u{53d1}\u{ff0c}\u{5b8c}\u{6574}\u{6761}\u{6b3e}\u{89c1} LICENSE \u{6587}\u{4ef6}\u{3002}",
        "GitHub repository" => "GitHub \u{4ed3}\u{5e93}",
        _ => key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests flip a process-wide switch, so they run as one unit.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn templates_substitute_by_index() {
        let _guard = LOCK.lock().unwrap();
        set(Lang::En);
        assert_eq!(trf("{0} transfer(s)", &[&3]), "3 transfer(s)");
        assert_eq!(
            trf("{0} free of {1} ({2}% used)", &[&"1 G", &"2 G", &"50"]),
            "1 G free of 2 G (50% used)"
        );
        // A stray brace is copied through instead of panicking.
        assert_eq!(trf("literal { } brace", &[]), "literal { } brace");
        assert_eq!(trf("missing {9} arg", &[&"x"]), "missing  arg");
    }

    #[test]
    fn chinese_only_applies_when_selected() {
        let _guard = LOCK.lock().unwrap();
        set(Lang::En);
        assert_eq!(tr("Cancel"), "Cancel");
        set(Lang::Zh);
        assert_eq!(tr("Cancel"), "\u{53d6}\u{6d88}");
        assert_eq!(
            trf("{0} transfer(s)", &[&2]),
            "2 \u{4e2a}\u{4f20}\u{8f93}\u{4efb}\u{52a1}"
        );
        // Unknown strings fall back to English rather than disappearing.
        assert_eq!(tr("Not in the table"), "Not in the table");
        set(Lang::En);
    }

    #[test]
    fn every_translation_is_reachable() {
        let _guard = LOCK.lock().unwrap();
        set(Lang::Zh);
        // A key that maps to itself is a forgotten translation, unless the
        // string is a product name, a protocol or pure punctuation.
        let neutral = [
            "OK", "Go", "SCP", "SFTP", "SSH", "TERM", "ImGoSSH", "Local", "+",
        ];
        for key in [
            "Cancel",
            "Close",
            "Connect",
            "Password",
            "Username",
            "Port",
            "Host",
            "Sessions",
            "Terminal",
            "Transfers",
            "Settings",
            "Help",
            "Download",
            "Upload",
            "Rename",
            "Delete",
            "Browse",
            "Refresh",
            "Properties",
            "Upload file",
            "Permissions",
            "Created",
            "Fingerprint",
            "Uptime",
            // Strings whose wording changed, so a key drifting away from its
            // entry would silently show English in a Chinese interface.
            "Passwords are stored in clear text in sessions.toml.",
            "Terminal, file browser and tunnels in one native app.",
            // What a session that has ended says about itself.
            "Reopen",
            "The connection was closed",
            "The session ended - exit code {0}",
            "The shell has exited",
            // What the online updater says, in the bar and in its window.
            "Check for updates...",
            "New version {0}",
            "Software update",
            "Update now",
            "Skip this version",
            "Restart to finish",
            "Checked when ImGoSSH starts and again every six hours.",
            "The download did not match the published checksum",
        ] {
            if neutral.contains(&key) {
                continue;
            }
            assert_ne!(tr(key), key, "{key:?} has no Chinese translation");
        }
        set(Lang::En);
    }

    #[test]
    fn language_codes_round_trip() {
        assert_eq!(Lang::parse("zh-CN"), Some(Lang::Zh));
        assert_eq!(Lang::parse(" EN "), Some(Lang::En));
        assert_eq!(Lang::parse("auto"), None);
        assert_eq!(Lang::Zh.code(), "zh");
        assert_eq!(Lang::parse(Lang::Zh.code()), Some(Lang::Zh));
    }
}
