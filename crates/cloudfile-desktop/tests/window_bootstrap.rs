//! 窗口创建契约：`tauri.conf.json` 里不能有任何 `create:true` 的窗口。
//!
//! 为什么要有这条测试（2026-11-02 现场）：Tauri 会在 `setup` 之前按配置自动创建窗口。
//! 缺 WebView2 运行时的机器上，自动创建 webview 的结果是 `tauri-runtime-wry` 直接弹一句
//! 英文原生错误框（`Could not find the WebView2 Runtime.`）—— 那发生在任何中文引导之前，
//! 用户既看不懂也不知道该装什么。
//!
//! 所以三个窗口都改成 `create:false`，由桌面进程探测运行时后自己创建（见 `create_window`）。
//! 这条契约只能靠测试守住：谁把 `create` 改回 true，行为就悄悄退回旧样子，而 macOS CI
//! 上完全看不出来（macOS 永远有系统 WebKit）。
use serde_json::{json, Value};

#[test]
fn no_window_is_created_before_the_runtime_is_checked() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tauri.conf.json");
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{path}: {error}")),
    )
    .unwrap_or_else(|error| panic!("{path}: {error}"));
    let windows = config["app"]["windows"]
        .as_array()
        .unwrap_or_else(|| panic!("tauri.conf.json 里没有 app.windows：{config}"));
    // 三个窗口（main / float / toast）都得在配置里：它们是运行时创建的模板。
    assert!(windows.len() >= 3, "{config}");
    for window in windows {
        assert_eq!(
            window["create"],
            json!(false),
            "窗口 {} 不能让 Tauri 在启动时自动创建：缺 WebView2 运行时的机器会先看到英文错误框",
            window["label"]
        );
    }
}
