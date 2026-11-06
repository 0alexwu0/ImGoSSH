//! Automatic desktop-client registration. Installers are embedded so portable
//! ImGoSSH executables work without adjacent PowerShell files.

#[cfg(windows)]
use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct Report {
    pub status: String,
    pub path: String,
    pub detail: String,
}

#[cfg(windows)]
pub fn configure() -> Result<Report> {
    use std::os::windows::process::CommandExt;
    let dir = tempfile::tempdir().context("creating JumpServer integration workspace")?;
    for (name, data) in [
        (
            "Auto-Configure.ps1",
            include_bytes!("../integrations/jumpserver/Auto-Configure.ps1").as_slice(),
        ),
        (
            "Install-JumpServerPlugin.ps1",
            include_bytes!("../integrations/jumpserver/Install-JumpServerPlugin.ps1").as_slice(),
        ),
        (
            "Install-LegacyConnector.ps1",
            include_bytes!("../integrations/jumpserver/Install-LegacyConnector.ps1").as_slice(),
        ),
        (
            "legacy/terminal.json",
            include_bytes!("../integrations/jumpserver/legacy/terminal.json").as_slice(),
        ),
        (
            "legacy/filetransfer.json",
            include_bytes!("../integrations/jumpserver/legacy/filetransfer.json").as_slice(),
        ),
        (
            "plugin-sftp/manifest.json",
            include_bytes!("../integrations/jumpserver/plugin-sftp/manifest.json").as_slice(),
        ),
        (
            "plugin-sftp/connect.json",
            include_bytes!("../integrations/jumpserver/plugin-sftp/connect.json").as_slice(),
        ),
        (
            "plugin/manifest.json",
            include_bytes!("../integrations/jumpserver/plugin/manifest.json").as_slice(),
        ),
        (
            "plugin/connect.json",
            include_bytes!("../integrations/jumpserver/plugin/connect.json").as_slice(),
        ),
        (
            "plugin/icon.png",
            include_bytes!("../integrations/jumpserver/plugin/icon.png").as_slice(),
        ),
    ] {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, data)?;
    }
    let powershell =
        std::path::PathBuf::from(std::env::var_os("SystemRoot").context("SystemRoot is not set")?)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let output = std::process::Command::new(powershell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(dir.path().join("Auto-Configure.ps1"))
        .arg("-ImGoSSHPath")
        .arg(std::env::current_exe()?)
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
        .context("starting JumpServer integration")?;
    // Avoid displaying raw script output, which might contain existing config data.
    serde_json::from_slice(&output.stdout)
        .context("JumpServer integration returned an invalid result")
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn embedded_installer_registers_once_without_adjacent_files() {
        if std::env::var_os("IMGOSSH_AUTO_TEST_CHILD").is_some() {
            assert_eq!(super::configure().unwrap().status, "installed");
            assert_eq!(super::configure().unwrap().status, "present");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("测试 client/resources/bin/config.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            include_bytes!("../integrations/jumpserver/fixtures/client-2.1.4-config.json"),
        )
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "jumpserver::tests::embedded_installer_registers_once_without_adjacent_files",
                "--nocapture",
            ])
            .env("IMGOSSH_AUTO_TEST_CHILD", "1")
            .env("IMGOSSH_JUMPSERVER_CONFIG", &path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(config["windows"]["terminal"][0]["display_name"], "ImGoSSH");
        assert_eq!(
            config["windows"]["filetransfer"][0]["display_name"],
            "ImGoSSH SFTP"
        );
        assert!(path
            .parent()
            .unwrap()
            .join("imgossh-legacy-restore.json")
            .is_file());
    }
}

#[cfg(not(windows))]
pub fn configure() -> Result<Report> {
    Ok(Report {
        status: "not_found".into(),
        path: String::new(),
        detail: String::new(),
    })
}
