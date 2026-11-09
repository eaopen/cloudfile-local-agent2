//! CloudFile desktop (任务中心) process.
//!
//! One resident process owns every task, lease and publication intent. The tray
//! menu, the draggable entry and the light notice are all views onto that same
//! durable store: they never hold their own copy of a decision.
//!
//! Exit policy: the process may only stop without a warning when nothing is at
//! risk. A held Checkout or an unconfirmed intent stops a silent exit, and the
//! user is asked from the same window they already use.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use cloudfile_core::{
    apps, autostart, browser, device, exclusive, extension, guard, hostreg, ipc, journal, native,
    paired_session, profile, session, settings, storage, update, upload, wire,
};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};

struct State {
    root: PathBuf,
    uploading: Arc<Mutex<HashSet<String>>>,
    quitting: Arc<AtomicBool>,
    toast: Arc<Mutex<u64>>,
    /// 已经弹过"正在下载"的文件，key = 任务目录 + 开始时间。
    announced_downloads: Arc<Mutex<HashSet<String>>>,
    /// Live handle to the tray's status row. The tray is the only place the
    /// guard verdict is visible without opening a window, so the row is kept
    /// addressable instead of being rebuilt on every refresh.
    guard_row: Arc<Mutex<Option<tauri::menu::MenuItem<tauri::Wry>>>>,
    /// Coalesces the position writes produced by a single drag: the native drag
    /// emits one `Moved` per frame and only the resting place is worth storing.
    float_moves: Arc<Mutex<u64>>,
    /// 任务中心窗口是否真的建起来了（缺 WebView2 运行时为 false）。
    ///
    /// 修改原因（2026-11-02 现场）：以前窗口由 Tauri 启动时自动创建，缺 WebView2
    /// 运行时的机器上只会在**任何中文引导之前**弹一句英文原生错误框。现在窗口改由
    /// [`create_window`] 运行时创建，托盘提示、[`show`] 与退出保护都要按这个标志分流：
    /// 没有窗口时那句「打开任务中心」必须变成中文说明，而不是点了没反应。
    windowed: Arc<AtomicBool>,
    /// 启动时是否真的缺 WebView2 运行时。
    ///
    /// 和 `windowed` 分开存：窗口没建起来可能是缺运行时，也可能是运行时在但建窗失败
    /// （例如被安全软件拦下）。两者的提示不能混为一谈 —— 把「装运行时」当成万能答案
    /// 只会让真正的原因更难查。这里只在启动时写一次，不再重探。
    runtime_missing: bool,
    /// 随包携带、校验通过的 WebView2 安装器路径（没有就是 `None`）。
    ///
    /// 缺运行时的机器上「安装」按钮直接用它把运行时装到**当前用户**（不需要管理员）；
    /// 没有随包安装器时只能退回官方下载页，所以部署方要把安装器打进发行包。
    installer: Option<PathBuf>,
}

/// 缺运行时说明只弹一次：自动入口（扩展、打开文件、配对完成）可能同时触发，
/// 每次双击都弹框比不弹更烦。**用户显式点击**不走这套限制（见 [`show_explicitly`]）。
static RUNTIME_NOTICE_SHOWN: AtomicBool = AtomicBool::new(false);

/// 本机 WebView 运行时版本；`None` 表示探测不到（Windows 上即缺 WebView2 运行时）。
///
/// 必须走 `tauri::webview_version()` 而不是自己的注册表探测：Tauri 建 webview 前判断
/// `webview_runtime_installed` 用的就是它，两边同一个入口才不会出现「我们以为能开、
/// 它却弹英文错误框」的错位（2026-11-02 现场弹的就是那个框）。
fn webview_runtime() -> Option<String> {
    tauri::webview_version().ok()
}

/// 这台机器上任务中心窗口**根本建不出来**（只有 Windows 会因为缺 WebView2 走到这里）。
///
/// 为什么只对 Windows 下结论：macOS 的 WebKit 探测会查 `com.apple.WebKit` 的 NSBundle，
/// 可能返回 Err，但 Tauri 在 macOS 上只记一条 warn 并继续建 webview；把 Err 当成门禁
/// 会让 macOS 无故变成零窗口、且没有任何说明（winbox 在非 Windows 是空实现）。
fn runtime_missing() -> bool {
    cfg!(windows) && webview_runtime().is_none()
}

/// 按 `tauri.conf.json` 的定义**在运行时**创建窗口。
///
/// 为什么不在启动时让 Tauri 自动建：配置里的三个窗口都是 `create:false`，所以这里是
/// 唯一的创建入口，也只应该在**确认运行时可用**时调用。缺 WebView2 的机器上建 webview
/// 会让 Tauri 在 release+Windows 弹出那句英文原生框（tauri-runtime-wry 的
/// `create_webview`：`!webview_runtime_installed` 时先弹框再返回错误）。
///
/// 另外 Tauri 文档提醒：在 Windows 的同步命令或事件处理器里建窗可能死锁（wry #583），
/// 所以这里只在 `setup` 里调用；`show()` 之类的晚路径不再补建窗口。
fn create_window(app: &tauri::AppHandle, label: &str) -> Option<tauri::WebviewWindow> {
    // 防御性门禁：即使将来有调用方忘了先探测，也绝不让进程走到会弹英文框的那一步。
    if runtime_missing() {
        record_launch_failure(&format!(
            "refused to create window {label}: webview runtime missing"
        ));
        return None;
    }
    let Some(config) = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == label)
        .cloned()
    else {
        // 配置里没有这个窗口：说明构建产物与代码不一致，现场只看 stderr 是看不到的。
        record_launch_failure(&format!("window config missing: {label}"));
        return None;
    };
    match tauri::WebviewWindowBuilder::from_config(app, &config).and_then(|builder| builder.build())
    {
        Ok(window) => Some(window),
        Err(error) => {
            // 一个窗口没建起来不该拖垮整个进程：托盘与后台同步仍然可用。
            // 但要留痕：发布版的 stderr 是空的，现场只能靠 launch.log 判断。
            eprintln!("CloudFile desktop: 无法创建窗口 {label}：{error}");
            record_launch_failure(&format!("window {label} could not be created: {error}"));
            None
        }
    }
}

/// 缺 WebView2 运行时的中文说明（见 `cloudfile_core::webview2`）。
/// 返回用户是否选择去下载安装包。**只能在主线程调用**：MessageBox 是模态的，
/// 显示期间会阻塞事件循环（托盘与 IPC 都停），这是用户主动点击时可接受的代价。
fn runtime_notice() -> bool {
    let open = cloudfile_core::winbox::ask(
        "CloudFile 任务中心无法打开",
        &cloudfile_core::webview2::runtime_missing_guidance(),
    );
    if open && !cloudfile_core::webview2::open_runtime_download() {
        // 分离进程的 stderr 是空的，失败要留痕，否则现场只能猜。
        record_launch_failure("could not open the WebView2 download page");
    }
    open
}

/// 本地没有随包安装器时，从配置的更新通道取一份（内网部署的常态）。
///
/// 只在**手动双击且确实缺运行时**时调用 —— 登录自启不下载，否则一台一直没装运行时的
/// 机器每次登录都要拉几百 MB。下载前必须问一次：这是几百 MB、可能几分钟，
/// 用户有权知道自己在等什么。
fn fetch_installer(root: &Path, install_root: &Path) -> Option<PathBuf> {
    let source = session::config(root)
        .ok()
        .and_then(|config| config.update_source)
        .filter(|value| !value.trim().is_empty())?;
    if !cloudfile_core::winbox::ask(
        "获取 WebView2 运行时",
        &format!(
            "本机缺少 WebView2 运行时，任务中心打不开窗口。\n\n\
             要从内网通道下载安装包吗？\n{source}\n\n\
             安装包约 200MB，下载完会自动安装（不需要管理员），装好任务中心随即打开。"
        ),
    ) {
        return None;
    }
    eprintln!("CloudFile desktop: 正在从通道获取 WebView2 安装器：{source}");
    match cloudfile_core::webview2_setup::fetch_from(&source, install_root) {
        Ok(path) => Some(path),
        Err(error) => {
            record_launch_failure(&format!("webview2 installer fetch failed: {error:#}"));
            cloudfile_core::winbox::notify(
                "获取 WebView2 安装包失败",
                &format!(
                    "{error:#}\n\n\
                     可以让管理员把安装包放到安装根的 webview2/ 目录，或稍后重试。"
                ),
            );
            None
        }
    }
}

/// 引导用户用**随包安装器**把 WebView2 装到当前用户（不需要管理员）。
///
/// 返回是否装好。调用方负责「装好后怎么办」的提示：启动阶段装好可以直接建窗口，
/// 运行中（托盘/打开任务中心）装好必须重启 Agent 才能建窗口。
/// **只能在主线程调用**（会弹模态询问框与微软安装器界面）。
fn offer_runtime_install(installer: &Path) -> anyhow::Result<bool> {
    if !cloudfile_core::winbox::ask(
        "安装 WebView2 运行时",
        &cloudfile_core::webview2::runtime_install_prompt(),
    ) {
        return Ok(false);
    }
    record_launch_failure(&format!(
        "installing webview2 runtime from {}",
        installer.display()
    ));
    // 用可见安装器而不是静默：用户点了「安装」就应该看得到进度，静默几分钟无反馈更像卡死。
    let outcome = cloudfile_core::webview2_setup::install(
        installer,
        false,
        cloudfile_core::webview2_setup::DEFAULT_TIMEOUT,
    )?;
    record_launch_failure(&format!(
        "webview2 installer exit={:?} installed={} version={:?}",
        outcome.exit_code, outcome.installed, outcome.version
    ));
    if outcome.installed {
        return Ok(true);
    }
    cloudfile_core::winbox::notify(
        "WebView2 运行时安装未完成",
        &cloudfile_core::webview2::runtime_install_failed_guidance(outcome.exit_code),
    );
    Ok(false)
}

/// 自动入口（扩展、打开文件、配对完成）请求一次说明：每个进程最多一次。
///
/// 不能在这些线程上直接弹模态框（会卡住 mailbox 循环），所以排到主线程再弹。
fn request_runtime_notice(app: &tauri::AppHandle) {
    if RUNTIME_NOTICE_SHOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = app.run_on_main_thread(|| {
        runtime_notice();
    });
}

/// 把任务中心窗口抬到前台；窗口不存在时返回 `false`。
fn raise_task_center(app: &tauri::AppHandle) -> bool {
    let Some(window) = app.get_webview_window("main") else {
        return false;
    };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
    true
}

/// 自动触发路径：抬窗口；没有窗口时最多说明一次。
///
/// 这里**绝不补建窗口**：缺运行时补建会让 release+Windows 重新弹出英文框，
/// 而运行时正常时窗口在启动阶段就已经建好（建失败属于故障，已写进 launch.log）。
fn show(app: &tauri::AppHandle) {
    if raise_task_center(app) {
        return;
    }
    if runtime_missing() {
        request_runtime_notice(app);
    } else {
        record_launch_failure("task centre window missing; not re-creating at runtime");
    }
}

/// 用户显式点击（托盘「打开任务中心」）：每次都给反馈，一次性限制会让人以为按钮坏了。
///
/// 由托盘菜单回调调用，本来就在主线程，可以直接弹模态框。
fn show_explicitly(app: &tauri::AppHandle) {
    if raise_task_center(app) {
        return;
    }
    if runtime_missing() {
        RUNTIME_NOTICE_SHOWN.store(true, Ordering::SeqCst);
        // 有随包安装器就先引导安装（免管理员）；没有才退回下载页说明。
        if let Some(installer) = app.state::<State>().installer.clone() {
            match offer_runtime_install(&installer) {
                Ok(true) => cloudfile_core::winbox::notify(
                    "WebView2 运行时已安装",
                    "请从托盘菜单「退出」Agent，再重新打开任务中心。",
                ),
                Ok(false) => {}
                Err(error) => {
                    record_launch_failure(&format!("webview2 install failed: {error:#}"));
                    cloudfile_core::winbox::notify(
                        "WebView2 运行时安装失败",
                        &format!(
                            "{error:#}\n\n{}",
                            cloudfile_core::webview2::runtime_install_failed_guidance(None)
                        ),
                    );
                }
            }
        } else {
            runtime_notice();
        }
    } else {
        // 运行时在却没有窗口：另一种故障，别用「缺 WebView2」误导现场。
        cloudfile_core::winbox::notify(
            "CloudFile 任务中心未启动",
            "任务中心窗口没有启动。请从托盘菜单「退出」Agent 后重新打开；\n\
             详情见安装目录下的 desktop-launch.log。",
        );
    }
}

fn desktop_binary() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cloudfile-desktop"))
}

/// native host 的文件名：安装时与桌面进程同级（注册、更新、自检都按它找）。
const HOST_NAME: &str = if cfg!(windows) {
    "cloudfile-native-host.exe"
} else {
    "cloudfile-native-host"
};

fn host_binary() -> Option<PathBuf> {
    let directory = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let candidate = directory.join(HOST_NAME);
    candidate.is_file().then_some(candidate)
}

/// Rich view model for the task centre. Grouping is derived here so the UI can
/// stay a pure renderer.
fn summaries(root: &Path, uploading: &Mutex<HashSet<String>>) -> anyhow::Result<Value> {
    let mut tasks = Vec::new();
    // Optional application brand is a read-only label, never a task owner or authorization.
    let application_profiles = profile::labels(root);
    let busy = uploading
        .lock()
        .map_err(|_| anyhow::anyhow!("上传状态不可用"))?
        .clone();
    for task in session::tasks(root)? {
        let changed = session::local_path(root, &task)
            .and_then(|path| session::digest(&path))
            .map(|digest| digest != task.baseline_digest);
        let pending = journal::pending(root, &task.task_id);
        let lease = journal::lease(root, &task.task_id);
        let acknowledged = journal::lease_acknowledged(root, &task.task_id);
        let closed = journal::closed(root, &task.task_id);
        let exclusive_mode = task.mode == "exclusive-edit"
            && (task.device_id.is_some() || cloudfile_core::v2_edit::is_v2(root, &task.task_id));
        let action = pending.as_ref().ok().and_then(|value| {
            value.as_ref().map(|intent| {
                match intent.action {
                    journal::Action::OptimisticUpdate => "optimistic-update",
                    journal::Action::Commit => "commit",
                    journal::Action::Checkin => "checkin",
                }
                .to_string()
            })
        });
        let needs_review = changed.is_err() || pending.is_err() || lease.is_err();
        let local_changed = changed.as_ref().copied().unwrap_or(false);
        let is_uploading = busy.contains(&task.task_id);
        let has_pending = pending.ok().flatten().is_some();
        let has_lease = lease.ok().flatten().is_some()
            || cloudfile_core::v2_edit::has_checkout(root, &task.task_id);
        let closed = closed.unwrap_or(false);
        // 云端已有新版本、或本地有未上传的修改，都要用户选一次：必须出现在「待处理」，
        // 否则用户永远看不到这个决定。
        let conflict = cloudfile_core::v2_edit::conflict(root, &task.task_id);
        let conflict_reason = conflict
            .as_ref()
            .and_then(|value| value["reason"].as_str())
            .unwrap_or("remote-changed")
            .to_string();
        let has_conflict = conflict.is_some();
        // Three states the user can act on, in the order they matter.
        let group = if has_conflict
            || needs_review
            || has_pending
            || (exclusive_mode && !closed && !has_lease)
        {
            "needs-attention"
        } else if is_uploading || local_changed || (exclusive_mode && !closed) {
            "in-progress"
        } else {
            "done"
        };
        let application = profile::label_for_file_server(&application_profiles, &task.server);
        tasks.push(json!({"task_id":task.task_id,"file_name":task.file_name,
            "application_profile_id": application.map(|p| p.profile_id.as_str()),
            "application_name": application.map(|p| p.application_name.as_str()),
            "mode":native::mode_label(&task.mode),"internal_mode":task.mode,
            "service":task.server,"remote_path":task.remote_path,
            // 库名只用于列表里区分文件来自哪个库（同名文件在不同库的路径可能一样）。
            "library_name":task.library_name,
            "group":group,
            "local_changed":local_changed,
            "needs_review":needs_review,
            "can_upload":task.mode == "optimistic-edit" && (task.device_id.is_some() || cloudfile_core::v2_edit::is_v2(root, &task.task_id)),
            "exclusive":exclusive_mode,
            "has_lease":has_lease,
            "lease_acknowledged":acknowledged.unwrap_or(false)
                || cloudfile_core::v2_edit::has_checkout(root, &task.task_id),
            "checkout_closed":closed,
            "authorization_expires_at":cloudfile_core::v2_edit::ticket_expires_at(root, &task.task_id),
            "conflict":has_conflict,
            "conflict_reason":conflict_reason,
            "uploading":is_uploading,
            "pending_upload":has_pending,"pending_action":action}));
    }
    let count = |group: &str| tasks.iter().filter(|task| task["group"] == group).count();
    Ok(
        json!({"ok":true,"cdc_enabled":cloudfile_core::cdc::enabled(),"tasks":tasks,
        // 修改原因（2026-10-23）：分组 key 必须和界面用的一致（kebab-case）。原来这里发的是
        // in_progress / needs_attention，而 app.js 按 counts['in-progress'] 取，取到 undefined
        // → 顶部「进行中」永远是 0，即使列表里就有进行中的任务。
        "counts":{"in-progress":count("in-progress"),"needs-attention":count("needs-attention"),"done":count("done")}}),
    )
}

#[tauri::command]
async fn task_summary(state: tauri::State<'_, State>) -> Result<Value, String> {
    let root = state.root.clone();
    let uploading = state.uploading.clone();
    tauri::async_runtime::spawn_blocking(move || {
        summaries(&root, &uploading).map_err(|_| "无法读取任务；请保留本地工作副本".to_string())
    })
    .await
    .map_err(|_| "任务执行失败".to_string())?
}

#[tauri::command]
fn settings_get(state: tauri::State<'_, State>) -> Result<Value, String> {
    let root = &state.root;
    let current = settings::load(root).map_err(|error| error.to_string())?;
    Ok(json!({
        "settings": current,
        "workspace_root": cloudfile_core::workspace::resolve(root).to_string_lossy(),
        "workspace_tasks": cloudfile_core::workspace::tasks_root(root).to_string_lossy(),
        "autostart_enabled": autostart::enabled(),
        "autostart_mechanism": autostart::mechanism().map(|item| item.id()),
        "install_root": update::install_root(&desktop_binary()).to_string_lossy(),
        "version": cloudfile_core::VERSION,
        "registered_host": hostreg::manifest_dir()
            .ok()
            .and_then(|dir| hostreg::state(&dir, hostreg::HOST, host_binary().as_deref()).ok())
            .map(|registration| json!({
                "host": registration.host,
                "registered": registration.binary.is_some(),
                "extension_id": registration.extension_id,
                "manifest_path": registration.manifest_path.to_string_lossy(),
                "foreign": registration.foreign,
            })),
    }))
}

/// 悬浮入口与轻提示是**独立的 webview**，拿不到主窗口那份 overview；
/// 它们各自调这个只读命令取品牌名，避免三处各写一份「CloudFile」。
/// 读失败就退回默认名 —— 提示窗口绝不该因为元数据问题画不出来。
#[tauri::command]
fn app_brand(state: tauri::State<'_, State>) -> String {
    let use_test = settings::load(&state.root)
        .unwrap_or_default()
        .use_test_server;
    profile::overview(&state.root, use_test)["brand"]["application_name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .unwrap_or("CloudFile")
        .to_string()
}

/// 后台自动刷新应用信息（ApplicationProfile V1）。
///
/// 节奏：启动后延迟 30 秒拉一次（不跟启动抢资源），之后每小时问一次
/// 「该不该刷」，真正刷新的判据在 `profile::refresh_target` 里（过期或超过一天）。
///
/// 三条硬约束：
/// 1. **只影响品牌显示** —— 刷失败保持原样，绝不阻塞文件操作、不弹窗；
/// 2. **不在登录自启时同步等它** —— 单起线程 + 先 sleep，自启路径零开销；
/// 3. 结果写 `profile-refresh.log`，现场排查「为什么名字没变」时有据可查。
fn spawn_profile_refresh(app: tauri::AppHandle, root: PathBuf) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(30));
        loop {
            let use_test = settings::load(&root).unwrap_or_default().use_test_server;
            let targets = profile::refresh_due(
                &root,
                profile::REFRESH_INTERVAL,
                cloudfile_core::VERSION,
                use_test,
            );
            let mut changed = false;
            for address in targets {
                match profile::sync(&root, &address) {
                    Ok(result) => {
                        // 按环境记住这个地址：切环境/下次刷新都不用再逐个候选去试。
                        let environment = result["environment"].as_str().unwrap_or("prod");
                        let _ = profile::remember_source(&root, &address, environment);
                        log_profile_refresh(&root, &format!("ok {environment} {address}"));
                        changed = true;
                    }
                    // 两套配置独立：一个环境拉失败不影响另一个，也不清缓存。
                    Err(error) => log_profile_refresh(&root, &format!("failed {address}: {error}")),
                }
            }
            if changed {
                let _ = app.emit("app-profile", profile::overview(&root, use_test));
            }
            // 记下"这个版本已经拉过"，升级后版本变了就会立刻再拉一次。
            let _ = settings::update(&root, |current| {
                current.profile_pull_version = Some(cloudfile_core::VERSION.to_string());
            });
            std::thread::sleep(std::time::Duration::from_secs(60 * 60));
        }
    });
}

/// 追加一行刷新记录，超过 256 KiB 先截掉前一半，避免日志无限长。
fn log_profile_refresh(root: &Path, line: &str) {
    let path = root.join("profile-refresh.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 256 * 1024 {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let keep = text.len() / 2;
                let tail = text
                    .char_indices()
                    .map(|(index, _)| index)
                    .find(|index| *index >= keep)
                    .unwrap_or(keep);
                let _ = std::fs::write(&path, &text[tail..]);
            }
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| {
            use std::io::Write;
            writeln!(file, "{stamp} {line}")
        });
}

/// 设置页的「应用信息」（ApplicationProfile V1）：本地已绑定的品牌 + 默认发现地址。
///
/// 纯读：不联网、不改信任、不碰任务。绑定与否由 [`sync_app_profile`] 决定，
/// 这里只是把本地已有的东西摊开展示。
#[tauri::command]
async fn app_profile(state: tauri::State<'_, State>) -> Result<Value, String> {
    let root = state.root.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let use_test = settings::load(&root).unwrap_or_default().use_test_server;
        profile::overview(&root, use_test)
    })
    .await
    .map_err(|_| "无法读取应用信息".to_string())
}

/// 切换测试/生产配置。
///
/// 一套软件、一个更新源，切的是**用哪一套配置**（品牌 + 该环境开放的功能通道）。
/// 切换本身不改信任关系：另一套配置里声明的文件服务仍必须已经受信任。
/// 切完立刻尝试拉一次目标环境；拉不到就用本地缓存，缓存也没有就退回默认品牌。
#[tauri::command]
async fn set_profile_environment(
    app: tauri::AppHandle,
    state: tauri::State<'_, State>,
    use_test: bool,
) -> Result<Value, String> {
    let root = state.root.clone();
    let value = tauri::async_runtime::spawn_blocking(move || -> Result<Value, String> {
        settings::update(&root, |current| current.use_test_server = use_test)
            .map_err(|error| error.to_string())?;
        let config = session::config(&root).map_err(|error| error.to_string())?;
        let mut sync_error = Value::Null;
        if let Some(target) = profile::default_address(&config, use_test) {
            match profile::sync(&root, &target) {
                Ok(result) => {
                    let environment = result["environment"].as_str().unwrap_or("prod");
                    let _ = profile::remember_source(&root, &target, environment);
                }
                // 拉取失败不是切换失败：按需求"拉不到就用本地配置"。
                Err(error) => {
                    log_profile_refresh(&root, &format!("switch failed {target}: {error}"));
                    sync_error = json!(error.to_string());
                }
            }
        }
        Ok(json!({
            "ok": true,
            "use_test": use_test,
            "sync_error": sync_error,
            "profile": profile::overview(&root, use_test),
        }))
    })
    .await
    .map_err(|_| "切换环境失败".to_string())??;
    // 托盘名字要跟着立刻变，否则用户会以为切换没生效（轮询是 5 秒一次）。
    refresh_tray(&state, &app);
    Ok(value)
}

/// 用户点「同步应用信息」时显式拉一次 Profile。
///
/// 成功后把用过的地址记回 config.json，下次不用再填。失败原样透出原因：
/// 常见的是「服务地址没被信任」「路径不对（漏了反代前缀）」「签名/有效期不符」，
/// 这三种要分别处理，笼统说「同步失败」等于没说。
#[tauri::command]
async fn sync_app_profile(
    app: tauri::AppHandle,
    state: tauri::State<'_, State>,
    address: Option<String>,
) -> Result<Value, String> {
    let root = state.root.clone();
    let value = tauri::async_runtime::spawn_blocking(move || -> Result<Value, String> {
        let use_test = settings::load(&root).unwrap_or_default().use_test_server;
        let config = session::config(&root).map_err(|error| error.to_string())?;
        let target = address
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| profile::default_address(&config, use_test))
            .ok_or_else(|| {
                "还没有受信任的 CloudFile 服务，无法确定应用信息地址；请先完成安装引导".to_string()
            })?;
        let result = profile::sync(&root, &target).map_err(|error| error.to_string())?;
        // 只记已经成功用过的地址：写失败不算同步失败，本次结果仍然有效。
        let environment = result["environment"].as_str().unwrap_or("prod");
        let _ = profile::remember_source(&root, &target, environment);
        Ok(json!({
            "ok": true,
            "source": target,
            "application_name": result["application_name"],
            "file_server": result["file_server"],
            "profile": profile::overview(&root, use_test),
        }))
    })
    .await
    .map_err(|_| "同步应用信息失败".to_string())??;
    // 刚拉到/换了一份配置，托盘名字立刻跟上。
    refresh_tray(&state, &app);
    Ok(value)
}

#[tauri::command]
fn settings_set(
    state: tauri::State<'_, State>,
    notify_on_saved: Option<bool>,
    notify_on_review: Option<bool>,
    float_enabled: Option<bool>,
    float_hidden_when_empty: Option<bool>,
    workspace_root: Option<String>,
) -> Result<Value, String> {
    let workspace_root = match workspace_root {
        Some(value) if value.trim().is_empty() => Some(
            settings::default_workspace(&state.root)
                .map_err(|error| error.to_string())?
                .to_string_lossy()
                .into_owned(),
        ),
        other => other,
    };
    let updated = settings::update(&state.root, |current| {
        if let Some(value) = notify_on_saved {
            current.notify_on_saved = value;
        }
        if let Some(value) = notify_on_review {
            current.notify_on_review = value;
        }
        if let Some(value) = float_enabled {
            current.float_enabled = value;
        }
        if let Some(value) = float_hidden_when_empty {
            current.float_hidden_when_empty = value;
        }
        if let Some(value) = workspace_root.clone() {
            current.workspace_root = Some(value.trim().to_string());
        }
    })
    .map_err(|error| error.to_string())?;
    Ok(json!(updated))
}

/// Monitor identity used to keep one resting place per display.
fn display_key(window: &tauri::WebviewWindow) -> String {
    window
        .current_monitor()
        .ok()
        .flatten()
        .and_then(|monitor| monitor.name().cloned())
        .unwrap_or_else(|| "unknown".to_string())
}

/// 悬浮球的落点：**拖过的按记住的位置恢复**（并夹进当前屏幕，换分辨率不会飘到屏幕外），
/// 没拖过就给一个明确默认位置 —— 右侧靠边、垂直居中，不压任务栏也不挡标题栏。
///
/// 修改逻辑（2026-10-27）：原来没有默认位置，只有「记住过才恢复」，第一次出现完全交给
/// 系统决定（常在左上角，压着标题栏）；而且恢复时不夹紧，换显示器后可能整块留在屏幕外。
fn place_float(window: &tauri::WebviewWindow, saved: Option<settings::FloatPosition>) {
    let Ok(size) = window.outer_size() else {
        return;
    };
    let monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return;
    };
    let area = monitor.position();
    let screen = monitor.size();
    let width = size.width as i32;
    let height = size.height as i32;
    let margin = 24;
    let (x, y) = match saved {
        Some(position) => (
            position
                .x
                .clamp(area.x, area.x + screen.width as i32 - width),
            position
                .y
                .clamp(area.y, area.y + screen.height as i32 - height),
        ),
        None => (
            area.x + screen.width as i32 - width - margin,
            area.y + (screen.height as i32 - height) / 2,
        ),
    };
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
}

/// Stores where the entry came to rest, keyed by the display it landed on.
fn persist_float_position(app: &tauri::AppHandle) {
    let state = app.state::<State>();
    let Some(window) = app.get_webview_window("float") else {
        return;
    };
    let Ok(scale) = window.scale_factor() else {
        return;
    };
    let Ok(position) = window.outer_position() else {
        return;
    };
    let x = (position.x as f64 / scale).round() as i32;
    let y = (position.y as f64 / scale).round() as i32;
    // A failed write costs a remembered position, not a task: stay quiet.
    let _ = settings::set_position(&state.root, &display_key(&window), x, y);
}

/// Watches the native drag and saves only the final resting place. The drag is
/// performed by the OS, so the webview never reports pointer coordinates and
/// nothing here has to convert between logical and physical pixels.
fn watch_float_drag(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("float") else {
        return;
    };
    let handle = app.clone();
    window.on_window_event(move |event| {
        if !matches!(event, tauri::WindowEvent::Moved(_)) {
            return;
        }
        let state = handle.state::<State>();
        let Ok(generation) = state.float_moves.lock().map(|mut value| {
            *value += 1;
            *value
        }) else {
            return;
        };
        let handle = handle.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(450));
            let state = handle.state::<State>();
            let Ok(current) = state.float_moves.lock().map(|value| *value) else {
                return;
            };
            if current == generation {
                persist_float_position(&handle);
            }
        });
    });
}

/// Shows or hides the entry according to the task list and the user's choice.
/// Called by the main window after every refresh so the two views cannot drift.
#[tauri::command]
async fn float_sync(
    app: tauri::AppHandle,
    state: tauri::State<'_, State>,
) -> Result<Value, String> {
    let root = state.root.clone();
    let uploading = state.uploading.clone();
    let summary = tauri::async_runtime::spawn_blocking(move || summaries(&root, &uploading))
        .await
        .map_err(|_| "任务执行失败".to_string())?
        .map_err(|_| "无法读取任务".to_string())?;
    let config = settings::load(&state.root).map_err(|error| error.to_string())?;
    let total = summary["tasks"].as_array().map(Vec::len).unwrap_or(0);
    let attention = summary["counts"]["needs-attention"].as_u64().unwrap_or(0);
    let Some(window) = app.get_webview_window("float") else {
        return Ok(json!({"visible": false}));
    };
    let visible = config.float_enabled && (total > 0 || !config.float_hidden_when_empty);
    let was_visible = window.is_visible().unwrap_or(false);
    if visible {
        if !was_visible {
            // Only restore on the hidden-to-shown transition, never during a drag.
            let display = display_key(&window);
            place_float(
                &window,
                settings::position(&state.root, &display).ok().flatten(),
            );
            let _ = window.show();
        }
    } else if was_visible {
        let _ = window.hide();
    }
    let payload = json!({"total": total, "needs_attention": attention, "visible": visible});
    let _ = window.emit("float-state", payload.clone());
    Ok(payload)
}

#[tauri::command]
fn open_task_center(app: tauri::AppHandle) {
    show(&app);
}

/// 正在下载的文件（在 `tasks/<id>/preparing.json` 里）。
///
/// 返回值第一个元素是幂等键：同一份文件重复标记只提示一次。
fn preparing_files(root: &Path) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("tasks")) else {
        return result;
    };
    for entry in entries.flatten() {
        let path = entry.path().join("preparing.json");
        let Ok(marker) = cloudfile_core::storage::read::<Value>(&path) else {
            continue;
        };
        let name = marker["file_name"].as_str().unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        let key = format!(
            "{}:{}",
            entry.file_name().to_string_lossy(),
            marker["started_at"].as_i64().unwrap_or(0)
        );
        result.push((key, name));
    }
    result
}

/// 任务目录的"指纹"：哪些目录已经有 task.json、哪些还在下载中。
/// 用来判断"列表该刷新了"，比等 10 秒定时器快得多。
fn task_signature(root: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("tasks")) else {
        return String::new();
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.join("task.json").exists() {
            parts.push(format!("{name}:task"));
        } else if path.join("preparing.json").exists() {
            parts.push(format!("{name}:preparing"));
        }
    }
    parts.sort();
    parts.join(",")
}

/// Non-focusing notice. The window is only ever shown, never focused, and is
/// hidden again after a fixed interval unless a newer notice replaced it.
fn present_toast(app: &tauri::AppHandle, level: &str, text: &str) {
    let state = app.state::<State>();
    let Some(window) = app.get_webview_window("toast") else {
        return;
    };
    let text: String = text.chars().take(240).collect();
    let Ok(generation) = state.toast.lock().map(|mut value| {
        *value += 1;
        *value
    }) else {
        return;
    };
    let _ = window.set_ignore_cursor_events(true);
    // 明确抬到最前：只靠 show() 在部分机器上会被主窗口压住。
    let _ = window.set_always_on_top(true);
    if let Some(monitor) = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
    {
        let size = monitor.size();
        let position = monitor.position();
        // 位置全是**物理像素**，窗口尺寸却是逻辑像素：175% 缩放下 420 逻辑 = 735 物理，
        // 直接拿 420 去减会让窗口右侧探出屏幕 300 多像素。这里取窗口真实外框尺寸。
        let scale = monitor.scale_factor();
        let width = window
            .outer_size()
            .map(|value| value.width as i32)
            .unwrap_or((420.0 * scale) as i32);
        let margin = (16.0 * scale) as i32;
        let x = (position.x + size.width as i32 - width - margin).max(position.x + margin);
        let y = position.y + margin;
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    }
    let _ = window.emit("toast", json!({"level": level, "text": text}));
    let _ = window.show();
    let handle = app.clone();
    // 「正在下载」要留够时间看清；其余照旧。
    let stay = if level == "working" { 5_000 } else { 3_500 };
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(stay));
        let state = handle.state::<State>();
        let Ok(current) = state.toast.lock().map(|value| *value) else {
            return;
        };
        if current == generation {
            if let Some(window) = handle.get_webview_window("toast") {
                let _ = window.hide();
            }
        }
    });
}

#[tauri::command]
fn toast_show(app: tauri::AppHandle, level: String, text: String) {
    present_toast(&app, &level, &text);
}

#[tauri::command]
fn toast_hide(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("toast") {
        let _ = window.hide();
    }
}

/// Re-read from disk on every call. A cached answer here would be a lie after
/// the journal changed elsewhere.
#[tauri::command]
fn exit_assessment(state: tauri::State<'_, State>) -> Value {
    serde_json::to_value(guard::assess(&state.root))
        .unwrap_or_else(|_| json!({"safe_to_exit": false}))
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle, state: tauri::State<'_, State>, force: bool) -> Value {
    let assessment = guard::assess(&state.root);
    if !force && !assessment.safe_to_exit {
        // Keep running, keep renewing, and ask from the window the user knows.
        show(&app);
        let _ = app.emit(
            "confirm-exit",
            serde_json::to_value(&assessment).unwrap_or(Value::Null),
        );
        return json!({"state": "blocked", "assessment": assessment});
    }
    state.quitting.store(true, Ordering::SeqCst);
    app.exit(0);
    json!({"state": "exiting"})
}

/// 扩展解压到哪个目录：配置里有就用配置的，否则 agent 根目录下的 `extension\`。
fn extension_dir(root: &Path) -> PathBuf {
    session::config(root)
        .ok()
        .and_then(|config| config.extension_dir)
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(extension::DIR_NAME))
}

#[tauri::command]
async fn register_host(state: tauri::State<'_, State>, takeover: bool) -> Result<Value, String> {
    let binary = host_binary().ok_or_else(|| "本机未找到 cloudfile-native-host".to_string())?;
    let dir = hostreg::manifest_dir().map_err(|error| error.to_string())?;
    // 扩展 ID 是产品常量：界面上没有「手输 ID」这条路，所以这里也不需要参数。
    let outcome = hostreg::install(
        &dir,
        hostreg::HOST,
        hostreg::EXTENSION_ID,
        &binary,
        takeover,
    )
    .map_err(|error| error.to_string())?;
    let _ = state.root;
    serde_json::to_value(outcome).map_err(|error| error.to_string())
}

#[tauri::command]
fn unregister_host() -> Result<(), String> {
    let dir = hostreg::manifest_dir().map_err(|error| error.to_string())?;
    hostreg::unregister(&dir, hostreg::HOST)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn rollback_host() -> Result<String, String> {
    let dir = hostreg::manifest_dir().map_err(|error| error.to_string())?;
    hostreg::rollback(&dir, hostreg::HOST)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| error.to_string())
}

/// 引导页要看到的全部事实。**只报状态，不做决定** ——
/// 「装没装好」由 [`setup_apply`] 去改，这里给出的是改之前的样子。
#[tauri::command]
fn setup_state(state: tauri::State<'_, State>) -> Result<Value, String> {
    let root = &state.root;
    let current = settings::load(root).map_err(|error| error.to_string())?;
    let config = session::config(root).unwrap_or_default();
    let registration = hostreg::manifest_dir()
        .ok()
        .and_then(|dir| hostreg::state(&dir, hostreg::HOST, host_binary().as_deref()).ok());
    let dir = extension_dir(root);
    let workspace = cloudfile_core::workspace::resolve(root);
    let registered = registration
        .as_ref()
        .map(|item| item.binary.is_some() && !item.foreign)
        .unwrap_or(false);
    // A reusable Agent has no embedded enterprise trust origins.
    // Administrators or users configure actual services in the Services view.
    let trusted = config.allowed_origins.len();
    // 只有真正的「全新机器」才停在引导页。升级、自启动、扩展里点开任务中心时，
    // 本机其实已经装好了 —— 引导页每次都挡在前面只会让用户每天多点一次。
    // 判据只看安装事实（Host 已注册 + 有受信任服务 + 扩展已部署），
    // 不看"用户是否点过完成"，点没点过是界面状态，不该决定要不要拦人。
    let ready = registered && trusted > 0 && extension::installed_version(&dir).is_some();
    let completed = current.setup_completed || ready;
    if completed && !current.setup_completed {
        let _ = settings::update(root, |value| value.setup_completed = true);
    }
    Ok(json!({
        "version": cloudfile_core::VERSION,
        "install_root": update::install_root(&desktop_binary()).to_string_lossy(),
        "host": hostreg::HOST,
        "host_known": registration.is_some(),
        "host_registered": registered,
        "host_foreign": registration.as_ref().map(|item| item.foreign).unwrap_or(false),
        "host_manifest": registration
            .as_ref()
            .map(|item| item.manifest_path.to_string_lossy().into_owned()),
        "extension_id": hostreg::EXTENSION_ID,
        "extension_dir": dir.to_string_lossy(),
        "extension_version": extension::installed_version(&dir),
        "workspace_root": workspace.to_string_lossy(),
        "workspace_tasks": cloudfile_core::workspace::tasks_root(root).to_string_lossy(),
        "workspace_default": cloudfile_core::workspace::default_root_hint(&state.root),
        "workspace_exists": workspace.is_dir(),
        "autostart_enabled": autostart::enabled(),
        "trusted_count": trusted,
        "trusted_ready": trusted > 0,
        "update_source": config.update_source,
        "setup_completed": completed,
        "chrome_found": browser::chrome_path().is_some(),
    }))
}

/// 一键完成：受信任服务 → 工作目录 → 注册 Host → 开机自启 → 部署扩展。
///
/// 每一步都**独立成败**：注册失败不该让已经建好的目录或已经写入的服务回滚，
/// 那是三件互不依赖的事。返回的 `steps` 直接渲染在引导页上，失败项带着原因，
/// 用户可以只重试那一步（再点一次「一键完成」是幂等的）。
#[tauri::command]
async fn setup_apply(state: tauri::State<'_, State>) -> Result<Value, String> {
    let root = state.root.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<Value, String> {
        let mut steps = Vec::new();

        // 1) Trust remains an explicit user/admin action, never silently
        // seed enterprise URLs into a public reusable client.
        let trusted_count = session::config(&root)
            .map(|config| config.allowed_origins.len())
            .unwrap_or_default();
        steps.push(json!({"key":"trusted","ok":trusted_count > 0,
        "detail": if trusted_count > 0 {
            format!("{trusted_count} 个受信任服务已配置")
        } else {
            "请先在「服务连接」中添加组织提供的网盘地址".to_string()
        }}));

        // 2) 工作目录：默认地址在 D:\CloudFileAgent2\workspace（可在设置里改）。
        let workspace = cloudfile_core::workspace::resolve(&root);
        steps.push(
            match std::fs::create_dir_all(cloudfile_core::workspace::tasks_root(&root)) {
                Ok(()) => json!({"key":"workspace","ok":true,
                    "detail":workspace.to_string_lossy()}),
                Err(error) => json!({"key":"workspace","ok":false,
                    "detail":format!("{}：{error}", workspace.display())}),
            },
        );

        // 3) 注册 Host。扩展 ID 固定；接管的旧注册会先备份，可用「回退」还原。
        let registration = (|| -> anyhow::Result<hostreg::InstallOutcome> {
            let binary =
                host_binary().ok_or_else(|| anyhow::anyhow!("本机未找到 cloudfile-native-host"))?;
            let dir = hostreg::manifest_dir()?;
            // An older CloudFile Agent registration is ours: this explicit
            // install/upgrade retires it (no second confirmation). A different
            // program's controller still requires confirmation in Host settings.
            hostreg::install(&dir, hostreg::HOST, hostreg::EXTENSION_ID, &binary, false)
        })();
        steps.push(match registration {
            Ok(outcome) => json!({"key":"host","ok":true,"detail":outcome.manifest_path,
                "backed_up":outcome.backed_up,
                "previous":outcome.previous_manifest_path}),
            Err(error) => json!({"key":"host","ok":false,"detail":format!("{error:#}")}),
        });

        // 4) 开机自启：企业机器上 Run 键可能被策略锁死，失败只报告，不阻断安装。
        steps.push(match autostart::install(&root, &desktop_binary()) {
            Ok(item) => json!({"key":"autostart","ok":true,"detail":item.id()}),
            Err(error) => json!({"key":"autostart","ok":false,"detail":format!("{error:#}")}),
        });

        // 5) 部署扩展：从通道下载并解包到 <agent 根>\extension。
        //    没有更新源时这一步会失败 —— 那也不阻塞其余四步，用户可手动解压。
        steps.push(match native::update_extension(&root) {
            Ok(detail) => {
                // 把扩展实际解压到的目录记进 config：引导页、设置页、后续更新都读同一处，
                // 免得"装好了但不知道该去哪儿加载"。
                let target = extension_dir(&root);
                if let Ok(mut config) = session::config(&root) {
                    let text = target.to_string_lossy().into_owned();
                    if config.extension_dir.as_deref() != Some(text.as_str()) {
                        config.extension_dir = Some(text);
                        let _ =
                            cloudfile_core::storage::write(&root.join("config.json"), &config);
                    }
                }
                json!({"key":"extension","ok":true,"detail":format!("{detail}；目录 {}", target.display())})
            }
            Err(error) => json!({"key":"extension","ok":false,"detail":format!("{error:#}")}),
        });

        Ok(json!({"steps": steps}))
    })
    .await
    .map_err(|_| "安装步骤执行失败".to_string())?
}

/// 引导页的「完成」：只记一个界面状态，不改任何安装事实。
#[tauri::command]
fn setup_finish(state: tauri::State<'_, State>) -> Result<Value, String> {
    settings::update(&state.root, |current| current.setup_completed = true)
        .map(|updated| json!(updated))
        .map_err(|error| error.to_string())
}

/// 在资源管理器里打开扩展目录（比手抄路径可靠）。
#[tauri::command]
fn open_extension_dir(state: tauri::State<'_, State>) -> Result<String, String> {
    let dir = extension_dir(&state.root);
    std::fs::create_dir_all(&dir).map_err(|error| format!("{}：{error}", dir.display()))?;
    browser::open_directory(&dir).map_err(|error| format!("{error:#}"))?;
    Ok(dir.to_string_lossy().into_owned())
}

/// 手动更新扩展（「Chrome 注册与升级」面板里的那个按钮）。
#[tauri::command]
async fn update_extension(state: tauri::State<'_, State>) -> Result<String, String> {
    let root = state.root.clone();
    tauri::async_runtime::spawn_blocking(move || native::update_extension(&root))
        .await
        .map_err(|_| "扩展更新失败".to_string())?
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
fn autostart_set(state: tauri::State<'_, State>, enabled: bool) -> Result<Value, String> {
    if enabled {
        autostart::install(&state.root, &desktop_binary()).map_err(|error| error.to_string())?;
    } else {
        autostart::remove(&state.root).map_err(|error| error.to_string())?;
    }
    // 开关状态读操作系统（不是刚写进去的意图），途径一并返回：Run 键被策略锁住的
    // 机器上实际用的是「启动」文件夹，界面得能把这件事说清楚。
    Ok(json!({
        "autostart_enabled": autostart::enabled(),
        "autostart_mechanism": autostart::mechanism().map(|item| item.id()),
    }))
}

#[tauri::command]
async fn check_update(state: tauri::State<'_, State>) -> Result<Value, String> {
    let root = state.root.clone();
    tauri::async_runtime::spawn_blocking(move || native::check_update(&root))
        .await
        .map_err(|_| "更新检查失败".to_string())
}

/// Launches the upgrade in a detached process so the UI stays responsive and
/// the running build is only replaced by the staged one after a health check.
///
/// Two things stop an upgrade before it starts. The update channel must be
/// configured and pinned, otherwise there is nothing to verify against. And an
/// upgrade replaces the process that renews Checkouts and publishes intents, so
/// it is refused while anything is at risk unless the caller passes `force`
/// with the user's explicit consent — the same rule the exit path follows.
#[tauri::command]
fn run_update(state: tauri::State<'_, State>, force: bool) -> Result<Value, String> {
    let config = session::config(&state.root).map_err(|error| error.to_string())?;
    if config.update_source.is_none() || config.update_public_key.is_none() {
        return Err("尚未配置签名更新源；请在配置中设置更新地址与公钥后再升级".to_string());
    }
    let assessment = guard::assess(&state.root);
    if !assessment.safe_to_exit && !force {
        return Ok(json!({"state": "blocked", "assessment": assessment}));
    }
    let binary = host_binary().ok_or_else(|| "本机未找到 cloudfile-native-host".to_string())?;
    // 修改逻辑（2026-10-29）：升级进程的 stdout/stderr 原来都指向 null，客户机上「升级失败」
    // 就只剩这四个字 —— 分不清是下载不通、验签不过，还是新版本 exe 被安全软件拦着起不来。
    // 现在全部落到 <安装根>\update.log，失败原因同时写进 update-result.json（见 native-host）。
    let log = cloudfile_core::native::update_log_path(&state.root);
    note_update_log(
        &log,
        &format!(
            "\n===== 开始后台升级：当前 {}，{} =====",
            cloudfile_core::VERSION,
            cloudfile_core::now()
        ),
    );
    let mut command = std::process::Command::new(binary);
    command.arg("--update").stdin(std::process::Stdio::null());
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
    {
        Ok(file) => {
            if let Ok(dup) = file.try_clone() {
                command.stdout(std::process::Stdio::from(dup));
            }
            command.stderr(std::process::Stdio::from(file));
        }
        Err(_) => {
            command
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // 同 native-host：DETACHED_PROCESS 让升级进程脱离本窗口活下来，并且没有控制台
        // （所以这里不需要 `childproc::hide`）。
        command.creation_flags(0x0000_0008);
    }
    // 连进程都起不来时（企业策略 / 安全软件拦了新程序启动）留一份同样的证据。
    if let Err(error) = command.spawn() {
        let message = format!("无法启动更新进程（{error}）；当前版本保持不变");
        native::record_update_failure(&state.root, "agent", &message);
        note_update_log(&log, &message);
        return Err(message);
    }
    Ok(json!({"state": "running"}))
}

#[tauri::command]
fn open_workspace(state: tauri::State<'_, State>) -> Result<(), String> {
    let directory = settings::ensure_workspace(&state.root).map_err(|error| error.to_string())?;
    apps::open_directory(&directory).map_err(|error| error.to_string())
}

/// 删除任务：任务目录 + 工作副本 + 空目录清理（只在 workspace 根以下）。
/// 已完成的任务才在界面上给出按钮，但这里不做门禁 —— 用户主动删除永远该被尊重。
#[tauri::command]
async fn delete_task(state: tauri::State<'_, State>, task_id: String) -> Result<Value, String> {
    let root = state.root.clone();
    tauri::async_runtime::spawn_blocking(move || {
        session::remove_task(&root, &task_id).map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "删除任务失败".to_string())?
}

/// 冲突处理：`remote` 用线上版本覆盖本地，`local` 保留本地版本，任务仍是一条。
#[tauri::command]
async fn resolve_conflict(
    state: tauri::State<'_, State>,
    task_id: String,
    choice: String,
) -> Result<Value, String> {
    let root = state.root.clone();
    tauri::async_runtime::spawn_blocking(move || {
        cloudfile_core::v2_edit::resolve_conflict(&root, &task_id, &choice)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "处理冲突失败".to_string())?
}

/// 升级面板轮询用：当前运行版本、注册指向的版本（磁盘上已装的最新一份）、以及最近一次
/// 升级的结果（native-host 在独立进程里跑的，只有 update-result.json 能告诉我们成败）。
#[tauri::command]
fn update_state(state: tauri::State<'_, State>) -> Result<Value, String> {
    let active = hostreg::manifest_dir()
        .ok()
        .and_then(|dir| update::active_version(&dir, hostreg::HOST).ok().flatten());
    let newer = active
        .as_deref()
        .map(|version| update::compare(version, cloudfile_core::VERSION).is_gt())
        .unwrap_or(false);
    Ok(json!({
        "version": cloudfile_core::VERSION,
        "active_version": active,
        "newer": newer,
        "result": native::load_update_result(&state.root),
        // 界面在失败时要把这个位置告诉用户：完整过程（含「文件被安全软件占用，继续等待」）
        // 都在里面，发给我们就能定位。
        "log": native::update_log_path(&state.root).to_string_lossy(),
    }))
}

/// 升级完重启任务中心：拉起注册指向的那份桌面（新版本），再退出自己。
/// 不重启的话，跑的还是旧构建 —— 用户会觉得「升级没生效」。
#[tauri::command]
fn restart_app(app: tauri::AppHandle, state: tauri::State<'_, State>) -> Result<Value, String> {
    let dir = hostreg::manifest_dir().map_err(|error| error.to_string())?;
    let version = update::active_version(&dir, hostreg::HOST)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "没有找到已安装的版本".to_string())?;
    let binary = update::version_dir(&state.root, &version)
        .map_err(|error| error.to_string())?
        .join(update::Binary::Desktop.installed_name(std::env::consts::OS));
    if !binary.is_file() {
        return Err(format!("新版本目录里没有任务中心：{}", binary.display()));
    }
    let mut command = std::process::Command::new(&binary);
    // 修改逻辑（2026-10-27）：升级重启是**用户点出来的**，新实例必须出现在前台。
    // 之前这里带着 --autostart，而 --autostart 在有托盘时会隐藏窗口 —— 用户看到的就是
    // 「重启后任务中心不见了，只剩右下角托盘图标」。
    command
        .arg("--await-instance")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0008);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动新版本：{error}"))?;
    // 新实例要先等老进程释放 desktop.lock（最多 10 秒），所以「几百毫秒内就退出的」
    // 一定是坏消息：参数被拒、缺 WebView2Loader.dll、目录被删……
    // 原来这里不看结果直接 app.exit(0)，用户看到的就是「点和没点一样，界面上什么都没了」。
    // 现在先确认它还活着；已经死了就把退出码交回界面，并把启动日志的位置告诉用户。
    std::thread::sleep(std::time::Duration::from_millis(600));
    if let Ok(Some(status)) = child.try_wait() {
        return Err(format!(
            "新版本启动后立即退出（{status}）：详见 {}",
            state.root.join("desktop-launch.log").display()
        ));
    }
    app.exit(0);
    Ok(json!({"state": "restarting", "version": version}))
}

#[tauri::command]
async fn save_cloud(state: tauri::State<'_, State>, task_id: String) -> Result<Value, String> {
    let root = state.root.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let result = upload::save(&root, &task_id).map_err(|error| error.to_string())?;
        Ok::<Value, String>(result)
    })
    .await
    .map_err(|_| "上传任务执行失败".to_string())??;
    Ok(result)
}

#[tauri::command]
async fn recover_upload(state: tauri::State<'_, State>, task_id: String) -> Result<Value, String> {
    let root = state.root.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = upload::recover(&root, &task_id).map_err(|error| error.to_string())?;
        Ok::<Value, String>(result)
    })
    .await
    .map_err(|_| "查询任务执行失败".to_string())?
}

#[tauri::command]
async fn begin_pairing(state: tauri::State<'_, State>, origin: String) -> Result<(), String> {
    let root = state.root.clone();
    tauri::async_runtime::spawn_blocking(move || {
        device::preflight(&root, &origin)
            .and_then(|()| device::begin(&root, &origin))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "设备连接失败".to_string())?
}

#[tauri::command]
fn service_status(state: tauri::State<'_, State>) -> Result<Value, String> {
    let origins = session::config(&state.root)
        .map_err(|_| "配置读取失败".to_string())?
        .allowed_origins;
    Ok(json!(origins
        .into_iter()
        .map(|origin| json!({
            "state":device::pairing_state(&state.root,&origin),
            "service_url":format!("{}{}",origin,session::service_path(&state.root,&origin).unwrap_or_default()),
            // 默认项只用于界面标注「不可删除」，判定权威在 session::untrust。
            "default":false,
            "origin":origin,
        }))
        .collect::<Vec<_>>()))
}

#[tauri::command]
fn trusted_origins(state: tauri::State<'_, State>) -> Result<Vec<String>, String> {
    session::config(&state.root)
        .map(|config| config.allowed_origins)
        .map_err(|_| "配置读取失败".into())
}

#[tauri::command]
fn open_task(state: tauri::State<'_, State>, task_id: String) -> Result<(), String> {
    session::launch(&state.root, &task_id).map_err(|error| error.to_string())
}

/// Explicit user action. `checkin` publishes and releases the Checkout.
#[tauri::command]
async fn commit_task(
    state: tauri::State<'_, State>,
    task_id: String,
    checkin: bool,
) -> Result<Value, String> {
    let root = state.root.clone();
    tauri::async_runtime::spawn_blocking(move || {
        exclusive::commit(&root, &task_id, checkin).map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "提交任务执行失败".to_string())?
}

/// Recovers the original commit intent; never creates a new one.
#[tauri::command]
async fn recover_commit(state: tauri::State<'_, State>, task_id: String) -> Result<Value, String> {
    let root = state.root.clone();
    tauri::async_runtime::spawn_blocking(move || {
        exclusive::recover(&root, &task_id).map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "查询提交执行失败".to_string())?
}

/// Explicit retry of a frozen commit, kept separate from read-only recovery.
#[tauri::command]
async fn retry_commit(state: tauri::State<'_, State>, task_id: String) -> Result<Value, String> {
    let root = state.root.clone();
    tauri::async_runtime::spawn_blocking(move || {
        exclusive::retry(&root, &task_id).map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "重试提交执行失败".to_string())?
}

#[tauri::command]
async fn renew_checkout(state: tauri::State<'_, State>, task_id: String) -> Result<Value, String> {
    let root = state.root.clone();
    tauri::async_runtime::spawn_blocking(move || {
        exclusive::renew_checkout(&root, &task_id).map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "续租执行失败".to_string())?
}

/// Explicit release. The server refuses while a commit intent is pending.
#[tauri::command]
async fn release_checkout(
    state: tauri::State<'_, State>,
    task_id: String,
) -> Result<Value, String> {
    let root = state.root.clone();
    tauri::async_runtime::spawn_blocking(move || {
        exclusive::release(&root, &task_id).map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "释放 Checkout 执行失败".to_string())?
}

/// 无窗口时托盘的「退出」要问的那句话。
///
/// 修改原因（2026-11-02）：有独占任务或待确认提交时，退出保护原本靠任务中心窗口
/// 弹确认框。缺 WebView2 运行时没有窗口，这时若仍然沿用「show + emit」，
/// 托盘的「退出」就变成点了没反应 —— 用户只能去任务管理器杀进程，更容易丢状态。
fn exit_risk_text(assessment: &guard::Assessment) -> String {
    format!(
        "仍有 {} 个独占任务、{} 项待确认提交。\n\n\
现在退出会停止独占续租，授权到期后其他人可能接管这些文件；\
待确认提交的结果将无从得知，需要重新核对。本地工作副本不会被删除。\n\n\
确定要退出吗？",
        assessment.active_checkouts, assessment.uncertain
    )
}

/// 托盘提示文案。抽成纯函数是为了能测全三个分支（正常 / 缺运行时 / 建窗失败），
/// 这三种在托盘上必须看起来不一样，否则现场分不清该装运行时还是该查日志。
///
/// `brand` 来自当前生效的应用配置（`profile::active`）：托盘的悬停提示就是用户在
/// 任务栏上看到的名字，必须和侧栏、标题栏一致，不能再写死 `CloudFile`。
fn tray_tooltip(windowed: bool, runtime_missing: bool, brand: &str, summary: &str) -> String {
    let version = cloudfile_core::VERSION;
    let brand = if brand.trim().is_empty() {
        "CloudFile"
    } else {
        brand
    };
    if windowed {
        format!("{brand} {version} · {summary}")
    } else if runtime_missing {
        format!("{brand} {version} · 后台运行（任务中心需要 WebView2 运行时）· {summary}")
    } else {
        format!("{brand} {version} · 后台运行（任务中心窗口启动失败，详见 desktop-launch.log）· {summary}")
    }
}

/// 当前环境该显示的名字。读不到配置就退回默认名 —— 托盘不能因为元数据问题空着。
fn active_brand(root: &Path) -> String {
    let use_test = settings::load(root).unwrap_or_default().use_test_server;
    profile::display_name(root, use_test)
}

/// Keeps the tray tooltip and the guard menu row honest about the real store.
/// `set_text` dispatches to the main thread on its own, so this is safe to call
/// from the background guard loop.
fn refresh_tray(state: &State, app: &tauri::AppHandle) {
    let assessment = guard::assess(&state.root);
    let summary = if assessment.safe_to_exit {
        "可以安全退出".to_string()
    } else {
        format!(
            "{} 个独占任务 · {} 项待确认",
            assessment.active_checkouts, assessment.uncertain
        )
    };
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(tray_tooltip(
            state.windowed.load(Ordering::SeqCst),
            state.runtime_missing,
            &active_brand(&state.root),
            &summary,
        )));
    }
    if let Ok(row) = state.guard_row.lock() {
        if let Some(item) = row.as_ref() {
            let prefix = if assessment.safe_to_exit {
                "状态"
            } else {
                "注意"
            };
            let _ = item.set_text(format!("{prefix}：{summary}"));
        }
    }
}

const USAGE: &str = "\
CloudFile Agent desktop

Usage: cloudfile-desktop [--self-test] [--autostart] [--await-instance]

  --version     print the build version
  --component   print the local component contract as JSON
  --self-test   check the local installation and exit (JSON, non-zero when broken);
                never opens a window, so it is safe in automation
  --help, -h    print this message
  --autostart   start resident in the tray without opening the window; the login
                item passes this so logging in never steals focus
  --await-instance
                wait up to 10s for the previous instance to release the desktop
                lock; the upgrade restart and the extension's open-task-center
                button pass this so the new instance is not dropped silently
";

/// 启动时允许出现的「横杠参数」，只放真正会被传进来的两个：
/// 登录项传 `--autostart`，升级重启 / 「打开任务中心」传 `--await-instance`。
///
/// 修改逻辑（2026-10-27）：这里原来写成 `argument != "--autostart"` 的硬编码判断。
/// 2.0.8 为了「重启后回到前台」把重启命令里的 `--autostart` 去掉了，于是新实例的
/// **第一个参数**变成 `--await-instance`，直接落进「未知参数」分支 `exit(2)`；
/// 而这条链路的 stderr 是 null（分离进程），用户看到的就是任务中心和托盘一起消失、
/// 没有任何提示。2.0.9 的现场故障就是这个。
const LAUNCH_FLAGS: [&str; 2] = ["--autostart", "--await-instance"];

/// 参数是否属于已知启动参数。
fn is_launch_flag(argument: &str) -> bool {
    LAUNCH_FLAGS.contains(&argument)
}

/// 启动阶段的失败必须留痕：常驻进程是分离启动的（stdin/stdout/stderr 都是 null），
/// 参数被拒或初始化失败时外面什么都看不到。写一行到安装根，现场一眼能查。
///
/// 加 1MB 上限（与 `note_update_log` 同款）：缺 WebView2 的机器每次启动都会写一行，
/// 不设上限的话这个文件会跟着登录次数无限长下去。
fn record_launch_failure(message: &str) {
    let Ok(root) = cloudfile_core::storage::root() else {
        return;
    };
    let path = root.join("desktop-launch.log");
    let too_big = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > 1024 * 1024);
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true);
    if too_big {
        options.truncate(true);
    } else {
        options.append(true);
    }
    if let Ok(mut file) = options.open(&path) {
        use std::io::Write;
        let _ = writeln!(file, "{message}");
    }
}

/// 往升级日志里写一行分段标题。超过 1MB 先清空，免得这个文件跟着每次升级无限长下去。
fn note_update_log(path: &std::path::Path, line: &str) {
    let too_big = std::fs::metadata(path).is_ok_and(|meta| meta.len() > 1024 * 1024);
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true);
    if too_big {
        options.truncate(true);
    } else {
        options.append(true);
    }
    if let Ok(mut file) = options.open(path) {
        use std::io::Write;
        let _ = writeln!(file, "{line}");
    }
}

/// Share one alpha mark with the desktop UI. macOS templates follow the system
/// menu-bar appearance; other platforms retain the Agent blue for visibility.
fn tray_icon() -> tauri::Result<tauri::image::Image<'static>> {
    let icon = tauri::image::Image::from_bytes(include_bytes!("../ui/cloudfile-mark.png"))?;
    if cfg!(target_os = "macos") {
        return Ok(icon);
    }
    let mut rgba = icon.rgba().to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[..3].copy_from_slice(&[30, 118, 238]);
    }
    Ok(tauri::image::Image::new_owned(
        rgba,
        icon.width(),
        icon.height(),
    ))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The upgrade health check starts a freshly staged build and reads its
    // stdout, so these answers must not depend on a window, the private store
    // or the single-instance lock being free.
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("{}", cloudfile_core::VERSION);
            return;
        }
        Some("--component") => {
            let value = json!({
                "version": cloudfile_core::VERSION,
                "ipc_version": update::IPC_VERSION,
                "journal_version": update::JOURNAL_VERSION,
                "agent_family": "agent2",
                "platform": update::platform_id(std::env::consts::OS, std::env::consts::ARCH),
                "role": "desktop",
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_default()
            );
            return;
        }
        Some("--self-test") => {
            // 自检必须在**建窗口之前**结束：它要在装坏了的机器上也能给出结论。
            std::process::exit(self_test());
        }
        // 诊断用：两个运行时探测入口在任何 webview 存在之前各自给什么答案。
        // 2026-11-03 真机验证时靠它定位「有运行时却被判成缺失、于是只出托盘」。
        Some("--probe-runtime") => {
            println!("tauri::webview_version() = {:?}", tauri::webview_version());
            println!(
                "apps::webview2_version()  = {:?}",
                cloudfile_core::apps::webview2_version()
            );
            println!("runtime_missing()         = {}", runtime_missing());
            return;
        }
        Some("--help") | Some("-h") => {
            print!("{USAGE}");
            return;
        }
        Some(argument) if argument.starts_with('-') && !is_launch_flag(argument) => {
            // Guessing at an unknown flag would start a second resident agent.
            eprintln!("unknown argument {argument}\n\n{USAGE}");
            record_launch_failure(&format!("refused to start: unknown argument {argument}"));
            std::process::exit(2);
        }
        _ => {}
    }
    let autostart = args.iter().any(|value| value == "--autostart");
    // 升级重启专用：老进程还在退出、桌面锁还握着，新实例要等一会儿而不是直接放弃。
    let await_instance = args.iter().any(|value| value == "--await-instance");
    if let Err(error) = run(autostart, await_instance) {
        eprintln!("CloudFile desktop: {error}");
        record_launch_failure(&format!("startup failed: {error:#}"));
    }
}

/// 打印自检报告并给出退出码（0 = 全绿，1 = 有项确实不工作，2 = 连自己的目录都定位不到）。
fn self_test() -> i32 {
    let root = match cloudfile_core::storage::root() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("CloudFile desktop: 无法定位本机私有目录：{error:#}");
            return 2;
        }
    };
    // 搭档是 native host：桌面进程缺了它就注册不上 Chrome，等于半个安装。
    let peer = host_binary().unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .unwrap_or_default()
            .join(HOST_NAME)
    });
    let report = cloudfile_core::selftest::run(&root, &peer);
    println!(
        "{}",
        serde_json::to_string_pretty(&report).unwrap_or_default()
    );
    i32::from(report["ok"] != json!(true))
}

fn run(autostart: bool, await_instance: bool) -> anyhow::Result<()> {
    let root = storage::root()?;
    // 修改逻辑（2026-10-26）：启动时就把用户要用的两个目录准备好。
    //  1) 内置受信任服务（部署方通过 CLOUDFILE_TRUSTED_ORIGINS 下发，只在首次初始化时写入）；
    //  2) 工作副本目录 —— 以前要等到第一次打开文件才创建，用户点「打开工作目录」时是空的/不存在。
    if let Ok(added) = session::ensure_default_origins(&root) {
        if !added.is_empty() {
            eprintln!(
                "CloudFile desktop: seeded trusted origins: {}",
                added.join(", ")
            );
        }
    }
    let _ = settings::ensure_workspace(&root);
    // 修改原因（2026-11-02 现场）：任务中心窗口由 WebView2 渲染。缺运行时的机器上，
    // 以前 Tauri 会在启动阶段自动建窗口，然后弹一句英文原生错误框
    // （Could not find the WebView2 Runtime.），用户既看不懂也不知道该装什么。
    // 现在先探测，再决定建不建窗口（配置里三个窗口都是 `create:false`，见 `create_window`）：
    // 缺失时**一个 webview 都不建**，改为中文说明 + 托盘常驻 + 后台同步继续跑。
    // 探测只对 Windows 成立（macOS 的 WebKit 探测失败不代表建不出窗口，见 `runtime_missing`）。
    let mut missing_now = runtime_missing();
    // 随包的 WebView2 安装器：发行包里 `webview2/` 目录携带（在线引导器或内网离线包都行），
    // 以当前用户身份安装即可，不需要管理员 —— 这是「用户没权限」场景的正解。
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let install_root = update::install_root(&desktop_binary());
    let installer = match cloudfile_core::webview2_setup::locate(&exe_dir, Some(&install_root)) {
        Ok(path) => Some(path),
        Err(error) => {
            if missing_now {
                // 只有缺运行时才值得记：macOS 上永远找不到 Windows 安装器，那是正常的。
                record_launch_failure(&format!("no usable webview2 installer: {error:#}"));
            }
            None
        }
    };
    let lock_path = root.join("desktop.lock");
    let _lock = match storage::lock(&lock_path) {
        Ok(lock) => lock,
        // 修改原因（2026-10-23）：升级后点「立即重启」时，新实例是被老进程拉起来的，
        // 而老进程要等 `app.exit(0)` 走完才释放 desktop.lock。原来这里一失败就 return，
        // 新实例直接消失 —— 用户看到的就是「点了重启没反应」。等待模式下最多等 10 秒。
        Err(_) if await_instance => {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(200));
                match storage::lock(&lock_path) {
                    Ok(lock) => break lock,
                    Err(_) if std::time::Instant::now() < deadline => continue,
                    Err(_) => {
                        // 已有实例在跑。缺运行时的话，再双击只会「什么都没发生」，
                        // 所以补一次说明；有随包安装器时直接引导装（装完仍需重启旧进程）。
                        if missing_now && !autostart {
                            match installer.as_deref() {
                                Some(installer) => {
                                    let _ = offer_runtime_install(installer);
                                }
                                None => {
                                    runtime_notice();
                                }
                            }
                        }
                        return Ok(());
                    }
                }
            }
        }
        // A second launch only needs to raise the window the first one owns.
        Err(_) => {
            if missing_now && !autostart {
                match installer.as_deref() {
                    Some(installer) => {
                        let _ = offer_runtime_install(installer);
                    }
                    None => {
                        runtime_notice();
                    }
                }
            }
            return Ok(());
        }
    };
    // 分离进程的 stderr 是空的：缺运行时留一行痕，现场排查直接看这个文件。
    // 放在拿到实例锁之后：第二实例不该把日志刷成重复行。
    if missing_now {
        record_launch_failure("webview runtime missing: task centre window disabled");
    }
    ipc::recover(&root)?;
    // 注册自愈（2026-11-02）：注册表键被清理工具删掉、或某次注册表写入失败时，
    // 扩展只会看到「Agent 未就绪」，用户唯一的出路是重跑 install.cmd ——
    // 而任务中心每次登录都会启动，正好是把它补回来的地方。
    // 只修「manifest 还指着本机程序、键却没了/指错」这一种：主动卸载会把 manifest
    // 一起删掉，所以那种情况这里什么都不做（见 hostreg::repair_registration）。
    if let (Ok(dir), Some(binary)) = (hostreg::manifest_dir(), host_binary()) {
        match hostreg::repair_registration(&dir, hostreg::HOST, &binary) {
            Ok(Some(note)) => eprintln!("CloudFile desktop: {note}"),
            Ok(None) => {}
            Err(error) => {
                eprintln!("CloudFile desktop: registration self-heal failed: {error:#}")
            }
        }
    }
    // 手动双击启动：先给一次「免管理员自助安装」的机会 —— 装好就能在**同一次启动**里
    // 直接建窗口，用户不用再退出重开。登录项启动（--autostart）不弹框：每次登录弹一次
    // 没人受得了；托盘菜单里有安装入口，用户随时能点。
    let mut installer = installer;
    if missing_now && !autostart {
        // 无论下面装没装成，自动入口都不再重复弹说明（用户已经明确看到过了）。
        RUNTIME_NOTICE_SHOWN.store(true, Ordering::SeqCst);
        // 本地没有随包安装器时，问一句要不要从内网通道取（内网部署的常态路径）。
        // 只在手动双击时做：登录自启每次拉几百 MB 没人受得了。
        if installer.is_none() {
            installer = fetch_installer(&root, &install_root);
        }
        if let Some(installer) = installer.as_deref() {
            match offer_runtime_install(installer) {
                Ok(true) => {
                    missing_now = runtime_missing();
                    if missing_now {
                        cloudfile_core::winbox::notify(
                            "WebView2 运行时已安装",
                            "运行时已安装，但系统还没就绪。请稍后从托盘菜单「退出」Agent 后重新打开任务中心。",
                        );
                    } else {
                        cloudfile_core::winbox::notify(
                            "WebView2 运行时已安装",
                            "任务中心马上打开。",
                        );
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    record_launch_failure(&format!("webview2 install failed: {error:#}"));
                    cloudfile_core::winbox::notify(
                        "WebView2 运行时安装失败",
                        &format!(
                            "{error:#}\n\n{}",
                            cloudfile_core::webview2::runtime_install_failed_guidance(None)
                        ),
                    );
                }
            }
        } else {
            // 没有随包安装器：退回中文说明 + 官方下载页（部署方应把安装器打进发行包）。
            runtime_notice();
        }
    }
    let uploading = Arc::new(Mutex::new(HashSet::new()));
    let quitting = Arc::new(AtomicBool::new(false));
    // 托盘菜单要不要挂「安装」项，只看启动时有没有可用的随包安装器。
    let installer_available = installer.is_some();
    let state = State {
        root: root.clone(),
        uploading: uploading.clone(),
        quitting: quitting.clone(),
        toast: Arc::new(Mutex::new(0)),
        announced_downloads: Arc::new(Mutex::new(HashSet::new())),
        guard_row: Arc::new(Mutex::new(None)),
        float_moves: Arc::new(Mutex::new(0)),
        windowed: Arc::new(AtomicBool::new(false)),
        runtime_missing: missing_now,
        installer: installer.clone(),
    };
    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            task_summary,
            settings_get,
            settings_set,
            app_brand,
            app_profile,
            set_profile_environment,
            sync_app_profile,
            float_sync,
            open_task_center,
            toast_show,
            toast_hide,
            exit_assessment,
            quit_app,
            register_host,
            unregister_host,
            rollback_host,
            setup_state,
            setup_apply,
            setup_finish,
            open_extension_dir,
            update_extension,
            autostart_set,
            check_update,
            run_update,
            update_state,
            restart_app,
            open_workspace,
            trusted_origins,
            open_task,
            delete_task,
            resolve_conflict,
            save_cloud,
            recover_upload,
            begin_pairing,
            service_status,
            commit_task,
            recover_commit,
            retry_commit,
            renew_checkout,
            release_checkout
        ])
        .setup(move |app| {
            use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
            use tauri::tray::TrayIconBuilder;
            // 修改逻辑（2026-11-02）：窗口改由这里创建（`tauri.conf.json` 里三个窗口都是
            // `create:false`）。缺 WebView2 运行时的机器上一个 webview 都不建 —— 建了也只会
            // 得到 Tauri 自己的英文原生错误框，用户看不懂、也不知道该装什么。
            // 只在 setup 里建窗：Windows 上在同步命令/事件处理器里建窗有死锁风险（wry #583）。
            if !missing_now {
                for label in ["main", "float", "toast"] {
                    let _ = create_window(app.handle(), label);
                }
            }
            let windowed = app.get_webview_window("main").is_some();
            app.state::<State>()
                .windowed
                .store(windowed, Ordering::SeqCst);
            let open = MenuItem::with_id(app, "open", "打开任务中心", true, None::<&str>)?;
            let refresh = MenuItem::with_id(app, "refresh", "刷新任务", true, None::<&str>)?;
            let guard_row = MenuItem::with_id(app, "guard", "状态：检查中", false, None::<&str>)?;
            if let Ok(mut slot) = app.state::<State>().guard_row.lock() {
                *slot = Some(guard_row.clone());
            }
            let separator = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            // 确属缺运行时：托盘菜单里直接给出「装到当前用户」的入口（不需要管理员）。
            // 没有随包安装器时退回「查看安装方法」（官方下载页）。
            // 运行时在、但建窗失败时不挂这一项：那是另一种故障，写「缺运行时」会误导。
            let runtime_hint = if !missing_now {
                None
            } else if installer_available {
                Some(MenuItem::with_id(
                    app,
                    "runtime-install",
                    "安装 WebView2 运行时（无需管理员）",
                    true,
                    None::<&str>,
                )?)
            } else {
                Some(MenuItem::with_id(
                    app,
                    "runtime",
                    "缺少 WebView2 运行时（点击查看安装方法）",
                    true,
                    None::<&str>,
                )?)
            };
            let mut items: Vec<&dyn IsMenuItem<tauri::Wry>> = vec![&open, &refresh, &guard_row];
            if let Some(hint) = &runtime_hint {
                items.push(hint);
            }
            items.push(&separator);
            items.push(&quit);
            let menu = Menu::with_items(app, &items)?;
            let icon = tray_icon()?;
            // The window doubles as the fallback entry when no tray is allowed.
            let tray = TrayIconBuilder::with_id("main")
                .icon(icon)
                .icon_as_template(cfg!(target_os = "macos"))
                .tooltip(tray_tooltip(
                    windowed,
                    missing_now,
                    &active_brand(&root),
                    "可以安全退出",
                ))
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    // 用户显式点击：每次都反馈，不能因为「之前提示过」而点了没反应。
                    "open" => show_explicitly(app),
                    // 托盘是缺运行时唯一的可见入口：先引导免管理员自助安装；
                    // 装好后必须重启进程（窗口只在启动时创建），装不上再给手动说明。
                    "runtime-install" => {
                        if let Some(installer) = app.state::<State>().installer.clone() {
                            match offer_runtime_install(&installer) {
                                Ok(true) => cloudfile_core::winbox::notify(
                                    "WebView2 运行时已安装",
                                    "请从托盘菜单「退出」Agent，再重新打开任务中心。",
                                ),
                                Ok(false) => {}
                                Err(error) => {
                                    record_launch_failure(&format!(
                                        "webview2 install failed: {error:#}"
                                    ));
                                    cloudfile_core::winbox::notify(
                                        "WebView2 运行时安装失败",
                                        &format!(
                                            "{error:#}\n\n{}",
                                            cloudfile_core::webview2::runtime_install_failed_guidance(None)
                                        ),
                                    );
                                }
                            }
                        }
                    }
                    // 没有随包安装器时的兜底：官方下载页 + 手动步骤。
                    // 如果用户在这期间已经把运行时装好，就别再说「缺运行时」了 ——
                    // 改为告诉他重启进程（窗口只在启动时创建）。
                    "runtime" => {
                        if runtime_missing() {
                            RUNTIME_NOTICE_SHOWN.store(true, Ordering::SeqCst);
                            runtime_notice();
                        } else {
                            cloudfile_core::winbox::notify(
                                "WebView2 运行时已就绪",
                                "已检测到 WebView2 运行时。请从托盘菜单「退出」Agent 后重新打开任务中心。",
                            );
                        }
                    }
                    "refresh" => {
                        let _ = app.emit("tasks-changed", ());
                        show(app);
                    }
                    "quit" => {
                        let state = app.state::<State>();
                        let assessment = guard::assess(&state.root);
                        if assessment.safe_to_exit {
                            state.quitting.store(true, Ordering::SeqCst);
                            app.exit(0);
                        } else if state.windowed.load(Ordering::SeqCst) {
                            // Ask before stopping renewal; never stop silently.
                            show(app);
                            let _ = app.emit(
                                "confirm-exit",
                                serde_json::to_value(&assessment).unwrap_or(Value::Null),
                            );
                        } else if cloudfile_core::winbox::ask(
                            "退出前请确认",
                            &exit_risk_text(&assessment),
                        ) {
                            // 没有窗口可以问（缺运行时）：用原生中文对话框把退出风险说清，
                            // 否则托盘「退出」在有独占任务时等于点了没反应。
                            state.quitting.store(true, Ordering::SeqCst);
                            app.exit(0);
                        }
                    }
                    _ => (),
                })
                .build(app);
            let has_tray = tray.is_ok();
            if !has_tray && !windowed {
                // 既没有窗口也没有托盘：用户没有任何入口（看不到状态、也退不出去）。
                // 不能主动退出——后台同步与浏览器打开文件仍靠这个进程；但必须留痕，
                // 否则现场只会看到「装了却什么都没有」。
                record_launch_failure("no tray and no window: the agent is invisible to the user");
            }
            if let Some(window) = app.get_webview_window("main") {
                if autostart && has_tray {
                    // Started by the login item: stay resident and reachable from the
                    // tray instead of opening a window on every login. Without a tray
                    // there would be no way back, so the window is kept.
                    let _ = window.hide();
                }
                let handle = app.handle().clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        if has_tray {
                            api.prevent_close();
                            if let Some(window) = handle.get_webview_window("main") {
                                let _ = window.hide();
                            }
                        }
                    }
                });
            }
            for label in ["float", "toast"] {
                if let Some(extra) = app.get_webview_window(label) {
                    let _ = extra.set_always_on_top(true);
                    if label == "toast" {
                        let _ = extra.set_ignore_cursor_events(true);
                    }
                }
            }
            watch_float_drag(app.handle());
            let handle = app.handle().clone();
            let auto_root = root.clone();
            let auto_handle = handle.clone();
            let auto_notice = handle.clone();
            std::thread::spawn(move || {
                let mut last_renew = std::time::Instant::now();
                let mut reauth_warned = HashSet::<(String, i64)>::new();
                loop {
                    // Only the resident desktop renews Guards; browser lifetime
                    // and popup windows must never drive a write lease.
                    if last_renew.elapsed() >= std::time::Duration::from_secs(240) {
                        if let Ok(tasks) = session::tasks(&auto_root) {
                            for task in tasks {
                                let open = task.mode == "exclusive-edit"
                                    && cloudfile_core::v2_edit::has_checkout(&auto_root, &task.task_id)
                                    && !journal::closed(&auto_root, &task.task_id).unwrap_or(true);
                                if open && exclusive::renew_checkout(&auto_root, &task.task_id).is_err() {
                                    let _ = auto_handle.emit("tasks-changed", ());
                                }
                                // The browser ticket cannot be refreshed silently; ask
                                // once per ticket to reopen the file from the web.
                                if let Some(expires) = cloudfile_core::v2_edit::ticket_expires_at(&auto_root, &task.task_id) {
                                    let now = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_secs() as i64)
                                        .unwrap_or(0);
                                    if open
                                        && expires - now < 600
                                        && reauth_warned.insert((task.task_id.clone(), expires))
                                        && settings::load(&auto_root).unwrap_or_default().notify_on_review
                                    {
                                        present_toast(
                                            &auto_notice,
                                            "warning",
                                            &format!("{}：编辑授权即将到期，请在网页中再次打开此文件以继续独占编辑", task.file_name),
                                        );
                                        let _ = auto_handle.emit("tasks-changed", ());
                                    }
                                }
                            }
                        }
                        last_renew = std::time::Instant::now();
                    }
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            });
            // 启动后自检一次更新：有新版就轻提示一下（不弹窗、不打断），
            // 具体升级仍由用户在「设置 → 检查更新」里决定。
            let update_handle = handle.clone();
            let update_root = root.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(5));
                let probe = cloudfile_core::native::check_update(&update_root);
                let available = probe
                    .get("update_available")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let latest = probe
                    .get("latest_version")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if available && !latest.is_empty() {
                    present_toast(
                        &update_handle,
                        "info",
                        &format!("发现新版本 v{latest}：可在设置里点「检查更新」升级"),
                    );
                    let _ = update_handle.emit("update-available", latest);
                }
            });
            let guard_handle = handle.clone();
            // 应用信息（品牌名）自动刷新：跟启动自检一样是「后台顺手做」，
            // 失败只落日志，不影响任务与文件操作。
            spawn_profile_refresh(handle.clone(), root.clone());
            // 「正在下载」要尽早看见：界面转的是 10 秒一次的全量刷新，等它太慢。
            // 这里 400ms 看一眼任务目录里的标记，出现就立刻弹提示。
            let download_handle = handle.clone();
            let mut last_signature = String::new();
            let mut first = true;
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(400));
                let state = download_handle.state::<State>();
                // 任务落盘/消失都通知界面刷新：否则文件都打开了，列表里还要等好几秒。
                let signature = task_signature(&state.root);
                if signature != last_signature || first {
                    last_signature = signature;
                    first = false;
                    let _ = download_handle.emit("tasks-changed", ());
                }
                let files = preparing_files(&state.root);
                let live: HashSet<String> = files.iter().map(|(key, _)| key.clone()).collect();
                let mut announce: Vec<String> = Vec::new();
                if let Ok(mut seen) = state.announced_downloads.lock() {
                    // 已经消失的标记要清掉，否则集合只增不减。
                    seen.retain(|key| live.contains(key));
                    for (key, name) in &files {
                        if seen.insert(key.clone()) {
                            announce.push(name.clone());
                        }
                    }
                }
                for name in announce {
                    present_toast(
                        &download_handle,
                        "working",
                        &format!("正在下载「{name}」到本机…大文件需要一点时间，请稍候。"),
                    );
                }
            });
            // 首次安装时把扩展自动解压到位：不该让用户先点「一键完成」才拿到扩展包。
            // 已经有扩展（升级/日常启动）就什么都不做，避免每次启动都去问通道。
            let extension_root = root.clone();
            std::thread::spawn(move || {
                let dir = extension_dir(&extension_root);
                if cloudfile_core::extension::installed_version(&dir).is_some() {
                    return;
                }
                // 失败不能静默：引导页第 5 步会一直是"未完成"，没有日志就查不出原因。
                let line = match cloudfile_core::native::update_extension(&extension_root) {
                    Ok(detail) => format!("ok {detail}"),
                    Err(error) => format!("failed {error:#}"),
                };
                log_profile_refresh(&extension_root, &format!("extension {line}"));
            });
            std::thread::spawn(move || loop {
                let state = guard_handle.state::<State>();
                refresh_tray(&state, &guard_handle);
                std::thread::sleep(std::time::Duration::from_secs(5));
            });
            std::thread::spawn(move || loop {
                let result = ipc::drain(&root, |request| match request {
                    wire::Request::Status {} => native::status(&root),
                    wire::Request::Tasks {} => {
                        summaries(&root, &uploading).unwrap_or_else(|_| wire::error("task storage unavailable"))
                    }
                    wire::Request::CheckUpdate {} => native::check_update(&root),
                    // 升级轮询专用：只读本地文件，不拉更新清单（见 wire::Request::UpgradeState）。
                    wire::Request::UpgradeState {} => native::upgrade_state(&root),
                    wire::Request::QueryLocalFile { repo_id, path, mode } => {
                        native::query_local_file(&root, &repo_id, &path, &mode)
                    }
                    wire::Request::QueryLocalFolder { repo_id, mode } => {
                        native::query_local_folder(&root, &repo_id, &mode)
                    }
                    wire::Request::OpenWorkspace { repo_id, path, mode } => {
                        native::open_workspace(&root, &repo_id, &path, &mode)
                    }
                    wire::Request::PairEnrollment { server } => match device::enrollment(&root, &server) {
                        Ok(value) => json!({"ok":true,"enrollment":value,
                            "service_path":session::service_path(&root,&server).unwrap_or_default()}),
                        Err(error) => wire::error(&error.to_string()),
                    },
                    wire::Request::PairPrepare { server, service_path } => {
                        match device::prepare(&root, &server, &service_path) {
                            Ok(mut value) => {
                                value["ok"] = json!(true);
                                value
                            }
                            Err(error) => wire::error(&error.to_string()),
                        }
                    }
                    wire::Request::PairSign { server, response } => {
                        match device::pair_sign(&root, &server, response) {
                            Ok(value) => json!({"ok":true,"proof":value}),
                            Err(error) => wire::error(&error.to_string()),
                        }
                    }
                    wire::Request::PairConfirm { server, response } => match device::confirm(&root, &server, response) {
                        Ok(()) => {
                            show(&handle);
                            json!({"ok":true})
                        }
                        Err(error) => wire::error(&error.to_string()),
                    },
                    wire::Request::OpenUri { uri } if uri.starts_with("seafile:") => {
                        match cloudfile_core::legacy_seafile::dispatch(&root, &uri) {
                            Ok(cloudfile_core::legacy_seafile::Destination::ExistingTask(_)) => {
                                show(&handle);
                                json!({"ok":true,"state":"opened-local-copy"})
                            }
                            Ok(cloudfile_core::legacy_seafile::Destination::BrowserUrl(_)) => {
                                json!({"ok":true,"state":"requires-browser-auth"})
                            }
                            Err(error) => wire::error(&error.to_string()),
                        }
                    },
                    wire::Request::OpenUri { uri } => match paired_session::open(&root, &uri) {
                        Ok(task) => {
                            show(&handle);
                            match session::launch(&root, &task.task_id) {
                                Ok(()) => json!({"ok":true}),
                                // 不吞掉原因：原来固定回一句「未能打开本地应用」，
                                // 到底是缺程序、没关联还是程序起不来，现场完全看不出来。
                                // 具体原因（含扩展名与下一步）由 core 给出，这里原样带出去，
                                // 并且在窗口里弹一条不抢焦点的提示 —— 用户当时就在看任务中心。
                                Err(error) => {
                                    let reason = format!("没能打开本地应用：{error}");
                                    present_toast(&handle, "warning", &reason);
                                    wire::error(&format!("文件已保留，{reason}"))
                                }
                            }
                        }
                        Err(error) => wire::error(&error.to_string()),
                    },
                    // 扩展弹窗点「打开任务中心」：进程已经在跑就直接切前台，
                    // 绝不重复开第二个（单实例锁会让第二个静默退出，用户以为没反应）。
                    wire::Request::OpenTaskCenter {} => {
                        show(&handle);
                        json!({"ok":true})
                    }
                    wire::Request::Focus { task_id } => {
                        if session::load(&root, &task_id).is_ok() {
                            show(&handle);
                            let _ = handle.emit("focus-task", task_id);
                            json!({"ok":true})
                        } else {
                            wire::error("task unavailable")
                        }
                    }
                    wire::Request::Update {} | wire::Request::UpdateExtension {} => {
                        wire::error("请在命令行执行 cloudfile-native-host --update 或 --update-extension")
                    }
                });
                if result.is_err() {
                    eprintln!("CloudFile mailbox operation failed");
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            });
            Ok(())
        })
        .run(tauri::generate_context!())?;
    Ok(())
}

#[cfg(test)]
mod launch_flag_tests {
    use super::is_launch_flag;

    #[test]
    fn handoff_flags_are_accepted_and_strangers_are_not() {
        // 升级重启只传 --await-instance（2026-10-27 起不再带 --autostart），
        // 它必须被当成已知参数，否则新实例会静默 exit(2)。
        assert!(is_launch_flag("--await-instance"));
        assert!(is_launch_flag("--autostart"));
        assert!(!is_launch_flag("--definitely-not-a-flag"));
        assert!(!is_launch_flag("--await"));
    }
}

/// 缺运行时降级后的用户可见文案：三种状态必须在托盘上分得清，
/// 否则现场分不清「该装运行时」还是「该查日志」。
#[cfg(test)]
mod degraded_ux_tests {
    use super::{exit_risk_text, tray_tooltip};

    #[test]
    fn tray_tooltip_tells_apart_no_window_reasons() {
        let normal = tray_tooltip(true, false, "Etech文件助手", "可以安全退出");
        // 托盘悬停就是用户在任务栏上看到的名字，必须跟侧栏一致。
        assert!(normal.starts_with("Etech文件助手 "), "{normal}");
        assert!(!normal.contains("CloudFile"));
        assert!(normal.contains("可以安全退出"));
        assert!(!normal.contains("后台运行"));

        let missing = tray_tooltip(false, true, "Etech文件助手", "可以安全退出");
        assert!(missing.contains("后台运行"));
        assert!(missing.contains("WebView2"));
        assert!(!missing.contains("启动失败"));

        // 运行时在却建不出窗口是另一种故障：不能让用户去装一个已经装好的运行时。
        let failed = tray_tooltip(false, false, "Etech文件助手", "可以安全退出");
        assert!(failed.contains("启动失败"));
        assert!(failed.contains("desktop-launch.log"));
        assert!(!failed.contains("需要 WebView2"));

        // 没有应用配置时退回默认名，不能出现空白的托盘提示。
        let fallback = tray_tooltip(true, false, "   ", "可以安全退出");
        assert!(fallback.starts_with("CloudFile "), "{fallback}");
    }

    #[test]
    fn exit_risk_text_states_both_checkout_and_pending_commit_consequences() {
        let assessment = cloudfile_core::guard::Assessment {
            safe_to_exit: false,
            active_checkouts: 2,
            uncertain: 3,
            risks: Vec::new(),
        };
        let text = exit_risk_text(&assessment);
        assert!(text.contains("2 个独占任务"), "{text}");
        assert!(text.contains("3 项待确认提交"), "{text}");
        assert!(text.contains("停止独占续租"), "{text}");
        // 未确认提交不能只说「工作副本不会删除」：结果未知才是用户要知道的后果。
        assert!(text.contains("重新核对"), "{text}");
    }

    /// 缺运行时的门禁只对 Windows 成立：macOS 的 WebKit 探测失败不代表建不出窗口。
    #[cfg(not(windows))]
    #[test]
    fn runtime_missing_never_blocks_macos() {
        assert!(!super::runtime_missing());
    }
}

#[cfg(test)]
mod tray_tests {
    #[test]
    fn embedded_mark_keeps_a_visible_shape_and_transparent_background() {
        let icon = super::tray_icon().unwrap();
        assert_eq!(icon.width(), icon.height());
        assert_eq!(icon.rgba()[3], 0, "canvas corner must stay transparent");
        let pixels = icon.rgba().chunks_exact(4).collect::<Vec<_>>();
        let opaque = pixels.iter().filter(|pixel| pixel[3] > 240).count();
        assert!(opaque > pixels.len() / 10, "the mark must not be empty");
        assert!(
            opaque < pixels.len() * 3 / 4,
            "do not restore an opaque square"
        );
    }
}
