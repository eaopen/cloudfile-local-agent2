//! Local application detection and user-defined open rules.
//!
//! Selecting an application is a local convenience only. It never decides which
//! server call runs and never changes what gets uploaded. The reference Windows
//! and Go agents already shipped these rules; this module ports the same intent
//! to one platform-adapted table instead of inheriting the historical GUI.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PLACEHOLDER: &str = "{file}";
const MAX_RULES: usize = 32;

/// An application the agent can hand a working copy to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Application {
    pub id: String,
    pub name: String,
    pub extensions: Vec<String>,
    /// Executable template. Exactly one argument is the `{file}` placeholder.
    pub command: Vec<String>,
    /// Whether the executable was actually found on this machine.
    pub available: bool,
}

/// A user rule always wins over detection and over the system default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRule {
    pub id: String,
    /// Restricted to the two local session modes so a read-only copy can never
    /// be routed into an editing application by accident.
    pub modes: Vec<String>,
    /// Lowercase extensions, or `*` as a final catch-all.
    pub extensions: Vec<String>,
    pub command: Vec<String>,
}

impl OpenRule {
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty() || self.id.len() > 64 || self.id.chars().any(char::is_control) {
            bail!("open rule requires a short identifier");
        }
        if self.modes.is_empty()
            || self
                .modes
                .iter()
                .any(|mode| !matches!(mode.as_str(), "local-view" | "local-edit"))
        {
            bail!("open rule modes must be local-view or local-edit");
        }
        if self.extensions.is_empty() {
            bail!("open rule requires at least one extension");
        }
        for extension in &self.extensions {
            if !valid_extension(extension) {
                bail!("open rule extension must be lowercase alphanumeric");
            }
        }
        if self.command.is_empty() || self.command.len() > 32 {
            bail!("open rule requires a command");
        }
        let program = &self.command[0];
        if program.contains('\0') || !Path::new(program).is_absolute() {
            bail!("open rule program must be an absolute path");
        }
        let placeholders = self
            .command
            .iter()
            .filter(|argument| argument.contains(PLACEHOLDER))
            .count();
        if placeholders != 1 {
            bail!("open rule command needs exactly one file placeholder");
        }
        Ok(())
    }
    fn matches(&self, mode: &str, name: &str) -> bool {
        let family = mode_family(mode);
        if !self.modes.iter().any(|value| value == family) {
            return false;
        }
        let extension = extension_of(name);
        let mut wildcard = false;
        for candidate in &self.extensions {
            if candidate == "*" {
                wildcard = true;
            } else if *candidate == extension {
                // A specific extension always outranks a later `*`.
                return true;
            }
        }
        wildcard
    }
    fn render(&self, path: &Path) -> Vec<String> {
        let file = path.to_string_lossy().into_owned();
        self.command
            .iter()
            .map(|argument| argument.replace(PLACEHOLDER, &file))
            .collect()
    }
}

/// The Chrome extension and the server use `local-edit`; the agent tracks the
/// same family as `optimistic-edit` because it publishes native updates.
/// `local-view` stays distinct so a read-only copy cannot be routed into an
/// editing rule.
///
/// 这是**模式名的唯一映射**：`native::mode_label` 直接复用它。各写一份的后果已经出现过
/// ——那边把 `local-view` 也报成 `local-edit`，等于告诉网页「这是一份可编辑副本」。
pub fn mode_family(mode: &str) -> &'static str {
    match mode {
        // Checkout changes server authorization and commit lifecycle, not
        // which application is suitable to open the local file.
        "exclusive-edit" | "optimistic-edit" | "local-edit" => "local-edit",
        "local-view" => "local-view",
        _ => "unknown",
    }
}

pub fn extension_of(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default()
}

fn valid_extension(value: &str) -> bool {
    value == "*"
        || (!value.is_empty()
            && value.len() <= 16
            && value
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '+' || c == '-'))
}

/// Types the agent will hand to the OS default handler. Anything else needs an
/// explicit rule or a detected application, so an unexpected type is not
/// silently launched.
///
/// 修改逻辑（2026-11-03）：补上设计类扩展名。现场 `.prt` 打不开，提示只写
/// 「未能打开本地应用」—— 根因就是它不在这张表里，而任务中心又把它当成 CAD 文件
/// 去提示「可能依赖其他参照文件」，两边说法对不上。
///
/// `.prt` 交给 Siemens NX：现场口径（2026-11-03）是**只用 NX、不装 Creo**，而客户机的
/// `.prt` 往往没有任何文件关联（本机 `HKCR\.prt` 就是空的），只靠系统关联必然打不开。
/// 所以先由 `nx_install()` 找到 `ugraf.exe` 直接打开，找不到 NX 时才退回系统关联。
///
/// `.asm` 仍**刻意不绑到某个具体程序**：Creo 与 SolidWorks 都用它，猜错就会拿另一个
/// 软件去开，交给系统关联更安全。
const DEFAULT_OPENABLE: &[&str] = &[
    "txt",
    "md",
    "pdf",
    "doc",
    "docx",
    "xls",
    "xlsx",
    "ppt",
    "pptx",
    "odt",
    "ods",
    "odp",
    "csv",
    "rtf",
    "png",
    "jpg",
    "jpeg",
    "gif",
    "tif",
    "tiff",
    "bmp",
    "dwg",
    "dxf",
    "dwt",
    "step",
    "stp",
    "iges",
    "igs",
    "sldprt",
    "sldasm",
    "slddrw",
    "prt",
    "asm",
    "catpart",
    "catproduct",
    "ipt",
    "iam",
    "idw",
    "ipn",
    "skp",
    "3dm",
    "rvt",
    "rfa",
    "nwd",
    "nwc",
    "nwf",
    "stl",
    "obj",
    "fcstd",
    // 仿真数据文件（2026-11-03 现场）：.dta/.erg 及 *_for_modN/*_from_modN 变体。
    // 只列小写——`extension_of()` 比较前统一下转，`.dta_for_Mod1` 自然命中
    // `dta_for_mod1`，不用把大小写变体再抄一遍。
    "erg",
    "input",
    "dta",
    "out",
    "in",
    "dta_for_mod1",
    "dta_from_mod1",
    "dta_for_mod2",
    "dta_from_mod2",
    "dta_for_mod3",
    "dta_from_mod3",
    "erg_from_mod1",
    "erg_from_mod2",
    "erg_from_mod3",
    // 现场又报的两类（2026-11-04 用户手加）：.aitp 是仿真输入/参数文件，
    // .dat 是各求解器通用的数据文件 —— 两者都没有固定的程序归属，只交给系统关联。
    "aitp",
    "dat",
];

pub fn validate_rules(rules: &[OpenRule]) -> Result<()> {
    if rules.len() > MAX_RULES {
        bail!("too many open rules");
    }
    let mut seen = std::collections::BTreeSet::new();
    for rule in rules {
        rule.validate()?;
        if !seen.insert(rule.id.clone()) {
            bail!("duplicate open rule identifier");
        }
    }
    Ok(())
}

pub fn resolve_open_rule<'a>(
    rules: &'a [OpenRule],
    mode: &str,
    name: &str,
) -> Option<&'a OpenRule> {
    rules.iter().find(|rule| rule.matches(mode, name))
}

pub fn for_file<'a>(apps: &'a [Application], name: &str) -> Option<&'a Application> {
    let extension = extension_of(name);
    if extension.is_empty() {
        return None;
    }
    apps.iter().find(|app| app.extensions.contains(&extension))
}

/// 把 NX 路由的决策写进 `<agent 根>\nx-route.log`（只追加，出错也不影响打开）。
///
/// 只有 Windows 的 NX 会话复用会走到这里，其它平台加不加这个属性都是死代码
/// —— 而 workspace 用 `-D warnings` 编译，死代码会直接让 CI 失败。
#[cfg(windows)]
fn log_nx_route(
    root: &Path,
    ugraf: &Path,
    running: &[crate::nx_router::RunningNx],
    choice: &Result<crate::nx_router::Choice>,
) {
    use std::io::Write;
    let path = root.join("nx-route.log");
    let sessions: Vec<String> = running
        .iter()
        .map(|item| {
            format!(
                "{}({})",
                item.pid,
                item.executable
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "路径不可读".into())
            )
        })
        .collect();
    let verdict = match choice {
        Ok(crate::nx_router::Choice::StartNew) => "StartNew".to_string(),
        Ok(crate::nx_router::Choice::RouteTo(pid)) => format!("RouteTo({pid})"),
        Err(error) => format!("拒绝：{error}"),
    };
    // 与其它日志统一：本地时间字符串，现场不用换算。
    let stamp = crate::local_stamp();
    let line = format!(
        "{stamp}\tdetected={}\trunning=[{}]\tchoice={verdict}\n",
        ugraf.to_string_lossy(),
        sessions.join("; ")
    );
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

/// 子进程的工作目录：`<agent 根>/spawn`。
///
/// 不能用文档目录 —— 那会让"删掉一个文件"变成"删不掉一个空文件夹"。
fn spawn_directory(root: &Path) -> PathBuf {
    let dir = root.join("spawn");
    if !dir.is_dir() {
        let _ = std::fs::create_dir_all(&dir);
    }
    dir
}

#[derive(Debug, Clone)]
pub struct OpenedProcess {
    pub pid: u32,
    pub app: String,
    /// ugs_router.exe exits quickly; the real NX session may keep this file in memory
    /// without a file handle. Require a human deletion confirmation after routing.
    pub routed_nx: bool,
}

/// Rule, then detected application, then the OS default. The first two carry an
/// explicit program chosen by the user or by this table; the default handler is
/// only used for known document types.
pub fn open_path(
    root: &Path,
    mode: &str,
    name: &str,
    path: &Path,
) -> Result<Option<OpenedProcess>> {
    if !path.is_absolute() {
        bail!("working copy path is not absolute");
    }
    // 修改逻辑（2026-11-10）：不要把"文件所在目录"设成子进程的工作目录。
    // Windows 不允许删除某个进程的当前目录 —— 于是删完文件、目录空了，只要程序还开着，
    // 那个空目录就永远删不掉（现场就是这样）。给它一个专用的中转目录当代工位。
    let cwd = spawn_directory(root);
    let rules = crate::settings::load(root)
        .map(|settings| settings.open_rules)
        .unwrap_or_default();
    if let Some(rule) = resolve_open_rule(&rules, mode, name) {
        let mut command = rule.render(path);
        let program = command.remove(0);
        return spawn(&program, &command, Some(&cwd));
    }
    let apps = detect();
    if let Some(app) = for_file(&apps, name) {
        if app.available {
            let text = path.to_string_lossy();
            if app.id == "nx" && !text.is_ascii() && !nx_reads_unicode_paths(&app.command[0]) {
                // 与其让 NX 起来停在空会话、用户以为文件坏了，不如把真实原因说清楚。
                bail!(
                    "这份文件的路径里有中文，NX 7.x 及更早版本不认（NX 8.0 起才支持中文路径）。\
                     请把文件改成英文或数字名字后再试，或者让管理员升级 NX"
                );
            }
            let mut command = app.command.clone();
            let program = command.remove(0);
            let command: Vec<String> = command
                .iter()
                .map(|argument| argument.replace(PLACEHOLDER, &text))
                .collect();
            // The installed Windows NX 7.5 path (ugraf -retrieve:) is proven
            // on customer workstations. Only an already-running, uniquely matched
            // NX may be given a file via the Siemens router; never guess a
            // different NX version, process or Teamcenter session.
            #[cfg(windows)]
            if app.id == "nx" {
                // 路由是"能复用就复用、不能就别乱开"，线上出问题时必须有据可查：
                // 把检测到的安装、正在运行的 NX、最终选择写进 <agent 根>\nx-route.log。
                let running = crate::nx_router::running()?;
                let choice = crate::nx_router::choose(Path::new(&program), &running);
                crate::apps::log_nx_route(root, Path::new(&program), &running, &choice);
                match choice? {
                    crate::nx_router::Choice::StartNew => {
                        return spawn(&program, &command, Some(&cwd));
                    }
                    crate::nx_router::Choice::RouteTo(pid) => {
                        let router = crate::nx_router::router(Path::new(&program)).ok_or_else(|| {
                            anyhow::anyhow!(
                                "NX 已在运行，但安装目录未找到 ugs_router.exe。为了不重复打开 NX，请在当前 NX 窗口中手动打开本地工作副本。"
                            )
                        })?;
                        // Separate arguments: no CMD/PowerShell, no global .prt association.
                        // Router spawn success != confirmation NX displayed the document.
                        spawn(
                            &router.to_string_lossy(),
                            &[
                                "-ug".into(),
                                "-use_file_dir".into(),
                                path.to_string_lossy().into_owned(),
                            ],
                            Some(&cwd),
                        )?;
                        return Ok(Some(OpenedProcess {
                            pid,
                            app: "ugraf.exe".to_string(),
                            routed_nx: true,
                        }));
                    }
                }
            }
            return spawn(&program, &command, Some(&cwd));
        }
    }
    let extension = extension_of(name);
    if !DEFAULT_OPENABLE.contains(&extension.as_str()) {
        // 说清楚「缺什么」和「怎么办」。不指向任务中心的「打开规则」—— 界面上没有这个
        // 入口（只有 config.json 里有），让用户去找一个不存在的地方等于没提示。
        bail!(
            "这台机器上没有能打开 .{extension} 的程序。\
             请先装对应软件后重试；装好后在 Windows「设置 → 应用 → 默认应用」里\
             把 .{extension} 关联到它"
        );
    }
    open::that(path).map(|_| None).map_err(|error| {
        anyhow::anyhow!(
            "这台机器没有为 .{extension} 指定默认程序（{error}）。\
             装好对应软件后，在 Windows「设置 → 应用 → 默认应用」里按文件类型关联一次即可"
        )
    })
}

/// NX 7.x 走 ANSI 文件名接口，**中文路径会静默失败**：进程起来了，但停在空会话
/// （真机实测标题只有 `NX 7.5 - `，没有文件名）。NX 8.0 起才有 UTF-8 文件名支持。
/// 版本只能从安装目录名读（`…\NX 7.5\UGII\ugraf.exe`）；读不出来时按「支持」处理，
/// 宁可让用户看到空会话，也不要误拦一台本来能用的机器。
#[cfg(windows)]
fn nx_reads_unicode_paths(program: &str) -> bool {
    Path::new(program)
        .ancestors()
        .filter_map(|part| part.file_name())
        .filter_map(|name| name.to_str())
        .find(|name| name.starts_with("NX "))
        .and_then(|name| name.trim_start_matches("NX ").trim().parse::<f64>().ok())
        .is_none_or(|version| version >= 8.0)
}

#[cfg(not(windows))]
fn nx_reads_unicode_paths(_program: &str) -> bool {
    true
}

/// `directory` 是新进程的工作目录。老版 CAD 会用当前目录做初始化，而任务中心
/// 继承来的目录未必是它能接受的（真机实测：不设工作目录，NX 直接
/// `NX Initialization Error`；设成部件所在目录就正常）。UNC 路径不能当工作目录
/// —— 用 `\\?\` 前缀的私有路径同理，遇到就保持继承。
/// 返回被启动进程的 PID：调用方要据此判断"这个文件是不是还开在那个程序里"。
/// 有些 CAD（比如 NX）读完文件就把句柄放掉了，操作系统层面看不出占用，
/// 只能靠"我们启动的那个进程还在不在"来判断。
fn spawn(
    program: &str,
    arguments: &[String],
    directory: Option<&Path>,
) -> Result<Option<OpenedProcess>> {
    if program.contains('\0') || arguments.iter().any(|argument| argument.contains('\0')) {
        bail!("invalid application command");
    }
    let mut command = std::process::Command::new(program);
    command
        .args(arguments)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(directory) = directory {
        let text = directory.to_string_lossy();
        if directory.is_absolute() && !text.starts_with("\\\\") {
            command.current_dir(directory);
        }
    }
    let child = command
        .spawn()
        .with_context(|| format!("could not start {program}"))?;
    let exe = std::path::Path::new(program)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string());
    Ok(Some(OpenedProcess {
        pid: child.id(),
        app: exe,
        routed_nx: false,
    }))
}

/// Opens a directory in the platform file manager.
pub fn open_directory(path: &Path) -> Result<()> {
    if !path.is_dir() {
        bail!("directory is not available");
    }
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(windows)]
    let program = "explorer";
    #[cfg(not(any(target_os = "macos", windows)))]
    let program = "xdg-open";
    spawn(program, &[path.to_string_lossy().into_owned()], None).map(|_| ())
}

/// Version of the WebView2 runtime the Windows desktop shell needs, if present.
///
/// Tauri on Windows renders through WebView2. It ships with current Windows and
/// alongside Edge, but not on every image, so the agent reports its absence
/// rather than bundling a large offline runtime — that would defeat the goal of
/// a small, dependency-free package. `None` means the check does not apply: on
/// macOS the system WebKit is always there.
pub fn webview2_version() -> Option<String> {
    #[cfg(windows)]
    {
        // Edge registers the runtime per machine and per user; either satisfies
        // the shell, so the machine-wide key is only preferred, not required.
        // 进程内读注册表：以前起 `reg.exe query`，父进程没有控制台时会闪黑窗口。
        const CLIENT: &str = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
        let keys = [
            format!("HKLM\\SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients\\{CLIENT}"),
            format!("HKCU\\Software\\Microsoft\\EdgeUpdate\\Clients\\{CLIENT}"),
        ];
        for key in keys {
            if let Some(version) = crate::winreg::read_string(&key, "pv") {
                if !version.is_empty() {
                    return Some(version);
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    pub struct Spec {
        pub id: &'static str,
        pub name: &'static str,
        pub extensions: &'static [&'static str],
        /// Bundle names, most preferred first. Locations are resolved per user.
        pub bundles: &'static [&'static str],
    }

    pub const SPECS: &[Spec] = &[
        Spec {
            id: "word",
            name: "Microsoft Word",
            extensions: &["doc", "docx", "dot", "dotx", "rtf"],
            bundles: &["Microsoft Word.app"],
        },
        Spec {
            id: "excel",
            name: "Microsoft Excel",
            extensions: &["xls", "xlsx", "xlsm", "xlsb", "csv"],
            bundles: &["Microsoft Excel.app"],
        },
        Spec {
            id: "powerpoint",
            name: "Microsoft PowerPoint",
            extensions: &["ppt", "pptx", "pptm", "ppsx"],
            bundles: &["Microsoft PowerPoint.app"],
        },
        Spec {
            id: "libreoffice",
            name: "LibreOffice",
            extensions: &[
                "odt", "ods", "odp", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
            ],
            bundles: &["LibreOffice.app"],
        },
        Spec {
            id: "autocad",
            name: "AutoCAD",
            extensions: &["dwg", "dxf", "dwt"],
            bundles: &[
                "Autodesk/AutoCAD 2026/AutoCAD.app",
                "Autodesk/AutoCAD 2025/AutoCAD.app",
                "Autodesk/AutoCAD 2024/AutoCAD.app",
            ],
        },
        Spec {
            id: "sketchup",
            name: "SketchUp",
            extensions: &["skp"],
            bundles: &["SketchUp 2024/SketchUp.app", "SketchUp 2023/SketchUp.app"],
        },
        Spec {
            id: "rhino",
            name: "Rhinoceros",
            extensions: &["3dm"],
            bundles: &["Rhino 8.app", "Rhino 7.app"],
        },
        Spec {
            id: "freecad",
            name: "FreeCAD",
            extensions: &["step", "stp", "iges", "igs", "stl", "obj", "fcstd"],
            bundles: &["FreeCAD.app"],
        },
        Spec {
            id: "preview",
            name: "Preview",
            extensions: &["pdf", "png", "jpg", "jpeg", "tif", "tiff", "gif", "bmp"],
            bundles: &[
                "System/Applications/Preview.app",
                "Applications/Preview.app",
            ],
        },
        Spec {
            id: "textedit",
            name: "TextEdit",
            extensions: &["txt", "md"],
            bundles: &[
                "System/Applications/TextEdit.app",
                "Applications/TextEdit.app",
            ],
        },
    ];

    pub fn roots() -> Vec<PathBuf> {
        let mut roots = vec![PathBuf::from("/Applications")];
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join("Applications"));
        }
        roots
    }

    pub fn resolve(bundle: &str) -> Option<PathBuf> {
        for root in roots() {
            let candidate = root.join(bundle);
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
        None
    }

    pub fn command(_spec: &Spec, bundle: &Path) -> Vec<String> {
        vec![
            "/usr/bin/open".into(),
            "-a".into(),
            bundle.to_string_lossy().into_owned(),
            PLACEHOLDER.into(),
        ]
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    pub struct Spec {
        pub id: &'static str,
        pub name: &'static str,
        pub extensions: &'static [&'static str],
        /// Executable name used for `App Paths` lookup and the root scan.
        pub exe: &'static str,
        /// Well known install locations relative to a program root.
        pub fallbacks: &'static [&'static str],
    }

    pub const SPECS: &[Spec] = &[
        Spec {
            id: "word",
            name: "Microsoft Word",
            extensions: &["doc", "docx", "dot", "dotx", "rtf"],
            exe: "WINWORD.EXE",
            fallbacks: &[
                "Microsoft Office/root/Office16/WINWORD.EXE",
                "Microsoft Office/Office16/WINWORD.EXE",
            ],
        },
        Spec {
            id: "excel",
            name: "Microsoft Excel",
            extensions: &["xls", "xlsx", "xlsm", "xlsb", "csv"],
            exe: "EXCEL.EXE",
            fallbacks: &[
                "Microsoft Office/root/Office16/EXCEL.EXE",
                "Microsoft Office/Office16/EXCEL.EXE",
            ],
        },
        Spec {
            id: "powerpoint",
            name: "Microsoft PowerPoint",
            extensions: &["ppt", "pptx", "pptm", "ppsx"],
            exe: "POWERPNT.EXE",
            fallbacks: &[
                "Microsoft Office/root/Office16/POWERPNT.EXE",
                "Microsoft Office/Office16/POWERPNT.EXE",
            ],
        },
        Spec {
            id: "visio",
            name: "Microsoft Visio",
            extensions: &["vsd", "vsdx", "vssx"],
            exe: "VISIO.EXE",
            fallbacks: &["Microsoft Office/root/Office16/VISIO.EXE"],
        },
        Spec {
            id: "libreoffice",
            name: "LibreOffice",
            extensions: &[
                "odt", "ods", "odp", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
            ],
            exe: "soffice.exe",
            fallbacks: &["LibreOffice/program/soffice.exe"],
        },
        Spec {
            id: "autocad",
            name: "AutoCAD",
            extensions: &["dwg", "dxf", "dwt"],
            exe: "acad.exe",
            fallbacks: &[
                "Autodesk/AutoCAD 2026/acad.exe",
                "Autodesk/AutoCAD 2025/acad.exe",
            ],
        },
        Spec {
            id: "revit",
            name: "Revit",
            extensions: &["rvt", "rfa", "rte", "rft"],
            exe: "Revit.exe",
            fallbacks: &[],
        },
        Spec {
            id: "solidworks",
            name: "SOLIDWORKS",
            extensions: &["sldprt", "sldasm", "slddrw"],
            exe: "SLDWORKS.exe",
            fallbacks: &["SOLIDWORKS Corp/SOLIDWORKS/SLDWORKS.exe"],
        },
        Spec {
            id: "sketchup",
            name: "SketchUp",
            extensions: &["skp"],
            exe: "SketchUp.exe",
            fallbacks: &[],
        },
        Spec {
            id: "rhino",
            name: "Rhinoceros",
            extensions: &["3dm"],
            exe: "Rhino.exe",
            fallbacks: &[],
        },
        Spec {
            id: "freecad",
            name: "FreeCAD",
            extensions: &["step", "stp", "iges", "igs", "stl", "obj", "fcstd"],
            exe: "FreeCAD.exe",
            fallbacks: &[
                "FreeCAD 1.0/bin/FreeCAD.exe",
                "FreeCAD 0.21/bin/FreeCAD.exe",
            ],
        },
        Spec {
            id: "nx",
            name: "Siemens NX",
            // 现场只有 NX 用 `.prt`；入口由 `nx_install()` 单独定位（见下）。
            extensions: &["prt"],
            exe: "ugraf.exe",
            fallbacks: &[],
        },
        Spec {
            id: "notepad",
            name: "Notepad",
            extensions: &["txt", "md", "csv"],
            exe: "notepad.exe",
            fallbacks: &["Windows/System32/notepad.exe"],
        },
    ];

    pub fn program_roots() -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for key in [
            "ProgramW6432",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "LOCALAPPDATA",
        ] {
            if let Some(value) = std::env::var_os(key) {
                roots.push(PathBuf::from(value));
            }
        }
        roots
    }

    fn registry(exe: &str) -> Option<PathBuf> {
        // App Paths 的默认值就是这个可执行文件的完整路径。
        // 进程内读：以前每个候选应用都要起一次 `reg.exe query`，一轮检测下来就是
        // 一串黑窗口（Go 侧同样的调用点每消息 48 次，是黑窗口的根因）。
        let candidates = [
            format!("HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{exe}"),
            format!("HKLM\\Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{exe}"),
            format!(
                "HKLM\\Software\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{exe}"
            ),
        ];
        for key in candidates {
            if let Some(value) = crate::winreg::read_string(&key, "") {
                let path = PathBuf::from(value);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
        None
    }

    /// NX 的安装根可以落在**任意盘**（本机就是 `D:\Program Files (x86)\UGS\NX 7.5`），
    /// 入口也不叫 `ugraf.exe` 所在的 `App Paths`——所以静态 `fallbacks` 拼不出来。
    /// 各版本的目录布局还不一样：
    /// - NX 7.5 及更早：`<安装根>\UGII\ugraf.exe`
    /// - NX 8 及以后：`<安装根>\NXBIN\ugraf.exe`
    ///
    /// 依次认：安装器写进注册表的环境变量 → 进程环境 → 常见安装根下的 `NX*` 版本目录
    /// （取版本号最大的那个）。全都找不到才返回 `None`，交回系统关联。
    fn nx_install() -> Option<PathBuf> {
        const RELATIVES: [&str; 2] = ["UGII/ugraf.exe", "NXBIN/ugraf.exe"];

        fn under(base: &Path) -> Option<PathBuf> {
            RELATIVES
                .iter()
                .map(|relative| base.join(relative))
                .find(|candidate| candidate.is_file())
        }

        // `UGII_ROOT_DIR` 直接指向 UGII 目录，`UGII_BASE_DIR` 指安装根，两者都试一遍。
        fn from_base(base: &Path) -> Option<PathBuf> {
            under(base).or_else(|| base.parent().and_then(under))
        }

        const ENVIRONMENT: &str =
            r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
        for name in ["UGII_BASE_DIR", "UGII_ROOT_DIR"] {
            if let Some(value) = crate::winreg::read_string(ENVIRONMENT, name) {
                if let Some(found) = from_base(Path::new(&value)) {
                    return Some(found);
                }
            }
        }

        for name in ["UGII_BASE_DIR", "UGII_ROOT_DIR"] {
            if let Some(value) = std::env::var_os(name) {
                if let Some(found) = from_base(Path::new(&value)) {
                    return Some(found);
                }
            }
        }

        for root in program_roots() {
            for family in ["Siemens", "UGS"] {
                let Ok(entries) = std::fs::read_dir(root.join(family)) else {
                    continue;
                };
                let mut versions: Vec<PathBuf> = entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.is_dir()
                            && path
                                .file_name()
                                .is_some_and(|name| name.to_string_lossy().starts_with("NX"))
                    })
                    .collect();
                versions.sort_by(|left, right| {
                    nx_version(left)
                        .partial_cmp(&nx_version(right))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                for directory in versions.iter().rev() {
                    if let Some(found) = under(directory) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    /// `NX 7.5` → 7.5、`NX 2306` → 2306.0；解析不出来时排最前（最后才试）。
    pub(super) fn nx_version(directory: &Path) -> f64 {
        directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
            .trim_start_matches("NX")
            .trim()
            .parse::<f64>()
            .unwrap_or(-1.0)
    }

    pub fn resolve(spec: &Spec) -> Option<PathBuf> {
        if let Some(path) = registry(spec.exe) {
            return Some(path);
        }
        // NX 走专用探测：它既不在 `App Paths`，也不保证装在系统盘。
        if spec.id == "nx" {
            if let Some(path) = nx_install() {
                return Some(path);
            }
        }
        for root in program_roots() {
            for fallback in spec.fallbacks {
                let candidate = root.join(fallback);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    pub fn command(spec: &Spec, path: &Path) -> Vec<String> {
        let program = path.to_string_lossy().into_owned();
        // NX **不认** `ugraf.exe <part.prt>`：那样只会开一个空会话（真机实测标题就是
        // `NX 7.5 - `，没有文件名）。官方开关是 `-retrieve:<part>`，实测标题才出现
        // `NX 7.5 - 基本环境 - [arrow.prt]`。
        if spec.id == "nx" {
            return vec![program, format!("-retrieve:{PLACEHOLDER}")];
        }
        vec![program, PLACEHOLDER.into()]
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::*;

    pub struct Spec {
        pub id: &'static str,
        pub name: &'static str,
        pub extensions: &'static [&'static str],
        pub programs: &'static [&'static str],
    }

    pub const SPECS: &[Spec] = &[
        Spec {
            id: "libreoffice",
            name: "LibreOffice",
            extensions: &[
                "odt", "ods", "odp", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
            ],
            programs: &["soffice", "libreoffice"],
        },
        Spec {
            id: "freecad",
            name: "FreeCAD",
            extensions: &["step", "stp", "iges", "igs", "stl", "obj", "fcstd"],
            programs: &["freecad", "FreeCAD"],
        },
        Spec {
            id: "inkscape",
            name: "Inkscape",
            extensions: &["svg"],
            programs: &["inkscape"],
        },
    ];

    pub fn resolve(program: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(program);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    pub fn command(_spec: &Spec, path: &Path) -> Vec<String> {
        vec![path.to_string_lossy().into_owned(), PLACEHOLDER.into()]
    }
}

/// Full table with availability resolved once per call. Detection is cheap
/// enough for a status request and must never be cached across an install.
pub fn detect() -> Vec<Application> {
    platform::SPECS
        .iter()
        .map(|spec| {
            #[cfg(target_os = "macos")]
            let resolved = spec
                .bundles
                .iter()
                .find_map(|bundle| platform::resolve(bundle));
            #[cfg(not(target_os = "macos"))]
            let resolved = {
                #[cfg(windows)]
                {
                    platform::resolve(spec)
                }
                #[cfg(not(windows))]
                {
                    spec.programs
                        .iter()
                        .find_map(|program| platform::resolve(program))
                }
            };
            let command = resolved
                .as_ref()
                .map(|found| platform::command(spec, found))
                .unwrap_or_default();
            let available = resolved.is_some();
            Application {
                id: spec.id.to_string(),
                name: spec.name.to_string(),
                extensions: spec
                    .extensions
                    .iter()
                    .map(|value| (*value).to_string())
                    .collect(),
                command,
                available,
            }
        })
        .collect()
}

pub fn names() -> Vec<String> {
    platform::SPECS
        .iter()
        .map(|spec| spec.name.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 现场（2026-11-03）：`.prt` 点了「本地查看」只回一句「未能打开本地应用」，
    /// 根因是它不在这张表里 —— 而任务中心又把它当 CAD 文件提示参照文件，两边说法对不上。
    #[test]
    fn design_file_extensions_are_handed_to_the_os_handler() {
        for extension in [
            "prt", "asm", "catpart", "ipt", "sldprt", "dwg", "step", "3dm",
        ] {
            assert!(
                DEFAULT_OPENABLE.contains(&extension),
                ".{extension} 应该交给系统关联打开"
            );
        }
        // 反过来：可执行文件绝不在这张表里（安全约束不能因为加 CAD 扩展名而放松）。
        for extension in ["exe", "bat", "ps1", "cmd"] {
            assert!(
                !DEFAULT_OPENABLE.contains(&extension),
                ".{extension} 不该被悄悄启动"
            );
        }
    }

    /// 现场（2026-11-03）：客户机的 `.prt` 根本没有文件关联（`HKCR\.prt` 为空），
    /// 只靠 `open::that` 必然失败。所以 `.prt` 必须先交给 NX：找到 `ugraf.exe` 就直接开，
    /// 找不到才退回系统关联。这条测试在没装 NX 的机器上也要通过——它只钉「映射关系」，
    /// 装机状态由 `available` 表达。
    #[cfg(windows)]
    #[test]
    fn a_part_file_is_handed_to_nx() {
        let apps = detect();
        let app = for_file(&apps, "bracket.prt").expect(".prt 必须映射到 NX");
        assert_eq!(app.id, "nx");
        if app.available {
            let program = app.command[0].to_ascii_lowercase();
            assert!(
                program.ends_with("ugraf.exe"),
                "NX 的入口应是 ugraf.exe，实际是 {program}"
            );
            assert!(
                Path::new(&app.command[0]).is_file(),
                "探测到的入口必须真实存在"
            );
            // 裸文件名只会开一个空会话（真机实测标题是 `NX 7.5 - `，没有文件名）。
            assert_eq!(app.command[1], format!("-retrieve:{PLACEHOLDER}"));
        }
    }

    /// `NX 7.5` / `NX 12.0` / `NX 2306` 必须按数值而不是字符串比大小，
    /// 否则 `NX 7.5` 会被当成最新版，拿旧版本去开新版本的文件。
    #[cfg(windows)]
    #[test]
    fn nx_versions_compare_numerically() {
        let old = super::platform::nx_version(Path::new("C:/Siemens/NX 7.5"));
        let newer = super::platform::nx_version(Path::new("C:/Siemens/NX 12.0"));
        let newest = super::platform::nx_version(Path::new("C:/Siemens/NX 2306"));
        assert!(newer > old, "NX 12.0 应比 NX 7.5 新");
        assert!(newest > newer, "NX 2306 应比 NX 12.0 新");
        // 解析不出数字的目录排最前，只有实在没有别的版本时才会被试到。
        let unknown = super::platform::nx_version(Path::new("C:/Siemens/Teamcenter"));
        assert!(unknown < old);
    }

    /// NX 7.x 不认中文路径（真机实测：进程起来了但停在空会话）。这条只钉版本判定，
    /// 不依赖本机装的是哪个版本。
    #[cfg(windows)]
    #[test]
    fn nx_only_reads_unicode_paths_from_version_eight() {
        assert!(!super::nx_reads_unicode_paths(
            r"D:\Program Files (x86)\UGS\NX 7.5\UGII\ugraf.exe"
        ));
        assert!(super::nx_reads_unicode_paths(
            r"C:\Program Files\Siemens\NX 12.0\NXBIN\ugraf.exe"
        ));
        assert!(super::nx_reads_unicode_paths(
            r"C:\Program Files\Siemens\NX 2306\NXBIN\ugraf.exe"
        ));
        // 版本读不出来时按「支持」处理：宁可让用户看到空会话，也不要误拦一台能用的机器。
        assert!(super::nx_reads_unicode_paths(r"D:\NX\UGII\ugraf.exe"));
    }

    /// 手动跑的端到端：让 `open_path` 真的把部件交给本机 NX。
    /// 默认 `#[ignore]` —— 它会拉起一个完整的 NX 进程（几十秒、几百兆内存），
    /// 不能进 CI；在受限会话里（进程随命令结束被回收）也会被连带杀掉，
    /// 所以要在真实桌面上跑：
    /// `CLOUDFILE_NX_SAMPLE=D:\tmp\arrow.prt cargo test -p cloudfile-core
    /// opens_a_part_through_the_detected_nx -- --ignored --nocapture`
    /// 通过的标准是 NX 标题里出现 `[<文件名>]`，而不是「进程起来了」。
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn opens_a_part_through_the_detected_nx() {
        let sample = std::env::var("CLOUDFILE_NX_SAMPLE").expect("需要 CLOUDFILE_NX_SAMPLE");
        let path = Path::new(&sample);
        assert!(path.is_file(), "样例部件不存在：{sample}");
        let root = std::env::temp_dir().join("cloudfile-nx-open");
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        open_path(&root, "local-edit", &name, path).expect("应当把部件交给 NX");
    }

    /// 现场（2026-11-03）：仿真数据文件点「本地查看」被白名单挡住。
    /// 用户列的 23 个名字里有大小写变体（`.dta_for_Mod1` 等），`extension_of()`
    /// 比较前下转，所以名单只收小写 —— 这里把两条路径都钉住。
    #[test]
    fn simulation_data_suffixes_are_openable() {
        for name in [
            "run.erg",
            "run.input",
            "run.dta",
            "run.out",
            "run.in",
            "run.dta_for_mod1",
            "run.dta_for_Mod1",
            "run.dta_from_mod1",
            "run.dta_from_Mod1",
            "run.dta_for_mod2",
            "run.dta_for_Mod2",
            "run.dta_from_mod2",
            "run.dta_from_Mod2",
            "run.dta_for_mod3",
            "run.dta_for_Mod3",
            "run.dta_from_mod3",
            "run.dta_from_Mod3",
            "run.erg_from_mod1",
            "run.erg_from_Mod1",
            "run.erg_from_mod2",
            "run.erg_from_Mod2",
            "run.erg_from_mod3",
            "run.erg_from_Mod3",
            // 2026-11-04：.aitp 与 .dat 也属于现场的数据文件，`.DAT` 走同一条下转路径。
            "run.aitp",
            "run.AITP",
            "run.dat",
            "run.DAT",
        ] {
            assert!(
                DEFAULT_OPENABLE.contains(&extension_of(name).as_str()),
                "{name} 应该交给系统关联打开"
            );
        }
    }

    /// A program path that is absolute on the host OS. Rules must not hard-code
    /// a Unix path: `Path::is_absolute` is false for `/usr/bin/tool` on Windows,
    /// so such a test passes locally and fails the Windows runner.
    #[cfg(windows)]
    const TOOL: &str = "C:/tools/tool.exe";
    #[cfg(not(windows))]
    const TOOL: &str = "/usr/bin/tool";

    fn rule(id: &str, modes: &[&str], extensions: &[&str], command: &[&str]) -> OpenRule {
        OpenRule {
            id: id.into(),
            modes: modes.iter().map(|value| (*value).into()).collect(),
            extensions: extensions.iter().map(|value| (*value).into()).collect(),
            command: command.iter().map(|value| (*value).into()).collect(),
        }
    }

    #[test]
    fn runtime_probe_only_answers_where_it_applies() {
        // macOS renders through the system WebKit, so there is nothing to report
        // and the caller must be able to tell that apart from "missing".
        if cfg!(windows) {
            // Either a version string or nothing; never a placeholder.
            if let Some(version) = webview2_version() {
                assert!(!version.is_empty());
                assert!(version.chars().next().unwrap().is_ascii_digit());
            }
        } else {
            assert_eq!(webview2_version(), None);
        }
    }

    #[test]
    fn rule_validation_rejects_unsafe_commands() {
        for bad in [
            rule("a", &["local-edit"], &["dwg"], &["relative/tool"]),
            rule("a", &["local-edit"], &["dwg"], &[TOOL]),
            rule("a", &["local-edit"], &["dwg"], &[TOOL, "{file}", "{file}"]),
            rule("a", &["prompt"], &["dwg"], &[TOOL, "{file}"]),
            rule("", &["local-edit"], &["dwg"], &[TOOL, "{file}"]),
            rule("a", &["local-edit"], &[], &[TOOL, "{file}"]),
            rule("a", &["local-edit"], &["DWG"], &[TOOL, "{file}"]),
        ] {
            assert!(bad.validate().is_err(), "{bad:?} must be rejected");
        }
        rule("ok", &["local-edit"], &["dwg"], &[TOOL, "{file}"])
            .validate()
            .unwrap();
    }

    #[test]
    fn exclusive_checkout_uses_edit_application_rule_but_view_does_not() {
        let rule = rule("company-cad", &["local-edit"], &["dwg"], &[TOOL, "{file}"]);
        rule.validate().unwrap();
        let rules = vec![rule];
        assert_eq!(
            resolve_open_rule(&rules, "optimistic-edit", "drawing.dwg")
                .unwrap()
                .id,
            "company-cad"
        );
        assert_eq!(
            resolve_open_rule(&rules, "exclusive-edit", "drawing.dwg")
                .unwrap()
                .id,
            "company-cad"
        );
        assert!(resolve_open_rule(&rules, "local-view", "drawing.dwg").is_none());
        assert!(resolve_open_rule(&rules, "other-mode", "drawing.dwg").is_none());
    }

    #[test]
    fn specific_extension_outranks_a_wildcard() {
        let rules = vec![
            rule("catch", &["local-edit"], &["*"], &[TOOL, "{file}"]),
            rule("cad", &["local-edit"], &["dwg"], &[TOOL, "{file}"]),
        ];
        assert_eq!(
            resolve_open_rule(&rules, "optimistic-edit", "a.dwg")
                .unwrap()
                .id,
            "catch"
        );
        // A `*` only applies after every specific rule failed.
        assert_eq!(
            resolve_open_rule(&rules, "local-edit", "a.txt").unwrap().id,
            "catch"
        );
        let ordered = vec![
            rule("cad", &["local-edit"], &["dwg"], &[TOOL, "{file}"]),
            rule("catch", &["local-edit"], &["*"], &[TOOL, "{file}"]),
        ];
        assert_eq!(
            resolve_open_rule(&ordered, "local-edit", "a.dwg")
                .unwrap()
                .id,
            "cad"
        );
    }

    #[test]
    fn rules_are_scoped_to_one_mode_and_rendered_without_a_shell() {
        let rules = vec![rule(
            "view",
            &["local-view"],
            &["dwg"],
            &[TOOL, "--readonly", "{file}"],
        )];
        assert!(resolve_open_rule(&rules, "local-edit", "a.dwg").is_none());
        assert!(resolve_open_rule(&rules, "local-view", "a.dwg").is_some());
        let rendered = rules[0].render(Path::new("/tmp/a b.dwg"));
        assert_eq!(
            rendered,
            vec![TOOL, "--readonly", "/tmp/a b.dwg"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn duplicate_rule_ids_are_rejected() {
        let rules = vec![
            rule("same", &["local-edit"], &["dwg"], &[TOOL, "{file}"]),
            rule("same", &["local-edit"], &["dxf"], &[TOOL, "{file}"]),
        ];
        assert!(validate_rules(&rules).is_err());
    }

    #[test]
    fn detection_always_answers_with_a_stable_shape() {
        let apps = detect();
        assert!(!apps.is_empty());
        assert_eq!(apps.len(), platform::SPECS.len());
        for app in &apps {
            assert!(!app.extensions.is_empty());
            if app.available {
                assert!(Path::new(&app.command[0]).is_absolute());
                assert_eq!(
                    app.command
                        .iter()
                        .filter(|argument| argument.contains(PLACEHOLDER))
                        .count(),
                    1
                );
            } else {
                assert!(app.command.is_empty());
            }
        }
        // First match wins, which is why the table order is the priority order.
        if let Some(app) = for_file(&apps, "MODEL.STEP") {
            assert!(app.extensions.contains(&"step".to_string()));
        }
    }

    #[test]
    fn unknown_types_are_not_launched_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evil.exe");
        std::fs::write(&path, b"x").unwrap();
        assert!(open_path(dir.path(), "local-edit", "evil.exe", &path).is_err());
        assert!(open_path(
            dir.path(),
            "local-edit",
            "notes.txt",
            Path::new("relative.txt")
        )
        .is_err());
    }
}
