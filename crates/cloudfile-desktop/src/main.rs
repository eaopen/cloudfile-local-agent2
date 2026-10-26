//! CloudFile Agent 2.2 desktop process.
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
    apps, auto_upload, autostart, browser, device, exclusive, extension, guard, hostreg, ipc,
    journal, native, paired_session, session, settings, storage, update, upload, wire,
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
    /// Live handle to the tray's status row. The tray is the only place the
    /// guard verdict is visible without opening a window, so the row is kept
    /// addressable instead of being rebuilt on every refresh.
    guard_row: Arc<Mutex<Option<tauri::menu::MenuItem<tauri::Wry>>>>,
    /// Coalesces the position writes produced by a single drag: the native drag
    /// emits one `Moved` per frame and only the resting place is worth storing.
    float_moves: Arc<Mutex<u64>>,
}

fn show(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
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
    let busy = uploading
        .lock()
        .map_err(|_| anyhow::anyhow!("上传状态不可用"))?
        .clone();
    for task in session::tasks(root)? {
        let changed = session::local_path(root, &task)
            .and_then(|path| session::digest(&path))
            .map(|digest| digest != task.baseline_digest);
        let pending = journal::pending(root, &task.task_id);
        let automatic = auto_upload::settings(root, &task.task_id);
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
        let needs_review =
            changed.is_err() || pending.is_err() || automatic.is_err() || lease.is_err();
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
        tasks.push(json!({"task_id":task.task_id,"file_name":task.file_name,
            "mode":native::mode_label(&task.mode),"internal_mode":task.mode,
            "service":task.server,"remote_path":task.remote_path,
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
            "auto_upload":automatic.ok(),"auto_upload_master":auto_upload::master_enabled(root),"uploading":is_uploading,
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
            if let Ok(Some(saved)) = settings::position(&state.root, &display) {
                let _ = window
                    .set_position(tauri::LogicalPosition::new(saved.x as f64, saved.y as f64));
            }
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
    if let Some(monitor) = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
    {
        let size = monitor.size();
        let position = monitor.position();
        let width = 360i32;
        let x = position.x + size.width as i32 - width - 16;
        let y = position.y + 16;
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    }
    let _ = window.emit(
        "toast",
        json!({"level": if level == "warning" {"warning"} else {"info"}, "text": text}),
    );
    let _ = window.show();
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(3_500));
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
        "setup_completed": current.setup_completed,
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
            Ok(detail) => json!({"key":"extension","ok":true,"detail":detail}),
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

/// 打开扩展管理页，并把要选的目录一起返回（界面上显示成可复制的只读文本）。
#[tauri::command]
fn open_extension_page(state: tauri::State<'_, State>) -> Result<Value, String> {
    let dir = extension_dir(&state.root);
    if extension::installed_version(&dir).is_none() {
        return Err(format!(
            "扩展还没部署到 {}；请先点「一键完成」，或手动把扩展包解压到该目录",
            dir.display()
        ));
    }
    let chrome = browser::open_extensions_page().map_err(|error| format!("{error:#}"))?;
    Ok(json!({
        "directory": dir.to_string_lossy(),
        "extension_id": hostreg::EXTENSION_ID,
        "chrome": chrome.map(|path| path.to_string_lossy().into_owned()),
    }))
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
    let mut command = std::process::Command::new(binary);
    command
        .arg("--update")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // 同 native-host：DETACHED_PROCESS 让升级进程脱离本窗口活下来，并且没有控制台
        // （所以这里不需要 `childproc::hide`）。
        command.creation_flags(0x0000_0008);
    }
    command
        .spawn()
        .map_err(|_| "无法启动更新进程".to_string())?;
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
    command
        .arg("--autostart")
        .arg("--await-instance")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0008);
    }
    command.spawn().map_err(|_| "无法启动新版本".to_string())?;
    app.exit(0);
    Ok(json!({"state": "restarting", "version": version}))
}

#[tauri::command]
async fn save_cloud(state: tauri::State<'_, State>, task_id: String) -> Result<Value, String> {
    let root = state.root.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let result = upload::save(&root, &task_id).map_err(|error| error.to_string())?;
        if result["state"] == "published" {
            let _ = auto_upload::confirmed(&root, &task_id);
        }
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
        if result["state"] == "published" {
            let _ = auto_upload::confirmed(&root, &task_id);
        }
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

/// 移除一条受信任服务，
/// 这里只是把拒绝原因原样带回界面。
#[tauri::command]
fn untrust_origin(state: tauri::State<'_, State>, origin: String) -> Result<(), String> {
    session::untrust(&state.root, origin).map_err(|error| error.to_string())
}

/// 单任务开关：关掉/恢复某个文件的自动上传。总开关（settings.json 的 auto_upload）
/// 关闭时界面不给入口，core 也会拒绝打开。
#[tauri::command]
fn set_auto_upload(
    state: tauri::State<'_, State>,
    task_id: String,
    enabled: bool,
) -> Result<(), String> {
    auto_upload::set_enabled(&state.root, &task_id, enabled).map_err(|error| error.to_string())
}

#[tauri::command]
fn trust_origin(state: tauri::State<'_, State>, origin: String) -> Result<(), String> {
    session::trust(&state.root, origin)
        .map_err(|_| "请输入完整服务地址，例如 https://files.example.com".into())
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
        let _ = tray.set_tooltip(Some(format!("CloudFile Agent 2.2 · {summary}")));
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
CloudFile Agent 2.2 desktop

Usage: cloudfile-desktop [--self-test] [--autostart]

  --version     print the build version
  --component   print the local component contract as JSON
  --self-test   check the local installation and exit (JSON, non-zero when broken);
                never opens a window, so it is safe in automation
  --help, -h    print this message
  --autostart   start resident in the tray without opening the window; the login
                item passes this so logging in never steals focus
";

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
        Some("--help") | Some("-h") => {
            print!("{USAGE}");
            return;
        }
        Some(argument) if argument.starts_with('-') && argument != "--autostart" => {
            // Guessing at an unknown flag would start a second resident agent.
            eprintln!("unknown argument {argument}\n\n{USAGE}");
            std::process::exit(2);
        }
        _ => {}
    }
    let autostart = args.iter().any(|value| value == "--autostart");
    // 升级重启专用：老进程还在退出、桌面锁还握着，新实例要等一会儿而不是直接放弃。
    let await_instance = args.iter().any(|value| value == "--await-instance");
    if let Err(error) = run(autostart, await_instance) {
        eprintln!("CloudFile desktop: {error}");
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
                    Err(_) => return Ok(()),
                }
            }
        }
        // A second launch only needs to raise the window the first one owns.
        Err(_) => return Ok(()),
    };
    ipc::recover(&root)?;
    let uploading = Arc::new(Mutex::new(HashSet::new()));
    let quitting = Arc::new(AtomicBool::new(false));
    let state = State {
        root: root.clone(),
        uploading: uploading.clone(),
        quitting: quitting.clone(),
        toast: Arc::new(Mutex::new(0)),
        guard_row: Arc::new(Mutex::new(None)),
        float_moves: Arc::new(Mutex::new(0)),
    };
    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            task_summary,
            settings_get,
            settings_set,
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
            open_extension_page,
            open_extension_dir,
            update_extension,
            untrust_origin,
            set_auto_upload,
            autostart_set,
            check_update,
            run_update,
            update_state,
            restart_app,
            open_workspace,
            trust_origin,
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
            use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
            use tauri::tray::TrayIconBuilder;
            let open = MenuItem::with_id(app, "open", "打开任务中心", true, None::<&str>)?;
            let refresh = MenuItem::with_id(app, "refresh", "刷新任务", true, None::<&str>)?;
            let guard_row = MenuItem::with_id(app, "guard", "状态：检查中", false, None::<&str>)?;
            if let Ok(mut slot) = app.state::<State>().guard_row.lock() {
                *slot = Some(guard_row.clone());
            }
            let separator = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &refresh, &guard_row, &separator, &quit])?;
            let icon = tray_icon()?;
            // The window doubles as the fallback entry when no tray is allowed.
            let tray = TrayIconBuilder::with_id("main")
                .icon(icon)
                .icon_as_template(cfg!(target_os = "macos"))
                .tooltip("CloudFile Agent 2.2")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show(app),
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
                        } else {
                            // Ask before stopping renewal; never stop silently.
                            show(app);
                            let _ = app.emit(
                                "confirm-exit",
                                serde_json::to_value(&assessment).unwrap_or(Value::Null),
                            );
                        }
                    }
                    _ => (),
                })
                .build(app);
            let has_tray = tray.is_ok();
            let window = app
                .get_webview_window("main")
                .ok_or_else(|| anyhow::anyhow!("main window missing"))?;
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
            let auto_busy = uploading.clone();
            let auto_notice = handle.clone();
            std::thread::spawn(move || {
                let started = std::time::Instant::now();
                let mut observer = auto_upload::Observer::default();
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
                    let notices = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
                    let writer = notices.clone();
                    let mut changed = false;
                    let result = observer.scan_with(
                        &auto_root,
                        started.elapsed().as_millis() as u64,
                        |id, digest| {
                            changed = true;
                            if let Ok(mut busy) = auto_busy.lock() {
                                busy.insert(id.to_string());
                            }
                            let _ = auto_handle.emit("tasks-changed", ());
                            let result = upload::save_observed(&auto_root, id, digest);
                            if let Ok(value) = &result {
                                let name = session::load(&auto_root, id)
                                    .map(|task| task.file_name)
                                    .unwrap_or_default();
                                // A cloud save is only ever announced after the
                                // server confirmed it.
                                let notice = match value["state"].as_str() {
                                    Some("published") => {
                                        ("info".to_string(), format!("{name}：已保存到云端"))
                                    }
                                    Some("unchanged") => {
                                        ("info".to_string(), format!("{name}：与云端基线一致"))
                                    }
                                    _ => (
                                        "warning".to_string(),
                                        format!("{name}：上传结果未确认，请在任务中心检查"),
                                    ),
                                };
                                if let Ok(mut list) = writer.lock() {
                                    list.push(notice);
                                }
                            }
                            if let Ok(mut busy) = auto_busy.lock() {
                                busy.remove(id);
                            }
                            result
                        },
                    );
                    if result.is_err() {
                        eprintln!("CloudFile automatic upload storage requires review");
                        let _ = auto_handle.emit("tasks-changed", ());
                    }
                    if changed {
                        let _ = auto_handle.emit("tasks-changed", ());
                    }
                    let preferences = settings::load(&auto_root).unwrap_or_default();
                    if let Ok(list) = notices.lock() {
                        for (level, text) in list.iter() {
                            let allowed = if level == "warning" {
                                preferences.notify_on_review
                            } else {
                                preferences.notify_on_saved
                            };
                            if allowed {
                                present_toast(&auto_notice, level, text);
                            }
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            });
            let guard_handle = handle.clone();
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
                                Err(_) => wire::error("文件已保留，未能打开本地应用，请在任务中心查看"),
                            }
                        }
                        Err(error) => wire::error(&error.to_string()),
                    },
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
