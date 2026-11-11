//! Chrome native messaging host and the agent's own installer entry point.
//!
//! With no arguments this is a stdio host: Chrome writes length-prefixed JSON,
//! and every request is either answered locally or forwarded to the desktop
//! process over the file mailbox. With arguments it is a plain CLI, which is
//! how registration, login items, upgrades and reverts are performed without
//! a scripting runtime on the target machine.
mod bridge_provider;

use anyhow::Context;
use cloudfile_core::{
    apps, autostart, device, guard, hostreg, ipc, native, paired_session, profile, selftest,
    session, storage, update, wire,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 桌面进程的文件名：安装时与 native host 同级，自检与更新都按它找。
const DESKTOP_NAME: &str = if cfg!(windows) {
    "cloudfile-desktop.exe"
} else {
    "cloudfile-desktop"
};

const USAGE: &str = "\
CloudFile Agent

Usage: cloudfile-native-host [command]

  --version                      print the build version
  --component                    print the local component contract as JSON
  --bridge-provider              run optional read-only CloudFile MCP stdio Provider
  --status                       print agent, registration and exit state as JSON
  --sync-app-profile <url>       sync V1 app metadata from app origin or explicit public discovery URL
  --list-app-profiles            list optional locally bound application profiles
  --register-host                register the Chrome host for the current user
  --unregister-host              remove the host this agent registered
  --rollback-host                restore the previous host registration
  --host-state                   print the current Chrome host registration
  --enable-autostart             start the agent at login
  --disable-autostart            stop starting the agent at login
  --install                      install or repair this machine from the update channel
  --install-webview2             install the bundled WebView2 runtime for the current user
                                 (silent, no administrator required; fixes a missing task centre)
  --set-update-source <url>      configure the update channel (with --public-key)
  --clear-update-source          remove the configured update channel
  --check-update                 report the available signed release, if any
  --self-test                    check the local installation and exit (JSON, non-zero when broken)
  --update                       download, verify, stage and activate an update
  --update-from <manifest>       run an update from a local signed manifest
  --update-extension             install the newest browser extension and let it reload
  --rollback                     point the host back at the previous version
  --prune                        keep the active version plus one revert point

Install options:
  --source <url>                 primary update origin (probed first)
  --source-fallback <url>        mirror used when the primary is unreachable; the one
                                 that actually works is written back into the config
  --skip-registration            do not register the Chrome host
  --skip-autostart               do not add the login item
  --skip-launch                  do not start the task centre afterwards

Update options:
  --public-key <base64url>       pinned P-256 release key (required by https channels)
  --extension-dir <path>         local unpacked extension copy, for --update-extension
  --allow-insecure-source        legacy switch; plain-http channels are allowed by default
                                 (kept so existing install scripts keep working)

Registration options:
  --extension-id <id>            Chrome extension ID to allow (defaults to the product ID;
                                 only pass this for a different build of the extension)
  --host-name <name>             host name (defaults to this platform's host)
  --takeover                     back up and replace a host another program owns
";

fn flag(args: &[String], name: &str) -> Option<String> {
    let index = args.iter().position(|value| value == name)?;
    args.get(index + 1).cloned()
}

fn root() -> anyhow::Result<PathBuf> {
    storage::root()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Chrome supplies the calling extension origin (and on Windows may also
    // supply a parent window). These are host launch metadata, not CLI commands.
    // Chrome's manifest allowlist remains responsible for caller authorization.
    let browser_launch = args.first().is_some_and(|origin| {
        origin
            .strip_prefix("chrome-extension://")
            .and_then(|value| value.strip_suffix('/'))
            .is_some_and(|id| hostreg::validate_extension_id(id).is_ok())
            && args.iter().skip(1).all(|argument| {
                argument
                    .strip_prefix("--parent-window=")
                    .is_some_and(|value| {
                        !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
                    })
            })
    });
    if args.as_slice() == ["--bridge-provider"] {
        if let Err(error) = bridge_provider::serve() {
            eprintln!("CloudFile Bridge Provider failed: {error:#}");
            std::process::exit(1);
        }
        return;
    }
    if args.is_empty() || browser_launch {
        if run_stdio().is_err() {
            eprintln!("CloudFile native messaging failed");
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = run_command(&args) {
        // 修改逻辑（2026-10-28）：原来只打印最外层一条 context，现场看到的
        // 「无法把暂存目录改成正式目录：…」没有后面那句 os error，等于没法定位。
        // `{error:#}` 会把整条 cause 链都打出来。
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run_command(args: &[String]) -> anyhow::Result<()> {
    let root = root()?;
    // 默认就是本平台的 Host（Windows 上是 `com.cloudfile.current_agent`，见 hostreg::HOST）。
    // `--host-name` 只用于显式覆盖（例如按开发身份另注册一份、或清理旧的 dev host）。
    let host = flag(args, "--host-name").unwrap_or_else(|| hostreg::HOST.to_string());
    let command = args[0].as_str();
    match command {
        "--open-uri" => {
            if args.len() != 2 {
                anyhow::bail!("--open-uri requires exactly one URI");
            }
            let uri = &args[1];
            if uri.len() > 16384
                || !(uri.starts_with("seafile://") || uri.starts_with("cloudfile-open://"))
            {
                anyhow::bail!("unsupported OS protocol URL");
            }
            let result = ipc::call(&root, wire::Request::OpenUri { uri: uri.clone() })?;
            if result["ok"] != true {
                anyhow::bail!(
                    "{}",
                    result["error"].as_str().unwrap_or("desktop rejected URL")
                );
            }
            println!("{}", serde_json::to_string(&result)?);
        }
        "--register-protocols" => {
            let binary = host_binary()?;
            cloudfile_core::protocol_registration::install(&binary)?;
            println!("registered per-user URL handler candidates; select defaults in OS settings");
        }
        "--unregister-protocols" => {
            cloudfile_core::protocol_registration::uninstall()?;
            println!("removed Agent-owned URL handler candidates");
        }
        "--protocol-state" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&cloudfile_core::protocol_registration::state()?)?
            );
        }
        "--version" => println!("{}", cloudfile_core::VERSION),
        "--component" => {
            let value = json!({
                "version": cloudfile_core::VERSION,
                "ipc_version": update::IPC_VERSION,
                "journal_version": update::JOURNAL_VERSION,
                "agent_family": "agent2",
                "platform": update::platform_id(std::env::consts::OS, std::env::consts::ARCH),
            });
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        "--sync-app-profile" => {
            if args.len() != 2 {
                anyhow::bail!("--sync-app-profile requires one canonical origin or discovery URL");
            }
            let value = profile::sync(&root, &args[1])?;
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        "--list-app-profiles" => {
            if args.len() != 1 {
                anyhow::bail!("--list-app-profiles does not accept arguments");
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&profile::summaries(&root))?
            );
        }
        "--status" => {
            let binary = host_binary()?;
            let dir = hostreg::manifest_dir()?;
            let registration = hostreg::state(&dir, &host, Some(&binary))?;
            // 修改逻辑（2026-11-02）：运行时探测只做一次，`webview2` 与 `task_center`
            // 必须出自同一个事实 —— 否则现场会看到「有版本号却说任务中心不可用」
            // 这种自相矛盾的报告，反而更难判断。
            // 注意：这里读的是注册表（`apps::webview2_version`），是**尽力探测**；
            // 桌面进程用的是 Tauri 的同一入口（wry 的 GetAvailableCoreWebView2BrowserVersionString）。
            // fixed-version 部署、运行时损坏或被策略拦下时两者可能不一致，所以
            // `task_center` 只作参考，最终以桌面进程实际能不能建窗为准。
            let webview2 = apps::webview2_version();
            let task_center =
                cloudfile_core::webview2::task_center_ready(cfg!(windows), webview2.as_deref());
            // 随包安装器是否可用：缺运行时时现场/IT 据此决定「一键装到当前用户」还是「走下载页」。
            // 校验失败也会报不可用（宁可不给按钮，也不执行来源不明的安装器）。
            let current = std::env::current_exe()?;
            let exe_dir = current
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            let installer = cloudfile_core::webview2_setup::locate(
                &exe_dir,
                Some(&update::install_root(&binary)),
            );
            let value = json!({
                "version": cloudfile_core::VERSION,
                "install_root": update::install_root(&binary).to_string_lossy(),
                "host": host,
                "registered": registration.binary.is_some(),
                "foreign": registration.foreign,
                "manifest_path": registration.manifest_path.to_string_lossy(),
                "binary": registration.binary.as_ref().map(|p| p.to_string_lossy().to_string()),
                "extension_id": registration.extension_id,
                "active_version": update::active_version(&dir, &host)?,
                "installed_versions": update::installed_versions(&update::install_root(&binary))?,
                "autostart": autostart::enabled(),
                "autostart_mechanism": autostart::mechanism().map(|item| item.id()),
                "applications": apps::names(),
                "application_profiles": profile::summaries(&root),
                "runtime": {
                    // The desktop shell needs WebView2 on Windows; on macOS the
                    // system WebKit is always present, so `null` there means the
                    // check does not apply, not that it failed.
                    "webview2": webview2,
                    // 扩展与现场支持据此回答「为什么没有任务中心」。
                    "task_center": task_center,
                    // 自助安装能力：`available=true` 表示可以直接装到当前用户（免管理员）。
                    "installer": {
                        "available": installer.is_ok(),
                        "path": installer
                            .as_ref()
                            .ok()
                            .map(|path| path.to_string_lossy().into_owned()),
                        "error": installer.err().map(|error| error.to_string()),
                    },
                },
                "exit": guard::assess(&root),
            });
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        "--register-host" => {
            // 扩展 ID 是产品常量：正常路径不需要传，传了也只是同一组字符
            // （`hostreg::install` 仍会校验 32 位 a-p 的形状）。
            let extension_id =
                flag(args, "--extension-id").unwrap_or_else(|| hostreg::EXTENSION_ID.to_string());
            let takeover = args.iter().any(|value| value == "--takeover");
            let binary = host_binary()?;
            let dir = hostreg::manifest_dir()?;
            // A host owned by another program is never replaced on the strength
            // of a failed attempt alone: the caller asks for that explicitly so
            // a script cannot escalate by accident.
            let outcome = hostreg::install(&dir, &host, &extension_id, &binary, takeover).map_err(
                |error| {
                    anyhow::anyhow!(
                        "{error}\n\n如果确认这属于本机 Agent，请加 --takeover 重新注册；原注册会先备份并可用 --rollback-host 还原"
                    )
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
            // 注册本身不依赖 WebView2（native host 与扩展照常工作），但任务中心窗口会打不开。
            // 修改逻辑（2026-11-02）：以前只说「可能无法启动」。现在 Agent 缺运行时
            // 会降级成后台运行，并支持随包安装器自助安装（免管理员），所以这里把
            // 三条出路一起说清：桌面引导 / 命令行静默 / 手动下载。
            if cfg!(windows) && apps::webview2_version().is_none() {
                let current = std::env::current_exe()?;
                let installer_available = cloudfile_core::webview2_setup::available(
                    current.parent().unwrap_or(Path::new(".")),
                    Some(&update::install_root(&current)),
                );
                eprintln!(
                    "警告：未检测到 WebView2 运行时，任务中心窗口无法打开（Agent 仍会在后台运行）。"
                );
                if installer_available {
                    eprintln!(
                        "  免管理员安装：双击任务中心按提示安装，或执行 \
                         cloudfile-native-host --install-webview2"
                    );
                }
                eprintln!(
                    "  手动安装：Microsoft Edge WebView2 Runtime「常青版独立安装程序」{}",
                    cloudfile_core::webview2::RUNTIME_DOWNLOAD_URL
                );
            }
        }
        "--unregister-host" => {
            let dir = hostreg::manifest_dir()?;
            let removed = hostreg::unregister(&dir, &host)?;
            println!("{}", removed.to_string_lossy());
        }
        "--rollback-host" => {
            let dir = hostreg::manifest_dir()?;
            println!("{}", hostreg::rollback(&dir, &host)?.to_string_lossy());
        }
        "--host-state" => {
            let dir = hostreg::manifest_dir()?;
            let binary = host_binary()?;
            let registration = hostreg::state(&dir, &host, Some(&binary))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "host": registration.host,
                    "manifest_path": registration.manifest_path.to_string_lossy(),
                    "registered": registration.binary.is_some(),
                    "binary": registration.binary.as_ref().map(|p| p.to_string_lossy().to_string()),
                    "extension_id": registration.extension_id,
                    "foreign": registration.foreign,
                }))?
            );
        }
        "--install" => run_install(&root, &host, args)?,
        // 自助安装 WebView2 运行时（静默、当前用户、免管理员）。
        // 修改逻辑（2026-11-02 现场追问）：安装脚本/IT 需要一条不需要管理员、也不需要
        // 用户会操作命令行之外东西的路径；桌面端在启动/托盘里用的是同一个安装器。
        "--install-webview2" => run_install_webview2()?,
        "--set-update-source" => {
            let source = flag(args, "--set-update-source")
                .ok_or_else(|| anyhow::anyhow!("--set-update-source requires a url"))?;
            let public_key = flag(args, "--public-key");
            let extension_dir = flag(args, "--extension-dir");
            // 明文 http 现在默认就支持（内网门户只有 http），这个开关只是为了兼容
            // 旧脚本，取值会被记进 config.json 当审计位，但不再影响源是否被接受。
            let policy = if args.iter().any(|value| value == "--allow-insecure-source") {
                update::SourcePolicy::DEV
            } else {
                update::SourcePolicy::STRICT
            };
            session::set_update_with_policy(
                &root,
                Some(source),
                public_key,
                extension_dir,
                policy,
            )?;
            let config = session::config(&root)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "update_source": config.update_source,
                    "update_public_key_set": config.update_public_key.is_some(),
                    "allow_insecure_update_source": config.allow_insecure_update_source,
                    "extension_dir": config.extension_dir,
                }))?
            );
        }
        "--clear-update-source" => {
            session::set_update(&root, None, None, None)?;
            println!("update source cleared");
        }
        "--enable-autostart" => {
            let binary = desktop_binary()?;
            // 说清落在哪条途径上：企业机器上 Run 键常被锁，实际生效的是
            // 「启动」文件夹 —— 不报出来就没法解释「为什么任务管理器里看不到」。
            let item = autostart::install(&root, &binary)?;
            println!("{}", binary.to_string_lossy());
            println!("{}", item.id());
        }
        "--disable-autostart" => {
            autostart::remove(&root)?;
            println!("disabled");
        }
        "--check-update" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&native::check_update(&root))?
            );
        }
        "--self-test" => {
            // 「搭档」是桌面进程：Agent 能跑，但用户没有入口。装的时候两个一起装，
            // 所以缺了就该报出来（读不到路径时用文件名占位，报告里会指出找不到）。
            let peer = desktop_binary().unwrap_or_else(|_| PathBuf::from(DESKTOP_NAME));
            let report = selftest::run(&root, &peer);
            println!("{}", serde_json::to_string_pretty(&report)?);
            if report["ok"] != json!(true) {
                // 退出码要能被脚本判：0 = 全绿，1 = 有项确实不工作。
                std::process::exit(1);
            }
        }
        "--update" => run_update(&root, &host, None)?,
        "--update-extension" => {
            native::record_update_result(&root, "extension", "running");
            match native::update_extension(&root) {
                Ok(message) => {
                    native::record_update_result(&root, "extension", "completed");
                    println!("{message}");
                }
                Err(error) => {
                    // 扩展升级同样是被分离进程触发的：原因不落盘，界面上就只剩一句「失败」。
                    native::record_update_failure(&root, "extension", &format!("{error:#}"));
                    return Err(error);
                }
            }
        }
        "--update-from" => {
            let path = flag(args, "--update-from")
                .ok_or_else(|| anyhow::anyhow!("--update-from requires a manifest path"))?;
            run_update(&root, &host, Some(PathBuf::from(path)))?;
        }
        "--rollback" => {
            let dir = hostreg::manifest_dir()?;
            println!("{}", update::rollback(&dir, &host)?.to_string_lossy());
        }
        "--prune" => {
            let binary = host_binary()?;
            let install = update::install_root(&binary);
            let dir = hostreg::manifest_dir()?;
            let removed = update::prune(&install, &host, &dir, 2)?;
            println!("{}", serde_json::to_string_pretty(&removed)?);
        }
        "--help" | "-h" => print!("{USAGE}"),
        other => anyhow::bail!("unknown command {other}\n\n{USAGE}"),
    }
    Ok(())
}

fn host_binary() -> anyhow::Result<PathBuf> {
    let binary = std::env::current_exe()?;
    Ok(binary.canonicalize().unwrap_or(binary))
}

/// The desktop executable is installed next to the native host; the mailbox
/// protocol depends on that, and so does the login item.
fn desktop_binary() -> anyhow::Result<PathBuf> {
    let host = host_binary()?;
    let name = DESKTOP_NAME;
    let directory = host
        .parent()
        .ok_or_else(|| anyhow::anyhow!("native host has no install directory"))?;
    let candidate = directory.join(name);
    if !candidate.is_file() {
        anyhow::bail!("cloudfile-desktop must be installed beside the native host");
    }
    Ok(candidate)
}

/// Verify, stage, health check and activate. Ordering is the safety property:
/// nothing replaces the running build until the staged build has started and
/// answered the local protocol.
fn run_update(root: &Path, host: &str, manifest: Option<PathBuf>) -> anyhow::Result<()> {
    native::record_update_result(root, "agent", "running");
    let result = apply_update(root, host, manifest);
    match &result {
        Ok(()) => native::record_update_result(root, "agent", "completed"),
        // 修改逻辑（2026-10-29）：失败原因必须落进 update-result.json。这个进程是分离启动的，
        // 它的 stderr 可能指向 null（任务中心侧）或已关闭的控制台（Chrome 侧），
        // 结果文件里只有一个 `failed` 等于什么都没说。
        Err(error) => native::record_update_failure(root, "agent", &format!("{error:#}")),
    }
    let install = update::install_root(&std::env::current_exe()?);
    if result.is_ok() {
        let dir = hostreg::manifest_dir()?;
        let _ = update::prune(&install, host, &dir, 2);
    }
    result
}

fn apply_update(root: &Path, host: &str, manifest: Option<PathBuf>) -> anyhow::Result<()> {
    let (release, _, _) =
        fetch_verified_release(root, manifest.as_deref(), cloudfile_core::VERSION)?;
    apply_release(host, &release)
}

/// 取清单并验签，返回 (清单, 策略, 源)。`--install` 复用这一段，所以它不碰安装目录。
///
/// `baseline` 是「拿什么版本和清单比」：升级用 `cloudfile_core::VERSION`（必须更新才装），
/// 全新安装用 `update::NO_INSTALLED_VERSION`（空白机器上清单版本就等于本机这份构建，
/// 否则会被判成「不比当前新」而什么都不装）。
fn fetch_verified_release(
    root: &Path,
    manifest: Option<&Path>,
    baseline: &str,
) -> anyhow::Result<(update::Release, update::SourcePolicy, String)> {
    let config = session::config(root)?;
    let (source, public_key) = match (&config.update_source, &config.update_public_key) {
        (Some(source), Some(key)) => (source.clone(), key.clone()),
        _ => anyhow::bail!(
            "尚未配置签名更新源；请提供 --update-from <manifest> 或先设置更新源与公钥"
        ),
    };
    let policy = config.update_policy();
    // 主源拿不到清单时换备用源；验签基准跟着换成真正提供清单的那个源。
    let (bytes, source) = match manifest {
        Some(path) => (std::fs::read(path)?, source),
        None => update::fetch_manifest_with_fallback(policy, root, &source)?,
    };
    let release = update::parse_release(&bytes)?;
    update::verify(
        policy,
        &release,
        &source,
        baseline,
        cloudfile_core::now(),
        &update::platform_id(std::env::consts::OS, std::env::consts::ARCH),
        &public_key,
    )?;
    Ok((release, policy, source))
}

/// 一键安装 / 修复。等价于 Go 的 `install.cmd`，区别只是目标机器**不需要任何脚本运行时**。
///
/// 步骤与顺序：安装目录可写 → 定源并写回（主源不通退备源）→ 有更新才换二进制 →
/// 注册 Host → 设开机自启 → 启动任务中心。幂等：已经装好的机器重跑一遍等于修复
/// （注册与自启重做、二进制不动）。
fn run_install(root: &Path, host: &str, args: &[String]) -> anyhow::Result<()> {
    // 先判「装得进去」再下载：别下完几 MB 才发现目录不可写（Go 的 :usable 也是这个顺序）。
    let install = update::install_root(&std::env::current_exe()?);
    ensure_install_root(&install)?;
    // 修改逻辑（2026-11-02）：把发行 ZIP 里随包的 WebView2 安装器复制到**安装根**
    // （`<安装根>/webview2/`）。`--install` 只把两个二进制下载到 `bin/<版本>/`，
    // 不复制这一步的话，装完的机器上桌面进程找不到安装器 —— 而缺运行时的机器
    // 往往既没有管理员权限、也打不开微软站点。失败只警告：安装本身不依赖它。
    if let Ok(current) = std::env::current_exe() {
        if let Some(exe_dir) = current.parent() {
            match cloudfile_core::webview2_setup::ensure_in_install_root(exe_dir, &install) {
                Ok(Some(path)) => eprintln!("WebView2 安装器已就位：{}", path.display()),
                Ok(None) => {}
                Err(error) => {
                    eprintln!("警告：未能把随包 WebView2 安装器放到安装根（{error:#}）。")
                }
            }
        }
    }
    // 修改逻辑（2026-10-26）：安装进程继承部署方注入的 CLOUDFILE_TRUSTED_ORIGINS，
    // 只有在这一步才拿得到（任务中心是登录后另起的进程，环境变量不会跟过去）。
    // 只在 config.json 不存在时写 —— 已装机器不会被重置。
    if let Ok(seeded) = session::ensure_default_origins(&install) {
        if !seeded.is_empty() {
            println!("已写入内置受信任服务：{}", seeded.join(", "));
        }
    }

    let (source, policy, probes) = resolve_source(root, args)?;
    let dir = hostreg::manifest_dir()?;

    // 二进制：清单里有更新才暂存。暂存只写版本目录、**不碰注册** —— 「健康检查通过之前
    // 什么都不替换」这条安全顺序仍然成立；而全新安装这边没有旧注册可以接管
    // （`update::activate` 是「接管既有注册」专用的），所以注册必须能和激活分开做。
    let (binaries, staged) = stage_binaries(root, &install, &dir, host)?;

    // 注册 Host 是安装的实质产物：没有它扩展就找不到 Agent。
    let (registration, registered_dir) = if args.iter().any(|value| value == "--skip-registration")
    {
        (
            json!({"skipped": true}),
            host_binary()?.parent().map(Path::to_path_buf),
        )
    } else {
        let (target, extension_id, takeover) =
            registration_target(&dir, host, staged.as_ref(), args)?;
        let outcome = hostreg::install(&dir, host, &extension_id, &target, takeover).map_err(
            |error| {
                anyhow::anyhow!(
                    "{error}\n\n如果确认这属于本机 Agent，请加 --takeover 重新注册；原注册会先备份并可用 --rollback-host 还原"
                )
            },
        )?;
        let directory = target.parent().map(Path::to_path_buf);
        (serde_json::to_value(outcome)?, directory)
    };

    // 自启与启动都用**注册真正指向的那个目录**：全新安装时正在跑的可能只是引导下载的临时
    // 副本，它旁边没有桌面进程。失败都只警告 —— 二进制已经就位，用户能自己双击。
    let desktop = registered_dir
        .as_deref()
        .map(|directory| directory.join(DESKTOP_NAME));
    let login_item = if args.iter().any(|value| value == "--skip-autostart") {
        None
    } else {
        match desktop.as_deref().filter(|path| path.is_file()) {
            Some(desktop) => match autostart::install(root, desktop) {
                Ok(item) => Some(item.id().to_string()),
                Err(error) => {
                    eprintln!("警告：开机自启没设上（{error:#}）；重启后手动双击任务中心即可。");
                    None
                }
            },
            None => {
                eprintln!("警告：注册目录里没有 {DESKTOP_NAME}，开机自启跳过。");
                None
            }
        }
    };

    // 修改逻辑（2026-11-02 现场）：任务中心窗口依赖 WebView2 运行时。缺失时**安装本身
    // 仍然成功**（后台同步、浏览器里打开文件都不依赖它），但必须当场说清 ——
    // 否则装完自动启动的任务中心只会弹一句英文原生错误框，用户不知道装什么。
    // 放在启动之前：先让用户看到结论，再拉起那个没有窗口的进程。
    let webview2 = apps::webview2_version();
    let task_center_ready =
        cloudfile_core::webview2::task_center_ready(cfg!(windows), webview2.as_deref());
    // 随包安装器在不在：决定提示里给「一键安装」还是只能给下载页。
    let current = std::env::current_exe()?;
    let installer_available = cloudfile_core::webview2_setup::available(
        current.parent().unwrap_or(Path::new(".")),
        Some(&update::install_root(&current)),
    );
    if !task_center_ready {
        // 优先给「不需要管理员」的自助路径：桌面端启动时会引导安装随包安装器，
        // 脚本/IT 也可以用 --install-webview2 静默装当前用户。
        eprintln!(
            "警告：本机未检测到 Microsoft Edge WebView2 运行时，任务中心窗口无法显示。\n\
             Agent 仍会在后台运行：已打开的本地副本与独占租约会继续；上传/提交要等任务中心恢复后再操作。\n\
             处理（都不需要管理员权限）："
        );
        if installer_available {
            eprintln!(
                "  1. 双击任务中心，按中文提示「立即安装」（安装包已随 Agent 提供）；\n\
                 2. 或执行 cloudfile-native-host --install-webview2（静默安装到当前用户）。"
            );
        }
        eprintln!(
            "  手动安装：下载「常青版独立安装程序（Evergreen Standalone Installer）」：{}",
            cloudfile_core::webview2::RUNTIME_DOWNLOAD_URL
        );
    }

    let launched = if args.iter().any(|value| value == "--skip-launch") {
        false
    } else {
        launch_desktop(desktop.as_deref())?
    };

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "version": cloudfile_core::VERSION,
            "install_root": install.to_string_lossy(),
            "update_source": source,
            "allow_insecure_update_source": policy.allow_insecure,
            "source_probes": probes.iter().map(|probe| json!({
                "source": probe.source,
                "error": probe.error,
            })).collect::<Vec<_>>(),
            "binaries": binaries,
            "staged_version": staged.as_ref().map(|(version, _)| version.clone()),
            "registered_directory": registered_dir
                .as_deref()
                .map(|dir| dir.to_string_lossy().into_owned()),
            "registration": registration,
            "login_item": login_item,
            "desktop_started": launched,
            // 安装脚本/现场支持直接看这一段就知道「任务中心为什么没出现」。
            "runtime": {
                "webview2": webview2,
                "task_center": task_center_ready,
                // 有随包安装器时，缺运行时可以免管理员自助安装（桌面引导/--install-webview2）。
                "installer": installer_available,
            },
        }))?
    );
    Ok(())
}

/// 用随包安装器把 WebView2 运行时装到**当前用户**（静默、免管理员）。
///
/// 桌面进程在启动/托盘里用的是同一个安装器，只是保留安装器界面让用户看到进度；
/// 这里是给 `install.cmd` 与 IT 脚本用的无交互版本。安装失败返回非零退出码，
/// 便于脚本判断（不要把「安装器跑完了」当成功，成功判据是运行时探测）。
fn run_install_webview2() -> anyhow::Result<()> {
    let current = std::env::current_exe()?;
    let exe_dir = current
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("无法定位安装目录"))?;
    let install_root = update::install_root(&current);
    // 先找随包的；没有就**从更新通道取**（2026-11-03：内网机器没有管理员、也打不开
    // 微软站点，而通道本来是通的 —— 安装器跟着通道走才是能落地的那条路）。
    let installer = match cloudfile_core::webview2_setup::locate(&exe_dir, Some(&install_root)) {
        Ok(path) => path,
        Err(_) => {
            let config_root = root()?;
            let source = session::config(&config_root)?
                .update_source
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "本机没有随包 WebView2 安装器，也没有配置更新通道。\n\
                         请把 {}/ 放进安装根，或先设置更新通道后重试",
                        cloudfile_core::webview2_setup::INSTALLER_DIR
                    )
                })?;
            eprintln!("本地没有随包安装器，正在从更新通道获取：{source}");
            cloudfile_core::webview2_setup::fetch_from(&source, &install_root)?
        }
    };
    let outcome = cloudfile_core::webview2_setup::install(
        &installer,
        true,
        cloudfile_core::webview2_setup::DEFAULT_TIMEOUT,
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "installer": installer.to_string_lossy(),
            "exit_code": outcome.exit_code,
            "installed": outcome.installed,
            "webview2": outcome.version,
        }))?
    );
    if !outcome.installed {
        let code = outcome
            .exit_code
            .map_or_else(|| "无".to_string(), |code| code.to_string());
        anyhow::bail!(
            "WebView2 运行时未安装成功（安装器退出码：{code}）。\n{}",
            cloudfile_core::webview2::runtime_install_failed_guidance(outcome.exit_code)
        );
    }
    Ok(())
}

/// 安装目录能不能真写进去：建目录 + 写一个探针文件再删掉。
fn ensure_install_root(install: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(install)
        .with_context(|| format!("创建安装目录失败：{}", install.display()))?;
    let probe = install.join(".cf-write-test");
    std::fs::write(&probe, b"ok")
        .with_context(|| format!("安装目录不可写：{}", install.display()))?;
    let _ = std::fs::remove_file(&probe);
    // 版本目录都建在 bin\ 下面：这个子目录如果是老版本（管理员安装）留下的，
    // 普通用户可能只能读不能写，装到一半才报「拒绝访问」。提前探测并说清楚怎么办。
    let bin = update::bin_dir(install);
    std::fs::create_dir_all(&bin)
        .with_context(|| format!("创建安装目录失败：{}", bin.display()))?;
    let bin_probe = bin.join(".cf-write-test");
    std::fs::write(&bin_probe, b"ok").with_context(|| {
        format!(
            "安装目录的 bin 子目录不可写：{}\n\
             请把绿色包解压到当前用户有写入权限的目录（如 %LOCALAPPDATA%\\CloudFileAgent），
             然后重新运行安装/引导；无需管理员权限，也不要删除原工作副本。",
            bin.display()
        )
    })?;
    let _ = std::fs::remove_file(&bin_probe);
    Ok(())
}

/// 定源并**把它写回配置**。
///
/// 给了 `--source` 就依次探测（`--source-fallback` 是备用、通常是直连静态服务的 IP），
/// 写回的是这台机器**真的能访问**的那个 —— 否则用户装完、第一次升级时才发现主源
/// 在这台机器上根本不通。两个都不通时，报错里带着每个地址各自的原因。
fn resolve_source(
    root: &Path,
    args: &[String],
) -> anyhow::Result<(String, update::SourcePolicy, Vec<update::SourceProbe>)> {
    let policy = if args.iter().any(|value| value == "--allow-insecure-source") {
        // 兼容位：http 早已默认支持，这里只影响写进 config 的审计字段。
        update::SourcePolicy::DEV
    } else {
        update::SourcePolicy::STRICT
    };
    let mut candidates = Vec::new();
    if let Some(primary) = flag(args, "--source") {
        candidates.push(primary);
    }
    if let Some(fallback) = flag(args, "--source-fallback") {
        candidates.push(fallback);
    }
    if candidates.is_empty() {
        // 没给地址：用这台机器上已经配好的（策略也以配置为准，见 --set-update-source）。
        let config = session::config(root)?;
        // 先算策略：下面的 ok_or_else 会把 update_source 移走。
        let policy = config.update_policy();
        let source = config.update_source.ok_or_else(|| {
            anyhow::anyhow!(
                "--install 需要 --source <url>，或先用 --set-update-source 配置更新通道"
            )
        })?;
        return Ok((source, policy, Vec::new()));
    }

    let (picked, probes) = update::pick_source(policy, &candidates);
    let Some(source) = picked else {
        anyhow::bail!(
            "两个更新源都不可用：{}\n请检查本机能否访问上面的地址（DNS/代理/防火墙）后重试。",
            update::describe_probes(&probes)
        );
    };
    session::set_update_with_policy(
        root,
        Some(source.clone()),
        flag(args, "--public-key"),
        flag(args, "--extension-dir"),
        policy,
    )?;
    // 把两个候选地址都留档：装的时候只有一个能用，另一个等它恢复后还要能顶上来。
    // 独立文件而不是写进 config —— config 是 deny_unknown_fields 的，加键会挡住回退。
    let primary = flag(args, "--source");
    let fallback = flag(args, "--source-fallback");
    if primary.is_some() || fallback.is_some() {
        let _ = cloudfile_core::storage::write(
            &root.join(update::CHANNELS_FILE),
            &serde_json::json!({ "primary": primary, "fallback": fallback }),
        );
    }
    Ok((source, policy, probes))
}

/// 某个**已完整安装**的版本目录里的 host 绝对路径。
/// 注册必须指向它，而不是正在跑的引导体临时副本（那份旁边没有任务中心）。
fn installed_host(install: &Path, version: &str) -> anyhow::Result<PathBuf> {
    let dir = update::version_dir(install, version)?;
    let host = dir.join(update::Binary::NativeHost.installed_name(std::env::consts::OS));
    anyhow::ensure!(
        host.is_file(),
        "已安装版本目录里没有 native host：{}",
        dir.display()
    );
    Ok(host)
}

/// 二进制：这台机器上还没有一份完整的同版本安装才暂存（`--install` 同时是修复命令）。
/// 返回 (结论, 暂存好的 (版本, host 路径))。
///
/// 「要不要装」的判据是**磁盘上那份安装完不完整**，不是「版本号和当前这个构建一样」：
/// 全新安装时正在跑的这份只是引导体刚下载到安装根里的临时副本，它旁边没有任务中心，
/// 拿版本号自比会让空白机器报「已是最新」然后什么都不装，引导体随后还会把那份副本删掉，
/// 留下一份指向空目录的注册（2026-10-19 现场就是这个状态）。
fn stage_binaries(
    root: &Path,
    install: &Path,
    dir: &Path,
    host: &str,
) -> anyhow::Result<(&'static str, Option<(String, PathBuf)>)> {
    let config = session::config(root)?;
    let (Some(source), Some(_)) = (
        config.update_source.clone(),
        config.update_public_key.as_ref(),
    ) else {
        // 只有扩展通道（开发通道常见）：二进制不升级，但安装照样可以完成。
        return Ok(("skipped", None));
    };
    let policy = config.update_policy();
    // 先**不验签**地读一眼版本号：安装要能在「通道上的版本与本地一样」时装（空白机器就是
    // 这种情况），而完整验签会拒绝「不比当前新」的清单。签名通过之前不会下载任何字节。
    let advertised = update::parse_release(&update::fetch_manifest_for(policy, &source)?)?;

    // 这个版本已经完整地装在这台机器上：重跑安装等于修复注册与自启，二进制不动。
    //
    // 修改逻辑（2026-10-23）：原来这里只回 ("installed", None)，于是注册目标退到
    // 「就地指正在跑的这份」——而跑的是引导体下载到安装根的临时副本，旁边没有
    // cloudfile-desktop.exe，安装直接失败：
    // 「安装根里没有 cloudfile-desktop.exe，无法就地注册；请用 --source 重新安装」。
    // 实际现场是「二进制早就装好了、只差注册」，用户却被这句话锁死（再跑一次还是同一句）。
    // 现在把**已装版本目录里的 host** 交给注册步骤，重跑安装即可修好注册与自启。
    if update::is_complete(install, &advertised.version) {
        let host = installed_host(install, &advertised.version)?;
        return Ok(("installed", Some((advertised.version, host))));
    }
    // 已经装着一个**更新的**版本（通道回退）：别用「安装」去降级。
    if let Some(active) = update::active_version(dir, host)? {
        if update::compare(&advertised.version, &active) == std::cmp::Ordering::Less
            && update::is_complete(install, &active)
        {
            // 同上：退回不回滚，但注册缺失时仍要能指到已装的那份。
            let host = installed_host(install, &active)?;
            return Ok(("current", Some((active, host))));
        }
    }
    // 同一份清单再取一次并完整验签。清单只有几十 KB、一台机器只跑一次安装，
    // 不值得为了省这一次请求把「验签」拆到两个地方。
    let (release, _, _) = fetch_verified_release(root, None, update::NO_INSTALLED_VERSION)?;
    let (_, staged_host) = stage_release(&release, install)?;
    Ok(("staged", Some((release.version.clone(), staged_host))))
}

/// 注册该指向哪个二进制、用哪个扩展 ID、要不要接管。顺序就是优先级：
///
/// 1. 这次刚暂存了新版本 → 指它（升级与全新安装都指它）
/// 2. 否则已有注册 → 沿用它（不无缘无故改别人的注册；`--extension-id` 显式给了才换）
/// 3. 都没有 → 就地修复：指当前正在跑的这份
fn registration_target(
    dir: &Path,
    host: &str,
    staged: Option<&(String, PathBuf)>,
    args: &[String],
) -> anyhow::Result<(PathBuf, String, bool)> {
    let existing = hostreg::state(dir, host, None)?;
    let owned_rust = existing.binary.as_ref().is_some_and(|binary| {
        binary
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("cloudfile-native-host"))
    });
    let target = match staged {
        // A staged Rust update wins regardless of the previous controller.
        Some((_, staged_host)) => staged_host.clone(),
        // Repair an existing Rust registration without changing its binary.
        None if owned_rust => existing.binary.clone().unwrap(),
        // A foreign controller is never re-registered as if it were ours.
        None => {
            // 没有暂存也没有自家注册：只能就地指正在跑的这份。它旁边**必须**有任务中心，
            // 否则注册出来的东西 Chrome 拉起来也起不了任务中心（引导体下载到安装根里的
            // 临时副本就是这种情况，它随后还会被删掉）。
            let binary = host_binary()?;
            let desktop = binary
                .parent()
                .map(|dir| dir.join(DESKTOP_NAME))
                .filter(|path| path.is_file());
            anyhow::ensure!(
                desktop.is_some(),
                "安装根里没有 {DESKTOP_NAME}，无法就地注册；请用 --source 重新安装"
            );
            binary
        }
    };
    // Reuse allowed origins only for an existing Rust host. Reusing a foreign
    // vendor's extension ID during an explicit takeover would disconnect the
    // CloudFile extension, even if the manifest was backed up correctly.
    let extension_id = flag(args, "--extension-id")
        .or_else(|| owned_rust.then(|| existing.extension_id.clone()).flatten())
        .unwrap_or_else(|| hostreg::EXTENSION_ID.to_string());
    // A version update may replace our own Rust binary without takeover;
    // replacing a different program always requires explicit --takeover.
    // hostreg::install enforces this even if the manifest changes concurrently.
    let takeover = args.iter().any(|value| value == "--takeover");
    Ok((target, extension_id, takeover))
}

/// 装完立刻启动任务中心：否则用户重启之前右下角什么都没有。
/// 桌面是 GUI 子系统程序，读写的都是自己的私有目录，启动它不会弹控制台。
/// 启动任务中心但**不杀**已在运行的实例：给「打开任务中心」用。
/// 已经在跑的那份会通过 IPC 被切到前台（见 desktop 的 OpenTaskCenter），
/// 这里只在「一个都没跑」时才会真的拉起新进程。
fn spawn_desktop(desktop: Option<&Path>) -> anyhow::Result<bool> {
    let Some(desktop) = desktop.filter(|path| path.is_file()) else {
        anyhow::bail!("找不到 {}，请重新运行安装包里的 install.cmd", DESKTOP_NAME);
    };
    let mut command = std::process::Command::new(desktop);
    // 不带 --autostart：那是登录项用的（有托盘时隐藏窗口）。用户点「打开任务中心」
    // 就是要看到窗口，带上它只会得到「只有一个托盘图标、没有窗口」。
    command
        .arg("--await-instance")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cloudfile_core::childproc::hide(&mut command);
    command.spawn()?;
    Ok(true)
}

fn launch_desktop(desktop: Option<&Path>) -> anyhow::Result<bool> {
    let Some(desktop) = desktop.filter(|path| path.is_file()) else {
        eprintln!("警告：找不到 {DESKTOP_NAME}，任务中心没能自动启动；装好后手动双击即可。");
        return Ok(false);
    };
    // 绝不能按进程名 taskkill /F：会误杀别的安装目录/用户的 Agent，也会
    // 中断正在续租或保存的工作文件。新进程已有 --await-instance 的单实例交接；
    // 老进程仍在处理任务时允许本次启动先交给它，明确提示退出后再生效。
    let mut command = std::process::Command::new(desktop);
    // 修改逻辑（2026-10-29）：**不带 `--autostart`**。
    // `--autostart` 是给登录项用的（有托盘时把窗口藏起来），装完也带上的话，用户第一次
    // 装完只会在右下角多一个托盘图标 —— 他根本不知道有「任务中心」这个程序，而安装引导页
    // （装浏览器扩展、复制扩展目录）就在那个窗口里。装完必须把窗口放到前台。
    command
        .arg("--await-instance")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cloudfile_core::childproc::hide(&mut command);
    match command.spawn() {
        Ok(_) => {
            // 装完这一屏就是「下一步做什么」，值得在控制台说一句。
            eprintln!(
                "已请求启动任务中心；若旧版本仍在运行，请先安全退出旧版，再打开新版任务中心。"
            );
            Ok(true)
        }
        Err(error) => {
            // 启动失败不算安装失败：二进制已经就位，用户可以自己双击。
            eprintln!(
                "警告：未能请求启动任务中心（{error}）；请手动双击 {}",
                desktop.display()
            );
            Ok(false)
        }
    }
}

/// 下载、暂存、健康检查、激活。顺序就是安全属性：在暂存的构建起来并回答本地协议之前，
/// 什么都替换不了正在运行的那份。
fn apply_release(host: &str, release: &update::Release) -> anyhow::Result<()> {
    let install = update::install_root(&std::env::current_exe()?);
    let (install, staged_host) = stage_release(release, &install)?;
    // `activate` 是「接管既有注册」专用的：它从当前注册取扩展 ID、把注册指到新版本目录，
    // 并备份原 manifest。全新安装没有旧注册可接管（会明确报错），那条路走 `--install`。
    let dir = hostreg::manifest_dir()?;
    update::activate(&install, &dir, host, &release.version)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "version": release.version,
            "install_root": install.to_string_lossy(),
            "native_host": staged_host.to_string_lossy(),
        }))?
    );
    Ok(())
}

/// 下载 + 暂存 + 健康检查，返回 (安装根, 暂存好的 host 路径)。**不碰注册** ——
/// 全新安装这边没有旧注册可以接管，所以这一步必须能单独用。
fn stage_release(release: &update::Release, install: &Path) -> anyhow::Result<(PathBuf, PathBuf)> {
    let os = std::env::consts::OS;
    let mut files = Vec::new();
    for artifact in &release.artifacts {
        let kind = match artifact.kind.as_str() {
            "native-host" => update::Binary::NativeHost,
            other => {
                anyhow::ensure!(other == "desktop", "unknown artifact");
                update::Binary::Desktop
            }
        };
        files.push((kind, update::download(artifact)?));
    }
    // 这个版本目录是不是这次才建出来的：只有新建的才允许在健康检查失败时删掉。
    // 就地补齐既有目录时删掉它等于毁掉这台机器上唯一能用的安装。
    let existed = update::version_dir(install, &release.version)?.exists();
    let staged = update::stage(install, &release.version, os, &files)?;
    let staged_host = staged.join(update::Binary::NativeHost.installed_name(os));
    let staged_desktop = staged.join(update::Binary::Desktop.installed_name(os));
    // 健康检查：新构建必须能起来并回答本地协议，否则一律不许留在版本目录里。
    //
    // 修改逻辑（2026-10-28）：这一步要**启动刚写出来的 exe**。企业杀软正扫着它时，
    // CreateProcess 会直接给出「被占用」（os error 5/32），一次失败就把整个安装否掉太脆。
    // 修改逻辑（2026-10-29）：原先只重试 6 次（约 7.5 秒）。桌面进程有 7.8MB，实时扫描窗口
    // 常常比这长，表现就是「装好之后一升级就失败」，而且失败信息用完就丢。现在改成和写盘
    // 共用同一套长等待（INSTALL_BUDGET，只对「被占用」类错误等），每一轮都记进日志。
    let healthy = storage::patient(
        "启动新版本做健康检查",
        &staged_host,
        storage::INSTALL_BUDGET,
        |error| mentions_busy(&error.to_string()),
        || {
            (|| -> anyhow::Result<()> {
                let reported = update::probe(&staged_host)?;
                anyhow::ensure!(
                    reported == release.version,
                    "staged binary reports a different version"
                );
                update::probe_component(&staged_host)?;
                update::probe(&staged_desktop)?;
                Ok(())
            })()
            .map_err(|error| std::io::Error::other(format!("{error:#}")))
        },
    );
    if let Err(error) = healthy {
        // 只有这次新建的暂存目录才删：就地补齐的既有版本目录是这台机器上唯一的安装。
        if !existed {
            let _ = std::fs::remove_dir_all(&staged);
        }
        return Err(error);
    }
    Ok((install.to_path_buf(), staged_host))
}

/// 错误链里有没有 Windows 的「文件被占用」错误码（5 / 32 / 33）。
fn mentions_busy(text: &str) -> bool {
    ["(os error 5)", "(os error 32)", "(os error 33)"]
        .iter()
        .any(|needle| text.contains(needle))
}

fn run_stdio() -> anyhow::Result<()> {
    let root = root()?;
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    while let Some(value) = wire::read(&mut input)? {
        let response = match serde_json::from_value::<wire::Request>(value) {
            Ok(wire::Request::Status {}) => native::status(&root),
            Ok(wire::Request::Tasks {}) => native::task_summary(&root),
            Ok(wire::Request::CheckUpdate {}) => native::check_update(&root),
            // 升级轮询专用：只读本地文件，不拉更新清单（见 wire::Request::UpgradeState）。
            Ok(wire::Request::UpgradeState {}) => native::upgrade_state(&root),
            // 「打开任务中心」：常驻进程在跑 → 由它切前台；没跑 → 这里拉起（不杀任何东西）。
            Ok(wire::Request::OpenTaskCenter {}) => {
                match ipc::call(&root, wire::Request::OpenTaskCenter {}) {
                    Ok(value) => value,
                    Err(_) => match spawn_desktop(desktop_binary().ok().as_deref()) {
                        Ok(_) => json!({"ok":true,"launched":true}),
                        Err(error) => wire::error(&error.to_string()),
                    },
                }
            }
            Ok(wire::Request::Update {}) => start_detached("--update", &root, "agent"),
            Ok(wire::Request::UpdateExtension {}) => {
                start_detached("--update-extension", &root, "extension")
            }
            Ok(wire::Request::QueryLocalFile {
                repo_id,
                path,
                mode,
            }) => native::query_local_file(&root, &repo_id, &path, &mode),
            Ok(wire::Request::QueryLocalFolder { repo_id, mode }) => {
                native::query_local_folder(&root, &repo_id, &mode)
            }
            Ok(wire::Request::OpenWorkspace {
                repo_id,
                path,
                mode,
            }) => native::open_workspace(&root, &repo_id, &path, &mode),
            Ok(request) => forward(&root, request),
            Err(_) => wire::error("invalid or unsupported native message"),
        };
        wire::write(&mut output, &response)?;
    }
    Ok(())
}

/// Anything that needs the resident process goes over the mailbox, so the
/// window can be raised in the same place the user already looks.
fn forward(root: &Path, request: wire::Request) -> Value {
    match ipc::call(root, request.clone()) {
        Ok(value) => value,
        Err(_) => {
            // Pairing and direct .cloudfile opening are native-host work: they
            // must not depend on the desktop being alive.
            match request {
                wire::Request::PairEnrollment { server } => match device::enrollment(root, &server)
                {
                    Ok(value) => json!({"ok":true,"enrollment":value,
                        "service_path":session::service_path(root,&server).unwrap_or_default()}),
                    Err(error) => wire::error(&error.to_string()),
                },
                wire::Request::PairPrepare {
                    server,
                    service_path,
                } => match device::prepare(root, &server, &service_path) {
                    Ok(mut value) => {
                        value["ok"] = json!(true);
                        value
                    }
                    Err(error) => wire::error(&error.to_string()),
                },
                wire::Request::PairSign { server, response } => {
                    match device::pair_sign(root, &server, response) {
                        Ok(value) => json!({"ok":true,"proof":value}),
                        Err(error) => wire::error(&error.to_string()),
                    }
                }
                wire::Request::PairConfirm { server, response } => {
                    match device::confirm(root, &server, response) {
                        Ok(()) => json!({"ok":true}),
                        Err(error) => wire::error(&error.to_string()),
                    }
                }
                wire::Request::OpenUri { uri } => match paired_session::open(root, &uri) {
                    Ok(task) => json!({"ok":true,"task_id":task.task_id}),
                    Err(error) => wire::error(&error.to_string()),
                },
                _ => wire::error("desktop unavailable; inspect task center"),
            }
        }
    }
}

/// Upgrades must outlive this process: Chrome terminates the host as soon as
/// the response is written.
/// 把升级交给一个脱离宿主的自己。`kind` 决定需要什么配置：
/// Agent 自身升级要「源 + 公钥」二者齐备（否则无法验签），扩展升级只要源
/// （信任锚是同源 https + sha256），缺什么就在这里如实拒绝，别让扩展点了没反应。
fn start_detached(argument: &str, root: &Path, kind: &str) -> Value {
    if update_configured(root, kind).is_none() {
        return match kind {
            "extension" => wire::error("尚未配置更新源，无法更新扩展"),
            _ => wire::error("尚未配置签名更新源，无法从浏览器更新 Agent"),
        };
    }
    let Ok(exe) = std::env::current_exe() else {
        return wire::error("无法定位 Agent 程序");
    };
    let mut command = std::process::Command::new(exe);
    command
        .arg(argument)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS：这个子进程必须活过宿主进程（Chrome 写完应答就把宿主干掉），
        // 顺带也让它没有控制台，所以不需要 `childproc::hide`。
        command.creation_flags(0x0000_0008);
    }
    match command.spawn() {
        Ok(_) => {
            native::record_update_result(root, kind, "running");
            json!({"ok":true,"state":"running","last_update_state":"running","last_update_kind":kind})
        }
        Err(_) => {
            native::record_update_result(root, kind, "failed");
            json!({"ok":false,"error":"无法启动更新进程；当前版本保持不变"})
        }
    }
}

fn update_configured(root: &Path, kind: &str) -> Option<()> {
    let config = session::config(root).ok()?;
    // 先算策略：`update_source?` 会把该字段移走。
    let policy = config.update_policy();
    let source = config.update_source?;
    if kind == "extension" {
        // 扩展升级不需要签名公钥，但它需要一个能解析出 archive 的相对源 ——
        // scheme 的判定走这台机器上已记录的策略（正式通道 https，开发通道也许放行了 http）。
        return policy.valid_source(&source).then_some(());
    }
    // Agent 自身升级要公钥（验签用），扩展不需要。
    config.update_public_key.as_ref()?;
    policy.valid_source(&source).then_some(())
}

#[cfg(test)]
mod registration_policy_tests {
    use super::*;

    #[test]
    fn install_never_auto_takes_a_foreign_controller_even_after_staging() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().canonicalize().unwrap();
        let host = hostreg::HOST;
        let manifest = hostreg::manifest_path(&dir, host);
        let foreign = json!({
            "name": host,
            "description": "Old .NET or unrelated Host",
            "path": "/opt/other/controller",
            "type": "stdio",
            "allowed_origins": [
                "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/"
            ]
        });
        storage::write(&manifest, &foreign).unwrap();
        let staged = ("2.2.1".to_string(), dir.join("cloudfile-native-host"));
        let (binary, extension_id, takeover) =
            registration_target(&dir, host, Some(&staged), &[]).unwrap();
        assert_eq!(binary, staged.1);
        assert_eq!(
            extension_id,
            hostreg::EXTENSION_ID,
            "foreign extension ID must not be inherited"
        );
        assert!(
            !takeover,
            "staging a new Rust version is not consent to replace another controller"
        );

        let (_, _, takeover) =
            registration_target(&dir, host, Some(&staged), &["--takeover".to_string()]).unwrap();
        assert!(takeover);
    }

    #[test]
    fn ordinary_rust_upgrade_keeps_explicit_takeover_false() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().canonicalize().unwrap();
        let host = hostreg::HOST;
        let manifest = hostreg::manifest_path(&dir, host);
        let current = hostreg::manifest(
            host,
            &dir.join("v2.2.0").join("cloudfile-native-host"),
            hostreg::EXTENSION_ID,
        )
        .unwrap();
        storage::write(&manifest, &current).unwrap();
        let staged = (
            "2.2.1".to_string(),
            dir.join("v2.2.1").join("cloudfile-native-host"),
        );
        let (binary, extension_id, takeover) =
            registration_target(&dir, host, Some(&staged), &[]).unwrap();
        assert_eq!(binary, staged.1);
        assert_eq!(extension_id, hostreg::EXTENSION_ID);
        assert!(
            !takeover,
            "existing Rust installs are upgraded by ownership check, not forced takeover"
        );
    }
}
