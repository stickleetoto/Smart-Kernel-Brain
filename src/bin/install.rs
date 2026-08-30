use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const PRODUCT_VERSION: &str = "v1";
pub const CORE_VERSION: &str = "v1";

#[derive(Debug, Clone)]
struct InstallOptions {
    add_path: bool,
    quiet: bool,
    scan_root: Option<PathBuf>,
    start_daemon: bool,
}

pub fn handle_no_args() -> io::Result<bool> {
    #[cfg(windows)]
    {
        if !is_running_from_install_path()? {
            let opts = parse_install_options(&[])?;
            match install_windows(&opts, false) {
                Ok(()) => {
                    pause_console(true);
                    return Ok(true);
                }
                Err(e) => {
                    eprintln!("\n[ERROR] {e}");
                    pause_console(false);
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

pub fn install(args: &[String], repair: bool, pause_after: bool) -> io::Result<()> {
    #[cfg(not(windows))]
    {
        let _ = (args, repair, pause_after);
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "single-EXE self-install is currently Windows-only",
        ));
    }

    #[cfg(windows)]
    {
        let opts = parse_install_options(args)?;
        let result = install_windows(&opts, repair);
        if pause_after && !opts.quiet {
            pause_console(result.is_ok());
        }
        result
    }
}

pub fn uninstall(args: &[String]) -> io::Result<()> {
    #[cfg(not(windows))]
    {
        let _ = args;
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "single-EXE self-uninstall is currently Windows-only",
        ));
    }

    #[cfg(windows)]
    {
        let mut purge_data = false;
        for arg in args {
            match arg.as_str() {
                "--purge-data" => purge_data = true,
                "--quiet" | "-q" => {},
                other => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unknown uninstall option: {other}"),
                    ))
                }
            }
        }
        uninstall_windows(purge_data)
    }
}

pub fn status() -> io::Result<()> {
    #[cfg(not(windows))]
    {
        println!("SKB self-install status is currently Windows-only.");
        return Ok(());
    }

    #[cfg(windows)]
    {
        let install_dir = install_dir()?;
        let exe = install_exe()?;
        let data = data_dir()?;
        println!("Smart Kernel Brain installation");
        println!("product       : {PRODUCT_VERSION}");
        println!("core          : {CORE_VERSION}");
        println!("install dir   : {}", install_dir.display());
        println!("installed     : {}", exe.is_file());
        println!("single binary : {}", exe.display());
        println!("PATH contains : {}", user_path_contains(&install_dir)?);
        println!("index.skb     : {}", data.join("index.skb").is_file());
        println!("state.json    : {}", data.join("state.json").is_file());
        if exe.is_file() {
            let _ = Command::new(&exe)
                .arg("daemon-status")
                .stdin(Stdio::null())
                .status();
        }
        Ok(())
    }
}

pub fn print_mcp_config() -> io::Result<()> {
    #[cfg(not(windows))]
    let exe = env::current_exe()?;
    #[cfg(windows)]
    let exe = install_exe()?;

    println!("{}", mcp_json(&exe));
    Ok(())
}

#[cfg(windows)]
fn parse_install_options(args: &[String]) -> io::Result<InstallOptions> {
    let mut add_path = true;
    let mut quiet = false;
    let mut scan_root = None;
    let mut start_daemon = false;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--no-path" => add_path = false,
            "--quiet" | "-q" => quiet = true,
            "--start-daemon" => start_daemon = true,
            "--scan" => {
                i += 1;
                let root = args.get(i).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--scan requires a root path")
                })?;
                scan_root = Some(PathBuf::from(root.as_str()));
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown install option: {other}"),
                ))
            }
        }
        i += 1;
    }
    Ok(InstallOptions {
        add_path,
        quiet,
        scan_root,
        start_daemon,
    })
}

#[cfg(windows)]
fn install_windows(opts: &InstallOptions, repair: bool) -> io::Result<()> {
    let current = env::current_exe()?;
    let install_dir = install_dir()?;
    let target = install_exe()?;
    let data = data_dir()?;
    fs::create_dir_all(&install_dir)?;
    fs::create_dir_all(&data)?;

    println!("Smart Kernel Brain {PRODUCT_VERSION}");
    println!("Core          : {CORE_VERSION} (frozen search core)");
    println!("Mode          : {}", if repair { "repair" } else { "install" });
    println!("Install dir   : {}", install_dir.display());
    println!("Data dir      : {}", data.display());
    println!("Admin         : not required (per-user install)");

    if !same_path(&current, &target) {
        if target.is_file() {
            stop_existing_daemon(&target);
        }
        replace_installed_binary(&current, &target)?;
        println!("Program       : single SKB.exe installed");
    } else {
        println!("Program       : already running from installed SKB.exe");
    }

    if opts.add_path {
        add_user_path(&install_dir)?;
        println!("CLI PATH      : registered for current user");
    } else {
        println!("CLI PATH      : skipped (--no-path)");
    }

    if let Some(root) = &opts.scan_root {
        run_installed(&target, &[OsString::from("scan"), root.as_os_str().to_owned()])?;
    }
    if opts.start_daemon {
        if !data.join("index.skb").is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "--start-daemon requested but no index exists; use --scan <root> first",
            ));
        }
        run_installed(&target, &[OsString::from("daemon-start")])?;
    }

    println!("\n[OK] SKB is ready.");
    println!("CLI           : skb --version");
    println!("Index         : skb scan <root>");
    println!("Resident      : skb daemon-start");
    println!("MCP           : skb mcp");
    println!("MCP config    : skb mcp-config");
    println!("Note          : open a new terminal before relying on the updated PATH.");
    Ok(())
}

#[cfg(windows)]
fn replace_installed_binary(source: &Path, target: &Path) -> io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid install path"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let staging = parent.join(format!("SKB.staging-{}-{stamp}.exe", std::process::id()));
    let backup = parent.join(format!("SKB.backup-{}-{stamp}.exe", std::process::id()));

    fs::copy(source, &staging)?;
    verify_binary(&staging)?;
    run_version_check(&staging)?;

    let had_target = target.exists();
    if had_target {
        fs::rename(target, &backup).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "cannot replace existing SKB.exe; close active SKB/MCP clients and retry: {e}"
                ),
            )
        })?;
    }

    if let Err(e) = fs::rename(&staging, target) {
        if had_target && backup.exists() && !target.exists() {
            let _ = fs::rename(&backup, target);
        }
        let _ = fs::remove_file(&staging);
        return Err(io::Error::new(
            e.kind(),
            format!("failed to activate installed SKB.exe: {e}"),
        ));
    }
    if backup.exists() {
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

#[cfg(windows)]
fn uninstall_windows(purge_data: bool) -> io::Result<()> {
    let current = env::current_exe()?;
    let install_dir = install_dir()?;
    let target = install_exe()?;
    let data = data_dir()?;

    println!("Smart Kernel Brain {PRODUCT_VERSION}");
    println!("Mode          : uninstall");
    println!("Install dir   : {}", install_dir.display());

    if target.is_file() {
        stop_existing_daemon(&target);
    }
    remove_user_path(&install_dir)?;

    if purge_data && data.exists() {
        fs::remove_dir_all(&data)?;
        println!("SKB data      : removed (--purge-data)");
    } else {
        println!("SKB data      : preserved at {}", data.display());
    }

    if same_path(&current, &target) {
        schedule_self_delete(&install_dir, &target)?;
        println!("Program files : scheduled for removal after exit");
    } else if install_dir.exists() {
        fs::remove_dir_all(&install_dir).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("failed to remove installed SKB; close active MCP clients and retry: {e}"),
            )
        })?;
        println!("Program files : removed");
    } else {
        println!("Program files : already absent");
    }

    println!("[OK] Uninstall complete.");
    Ok(())
}

#[cfg(windows)]
fn schedule_self_delete(install_dir: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let script = r#"Start-Sleep -Milliseconds 800; $exe=$env:SKB_REMOVE_EXE; $dir=$env:SKB_REMOVE_DIR; Remove-Item -LiteralPath $exe -Force -ErrorAction SilentlyContinue; Remove-Item -LiteralPath $dir -Force -ErrorAction SilentlyContinue"#;
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .env("SKB_REMOVE_EXE", target)
        .env("SKB_REMOVE_DIR", install_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    Ok(())
}

#[cfg(windows)]
fn verify_binary(path: &Path) -> io::Result<()> {
    let meta = fs::metadata(path)?;
    if meta.len() < 16 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "staged SKB.exe is unexpectedly small",
        ));
    }
    let head = fs::read(path)?;
    if head.get(0..2) != Some(b"MZ") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "staged SKB.exe does not look like a Windows PE executable",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn run_version_check(path: &Path) -> io::Result<()> {
    let output = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("staged SKB.exe --version failed with {}", output.status),
        ))
    }
}

#[cfg(windows)]
fn stop_existing_daemon(exe: &Path) {
    if !exe.is_file() {
        return;
    }
    let _ = Command::new(exe)
        .arg("daemon-stop")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    thread::sleep(Duration::from_millis(150));
}

#[cfg(windows)]
fn run_installed(exe: &Path, args: &[OsString]) -> io::Result<()> {
    let status = Command::new(exe).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("installed SKB command failed with {status}"),
        ))
    }
}

fn mcp_json(exe: &Path) -> String {
    let mut out = String::new();
    out.push_str("{\n  \"mcpServers\": {\n    \"skb\": {\n      \"command\": \"");
    out.push_str(&json_escape(&exe.to_string_lossy()));
    out.push_str("\",\n      \"args\": [\"mcp\"]\n    }\n  }\n}\n");
    out
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(windows)]
fn local_app_data() -> io::Result<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData\\Local")))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is unavailable"))
}

#[cfg(windows)]
fn install_dir() -> io::Result<PathBuf> {
    Ok(local_app_data()?.join("Programs").join("SmartKernelBrain"))
}

#[cfg(windows)]
fn install_exe() -> io::Result<PathBuf> {
    Ok(install_dir()?.join("SKB.exe"))
}

#[cfg(windows)]
fn data_dir() -> io::Result<PathBuf> {
    Ok(local_app_data()?.join("SKB"))
}

#[cfg(windows)]
fn is_running_from_install_path() -> io::Result<bool> {
    Ok(same_path(&env::current_exe()?, &install_exe()?))
}

#[cfg(windows)]
fn same_path(a: &Path, b: &Path) -> bool {
    let left = fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let right = fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    left.to_string_lossy().eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(windows)]
fn powershell_path_script(add: bool) -> &'static str {
    if add {
        r#"$d=$env:SKB_INSTALL_DIR; $p=[Environment]::GetEnvironmentVariable('Path','User'); if($null -eq $p){$p=''}; $parts=@($p -split ';' | Where-Object { $_ -and $_.Trim() }); if(-not ($parts | Where-Object { $_.TrimEnd('\\') -ieq $d.TrimEnd('\\') })){ $parts += $d }; [Environment]::SetEnvironmentVariable('Path',($parts -join ';'),'User')"#
    } else {
        r#"$d=$env:SKB_INSTALL_DIR; $p=[Environment]::GetEnvironmentVariable('Path','User'); if($null -eq $p){exit 0}; $parts=@($p -split ';' | Where-Object { $_ -and ($_.TrimEnd('\\') -ine $d.TrimEnd('\\')) }); [Environment]::SetEnvironmentVariable('Path',($parts -join ';'),'User')"#
    }
}

#[cfg(windows)]
fn add_user_path(install_dir: &Path) -> io::Result<()> {
    run_path_powershell(install_dir, true)
}

#[cfg(windows)]
fn remove_user_path(install_dir: &Path) -> io::Result<()> {
    run_path_powershell(install_dir, false)
}

#[cfg(windows)]
fn run_path_powershell(install_dir: &Path, add: bool) -> io::Result<()> {
    let status = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            powershell_path_script(add),
        ])
        .env("SKB_INSTALL_DIR", install_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "failed to {} user PATH (PowerShell exit {status})",
                if add { "update" } else { "clean" }
            ),
        ))
    }
}

#[cfg(windows)]
fn user_path_contains(install_dir: &Path) -> io::Result<bool> {
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::Out.Write([Environment]::GetEnvironmentVariable('Path','User'))",
        ])
        .output()?;
    if !output.status.success() {
        return Ok(false);
    }
    let path = String::from_utf8_lossy(&output.stdout);
    let wanted = install_dir.to_string_lossy();
    Ok(path.split(';').filter(|v| !v.trim().is_empty()).any(|v| {
        v.trim()
            .trim_end_matches('\\')
            .eq_ignore_ascii_case(wanted.trim_end_matches('\\'))
    }))
}

#[cfg(windows)]
fn pause_console(success: bool) {
    use std::io::Write;
    let message = if success {
        "Press Enter to close..."
    } else {
        "Press Enter after reviewing the error..."
    };
    print!("\n{message}");
    let _ = io::stdout().flush();
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_config_uses_same_binary_with_mcp_argument() {
        let config = mcp_json(Path::new(r#"C:\Users\example\SKB.exe"#));
        let value: serde_json::Value = serde_json::from_str(&config).unwrap();
        assert_eq!(
            value.pointer("/mcpServers/skb/args/0").and_then(|v| v.as_str()),
            Some("mcp")
        );
        assert!(value
            .pointer("/mcpServers/skb/command")
            .and_then(|v| v.as_str())
            .unwrap()
            .ends_with("SKB.exe"));
    }

    #[test]
    fn json_escape_handles_windows_paths() {
        assert_eq!(json_escape(r#"C:\A\B\SKB.exe"#), r#"C:\\A\\B\\SKB.exe"#);
    }
}
