本版本新增 Dropbox 配置同步，并统一 SSH 终端与 SFTP 的右键菜单样式。

## Dropbox 配置同步

- 在「设置 → 配置...」中找到「会话文件加密」下方的「配置同步」。点击「管理配置同步 → 连接 Dropbox」，在浏览器授权后，将一次性授权码粘贴回 ImGoSSH。无需安装 Dropbox 客户端或自建服务。
- 在源电脑设置独立的同步密码并发布快照；在另一台电脑连接同一个 Dropbox 账号，输入相同的同步密码，拉取、预览后再应用。此版本不会自动上传或拉取。
- 快照在本机加密，包含可移植偏好与 SSH、SFTP、SCP 的基本会话信息。连接密码、私钥、已信任的主机密钥和本地权限不会同步。应用时不会删除本机会话；云端有更新时，需要先拉取并应用才能再次发布。
- 授权过期时，可在设置中为同一账号重新授权，不会清除本机同步进度。若 Dropbox 页面提示 `No scope requested can be granted for this app`，需要由应用维护者在 Dropbox 后台确认已保存 `files.content.read` 和 `files.content.write` 权限。

## 下载与升级

提供 Windows x86-64、Linux x86-64、macOS Apple Silicon 和 macOS Intel 压缩包，以及 `SHA256SUMS.txt`。解压替换程序即可，已有配置与会话继续使用。

使用说明：[README](https://github.com/0alexwu0/ImGoSSH/blob/v0.3.5/README.md#跨电脑配置同步)。
