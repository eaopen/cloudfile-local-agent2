// Light notice window.
//
// Shown by the desktop process and hidden by it again, so this script only
// paints. It never takes focus and never blocks the user: the process sets
// `ignore_cursor_events` and shows the window without activating it.
const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

// 品牌名由 agent 侧统一给出（与主窗口、悬浮入口同源）。取不到就用默认名，
// 提示窗口绝不能因为元数据问题画不出来。
let brand = 'CloudFile';
invoke('app_brand').then((name) => { if (name) brand = name; }).catch(() => {});

const card = document.querySelector('#card');
const title = document.querySelector('#title');
const text = document.querySelector('#text');

listen('toast', (event) => {
  const payload = event.payload || {};
  const warning = payload.level === 'warning';
  const working = payload.level === 'working';
  // 三种提示各有各的标题：混用会让"正在下载"看起来像"已完成"。
  title.textContent = warning
    ? `${brand} · 需要处理`
    : working ? `${brand} · 正在准备文件` : `${brand} · 已保存到云端`;
  text.textContent = String(payload.text || '');
  card.className = warning ? 'toast warning' : working ? 'toast working' : 'toast info';
  // Restart the fade so two notices in a row both animate.
  card.classList.remove('show');
  void card.offsetWidth;
  card.classList.add('show');
}).catch(() => {});
