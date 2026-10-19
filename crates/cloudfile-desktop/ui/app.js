// CloudFile task centre.
//
// Every panel here is a view onto the durable store owned by the desktop
// process: nothing is cached across refreshes and nothing decides on its own
// whether a task is finished. The window only renders state and forwards the
// user's choice.
//
// 安装引导（setup view）也一样：它把 `setup_state` 报出来的事实画成步骤，
// 「一键完成」只是在同一个进程里依次做那几件事 —— 没有一步是界面自己判断的。
const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const taskUX = window.CloudFileTaskUX;

const message = document.querySelector('#message');
const tasks = document.querySelector('#tasks');
const counts = document.querySelector('#counts');
const exitBanner = document.querySelector('#confirm-exit');
const exitText = document.querySelector('#confirm-exit-text');
const updateBanner = document.querySelector('#update-hint');
const hostState = document.querySelector('#host-state');
const setupSteps = document.querySelector('#setup-steps');
const setupLog = document.querySelector('#setup-log');
const setupHelp = document.querySelector('#setup-extension-help');

let busy = new Set();
let refreshing = false;
let lastAssessment = null;
let lastSummary = null;
let services = null;
let component = null;
let welcomeKey = null;
let setup = null;

const VIEWS = {
  tasks: ['本地任务', '查看编辑进度，处理需要确认的任务。'],
  services: ['服务连接', '管理 CloudFile 服务，查看设备连接状态。'],
  settings: ['设置', '按你的习惯安排本地工作。'],
  setup: ['安装引导', '检查本机 Host、工作目录、浏览器扩展与服务连接。'],
};

function setView(name) {
  if (!VIEWS[name]) return;
  if (name === 'setup') loadSetup().catch((error) => say(String(error)));
  message.textContent = '';
  for (const key of Object.keys(VIEWS)) {
    document.querySelector(`#view-${key}`).hidden = key !== name;
  }
  for (const item of document.querySelectorAll('[data-view]')) {
    const selected = item.dataset.view === name;
    item.classList.toggle('selected', selected);
    if (selected) item.setAttribute('aria-current', 'page');
    else item.removeAttribute('aria-current');
  }
  document.querySelector('#page-title').textContent = VIEWS[name][0];
  document.querySelector('#page-description').textContent = VIEWS[name][1];
  document.querySelector('.header-actions').hidden = name !== 'tasks';
  document.querySelector('main').scrollTop = 0;
}

async function openWorkspace() {
  try {
    await invoke('open_workspace');
    say('已打开工作目录。');
  } catch (error) { say(String(error)); }
}

// First-use guidance reflects the desktop's service/registration records only.
// It never invents a paired device or starts pairing merely by opening a view.
function renderWelcome() {
  if (!lastSummary || lastSummary.tasks.length) return;
  const key = JSON.stringify([services, component?.registered_host]);
  // Keep keyboard focus on onboarding actions during periodic status refreshes.
  if (tasks.firstElementChild?.classList.contains('welcome') && welcomeKey === key) return;
  welcomeKey = key;
  tasks.replaceChildren();
  if (services === null || component === null) {
    const loading = document.createElement('p');
    loading.className = 'empty';
    loading.textContent = '正在读取服务与任务状态…';
    tasks.append(loading);
    return;
  }
  const paired = services.some((service) => service.state === 'paired');
  const registered = Boolean(component.registered_host?.registered && !component.registered_host.foreign);
  const ready = paired && registered;
  const card = document.createElement('section');
  card.className = 'welcome';
  const icon = document.createElement('div');
  icon.className = 'welcome-icon';
  icon.setAttribute('aria-hidden', 'true');
  icon.textContent = ready ? '✓' : '↗';
  const heading = document.createElement('h2');
  heading.textContent = ready ? '已准备好，开始本地编辑' : '让云端文件，在本机应用中打开';
  const description = document.createElement('p');
  description.textContent = ready
    ? '从 CloudFile 网页选择文件，点击“本地编辑”。文件会在本机打开，编辑进度与需要处理的事项将在这里显示。'
    : '完成服务与浏览器连接后，就能使用熟悉的 Office、CAD 等本机应用处理文件。';
  const actions = document.createElement('div');
  actions.className = 'welcome-actions';
  if (ready) {
    actions.append(button('打开工作目录', openWorkspace));
    actions.append(button('查看服务连接', () => setView('services'), { secondary: true }));
  } else if (!services.length) {
    actions.append(button('添加 CloudFile 服务', () => {
      setView('services'); document.querySelector('#origin').focus();
    }));
  } else if (!registered) {
    actions.append(button('检查浏览器连接', () => {
      setView('setup');
    }));
  } else {
    actions.append(button('查看连接步骤', () => {
      document.querySelector('#task-help').open = true;
      document.querySelector('#task-help').scrollIntoView({ block: 'nearest' });
    }));
    actions.append(button('管理服务', () => setView('services'), { secondary: true }));
  }
  card.append(icon, heading, description, actions);
  if (ready) {
    const note = document.createElement('p');
    note.className = 'ready-note';
    note.textContent = '暂时没有编辑任务。普通编辑在本机保存后自动上传，独占编辑由你手动提交。';
    card.append(note);
  } else {
    const steps = document.createElement('div');
    steps.className = 'setup-steps';
    for (const [index, title, description, complete] of [
      [1, '添加服务', services.length ? '已保存组织网盘地址' : '填写组织提供的网盘地址', services.length > 0],
      [2, '检查浏览器连接', registered ? '本机连接组件已注册' : '安装扩展并连接本机 Agent', registered],
      [3, '从网页开始编辑', paired ? '设备已配对' : '登录网盘，首次编辑时完成配对', paired],
    ]) {
      const step = document.createElement('div');
      step.className = complete ? 'step complete' : 'step';
      const number = document.createElement('span');
      number.className = 'step-number'; number.textContent = complete ? '✓' : String(index);
      const body = document.createElement('div');
      const label = document.createElement('strong'); label.textContent = title;
      const note = document.createElement('p'); note.textContent = description;
      body.append(label, note); step.append(number, body); steps.append(step);
    }
    card.append(steps);
  }
  tasks.append(card);
}

function say(text) {
  message.textContent = text;
}

// ── 安装引导 ────────────────────────────────────────────────────────────────

// 步骤与「完成判据」的唯一来源。判据全部来自 `setup_state`，
// 界面不自己做「大概装好了吧」的推断。
const SETUP_STEPS = [
  {
    key: 'agent',
    title: '安装 Agent',
    hint: '任务中心已经在运行，二进制就位',
    state: (s) => ({ ok: true, detail: `v${s.version} · ${s.install_root}` }),
  },
  {
    key: 'trusted',
    title: '受信任的 CloudFile 服务',
    hint: '请在「服务连接」添加组织提供的服务器地址',
    state: (s) => ({
      ok: s.trusted_ready,
      detail:
        s.trusted_ready
          ? `${s.trusted_count} 个服务已配置`
          : '尚未添加服务，请前往「服务连接」设置',
    }),
  },
  {
    key: 'workspace',
    title: '工作目录',
    hint: '本地副本落在这里，可在设置里改',
    state: (s) => ({
      ok: s.workspace_exists,
      detail: s.workspace_exists ? s.workspace_root : `${s.workspace_root}（尚未创建）`,
    }),
  },
  {
    key: 'host',
    title: '注册 Chrome Host',
    hint: '扩展 ID 固定，注册由 Agent 自动写入',
    state: (s) => ({
      ok: s.host_registered,
      detail: s.host_registered
        ? `${s.host} 已注册`
        : s.host_foreign
          ? `${s.host} 由其他程序注册，一键完成会备份后接管`
          : `${s.host} 尚未注册`,
    }),
  },
  {
    key: 'autostart',
    title: '开机自启',
    hint: '登录后自动运行，关闭窗口不丢任务',
    state: (s) => ({
      ok: s.autostart_enabled,
      detail: s.autostart_enabled ? '已设为开机自启' : '未设置',
    }),
  },
  {
    key: 'extension',
    title: '安装浏览器扩展',
    hint: '自动下载解包，最后在 Chrome 里点三下',
    state: (s) => ({
      ok: Boolean(s.extension_version),
      detail: s.extension_version
        ? `已部署 ${s.extension_version} → ${s.extension_dir}`
        : `尚未部署到 ${s.extension_dir}`,
    }),
  },
];

function renderSetup(state) {
  setup = state;
  setupSteps.replaceChildren();
  let done = 0;
  for (const step of SETUP_STEPS) {
    const outcome = step.state(state);
    if (outcome.ok) done += 1;
    const item = document.createElement('li');
    item.className = outcome.ok ? 'step done' : 'step';
    const mark = document.createElement('span');
    mark.className = 'mark';
    mark.textContent = outcome.ok ? '✓' : '○';
    const body = document.createElement('div');
    const title = document.createElement('strong');
    title.textContent = step.title;
    const detail = document.createElement('span');
    detail.textContent = outcome.detail;
    const hint = document.createElement('small');
    hint.className = 'hint';
    hint.textContent = step.hint;
    body.append(title, detail, hint);
    item.append(mark, body);
    setupSteps.append(item);
  }
  const ratio = document.createElement('div');
  ratio.className = 'count';
  const value = document.createElement('b');
  value.textContent = `${done}/${SETUP_STEPS.length}`;
  const caption = document.createElement('span');
  caption.textContent = '已完成';
  ratio.append(value, caption);
  document.querySelector('#setup-counts').replaceChildren(ratio);

  setupHelp.hidden = !state.extension_version;
  document.querySelector('#setup-ext-dir').value = state.extension_dir;
  document.querySelector('#setup-ext-id').textContent = state.extension_id;
  document.querySelector('#host-extension-id').textContent = state.extension_id;
  document.querySelector('#setup-finish').textContent = state.setup_completed
    ? '进入任务中心'
    : '完成，进入任务中心';
}

async function loadSetup() {
  const state = await invoke('setup_state');
  renderSetup(state);
  return state;
}

function showView(target) {
  setView(target);
}

function logSetup(title, lines) {
  setupLog.hidden = false;
  const head = document.createElement('div');
  head.className = 'log-head';
  head.textContent = title;
  setupLog.append(head);
  for (const line of lines) {
    const row = document.createElement('div');
    row.className = line.ok ? 'log-row ok' : 'log-row bad';
    row.textContent = `${line.ok ? '✓' : '✗'} ${line.label}${line.detail ? ' — ' + line.detail : ''}`;
    setupLog.append(row);
  }
}

const STEP_LABELS = {
  trusted: '受信任服务',
  workspace: '工作目录',
  host: '注册 Host',
  autostart: '开机自启',
  extension: '部署扩展',
};

async function runSetup() {
  const button = document.querySelector('#setup-run');
  button.disabled = true;
  say('正在安装，请稍候…');
  setupLog.hidden = true;
  setupLog.replaceChildren();
  try {
    const result = await invoke('setup_apply');
    logSetup(
      '一键完成',
      result.steps.map((step) => ({
        ok: step.ok,
        label: STEP_LABELS[step.key] || step.key,
        detail: step.detail,
      })),
    );
    const state = await loadSetup();
    const failed = result.steps.filter((step) => !step.ok);
    say(
      failed.length
        ? `${failed.length} 步没成功，原因见上面的记录；修好后可以再点一次「一键完成」。`
        : '五步都完成了；最后在 Chrome 里加载扩展，然后点「完成，进入任务中心」。',
    );
    if (state.extension_version) setupHelp.hidden = false;
  } catch (error) {
    say(String(error));
  } finally {
    button.disabled = false;
  }
}

async function openExtensionPage() {
  try {
    const result = await invoke('open_extension_page');
    setupHelp.hidden = false;
    document.querySelector('#setup-ext-dir').value = result.directory;
    say(
      result.chrome
        ? `已用 ${result.chrome} 打开扩展管理页；开启开发者模式后选择上面的目录。`
        : '已把扩展管理页交给默认浏览器；若打不开，请在 Chrome 地址栏输入 chrome://extensions。',
    );
  } catch (error) {
    say(String(error));
  }
}

async function openExtensionDir() {
  try {
    say(`已打开扩展目录：${await invoke('open_extension_dir')}`);
  } catch (error) {
    say(String(error));
  }
}

async function finishSetup() {
  try {
    await invoke('setup_finish');
    showView('tasks');
    await Promise.all([loadSettings(), renderOrigins(), refresh(), renderExitState()]);
    say('安装引导已完成。之后可以从左侧「安装引导」重新打开做修复。');
  } catch (error) {
    say(String(error));
  }
}

// ── 任务列表 ────────────────────────────────────────────────────────────────

function stateText(result) {
  if (result.state === 'published') return '云端已确认新版本。';
  if (result.state === 'unchanged') return '没有需要提交的修改。';
  if (result.state === 'pending') return '提交结果尚未确认，本地副本已保留；请先查询原提交结果。';
  if (result.state === 'released') return '已释放 Checkout，本地任务已关闭。';
  if (result.state === 'renewed') return 'Checkout 已续租。';
  if (result.state === 'running') return '更新已在后台开始；完成前不会替换正在运行的版本。';
  return '操作已完成。';
}

async function action(task, command, args) {
  if (busy.has(task.task_id)) return;
  busy.add(task.task_id);
  say('正在处理，请保留本地文件…');
  refresh();
  try {
    const result = await invoke(command, { taskId: task.task_id, ...args });
    say(stateText(result));
  } catch (error) {
    say(String(error));
  } finally {
    busy.delete(task.task_id);
    refresh();
  }
}

// The exit banner maps the guard's kebab-case reasons to wording that says what
// the agent did with the file, never that the file is "safe". Automatic upload
// pause reasons are already user-facing text and are shown verbatim.
const REASON_TEXT = {
  'unreadable-state': '任务状态无法读取，已停止自动处理',
  'pending-intent': '原提交尚未确认，本地修改已保留',
  'unacknowledged-lease': 'Checkout 交付尚未确认，请重新打开该文件',
  'active-checkout': '独占 Checkout 仍在续租中',
};

// Mode-specific task messages are derived by task-ux.js, not inferred by UI actions.

function button(label, handler, { secondary = false, disabled = false } = {}) {
  const element = document.createElement('button');
  element.textContent = label;
  if (secondary) element.className = 'secondary';
  element.disabled = disabled;
  element.onclick = handler;
  return element;
}

function openButton(task) {
  return button(
    '打开文件',
    async () => {
      try {
        await invoke('open_task', { taskId: task.task_id });
      } catch (error) {
        say(String(error));
      }
    },
    { secondary: true },
  );
}

function renderOptimistic(buttons, task, automatic, working) {
  buttons.append(openButton(task));
  if (task.pending_upload) {
    buttons.append(button('查询原上传结果', () => action(task, 'recover_upload'), { disabled: working }));
    buttons.append(button('重新尝试原上传', () => action(task, 'save_cloud'), {
      secondary: true, disabled: working || task.needs_review,
    }));
  } else {
    buttons.append(button('立即上传', () => action(task, 'save_cloud'), {
      disabled: working || task.needs_review || !task.local_changed,
    }));
  }
  const paused = Boolean(automatic && automatic.pause_reason);
  buttons.append(
    button(
      paused ? '恢复自动上传' : automatic && automatic.enabled ? '暂停自动上传' : '开启自动上传',
      async () => {
        try {
          await invoke('set_auto_upload', {
            taskId: task.task_id,
            enabled: !(automatic && automatic.enabled) || paused,
          });
          await refresh();
        } catch (error) {
          say(String(error));
        }
      },
      { secondary: true, disabled: working || task.needs_review || task.pending_upload },
    ),
  );
}

function renderExclusive(buttons, task, working) {
  buttons.append(openButton(task));
  if (task.checkout_closed) return;
  const pending = task.pending_upload;
  buttons.append(
    button('提交版本（继续独占）', () => action(task, 'commit_task', { checkin: false }), {
      disabled: working || task.needs_review || pending,
    }),
  );
  buttons.append(
    button('签入并释放', () => action(task, 'commit_task', { checkin: true }), {
      secondary: true,
      disabled: working || task.needs_review || pending,
    }),
  );
  if (pending) {
    buttons.append(button('查询提交结果', () => action(task, 'recover_commit'), { disabled: working }));
    buttons.append(button('重新尝试原提交', () => action(task, 'retry_commit'), {
      secondary: true, disabled: working,
    }));
  }
  buttons.append(
    button('续租', () => action(task, 'renew_checkout'), {
      secondary: true,
      disabled: working || pending || !task.has_lease,
    }),
  );
  buttons.append(
    button('释放独占（不提交）', () => {
      const warning = taskUX.releaseWarning(task);
      if (warning && window.confirm(warning)) action(task, 'release_checkout');
    }, {
      secondary: true,
      disabled: working || pending || !task.has_lease,
    }),
  );
}

function renderTask(task) {
  const row = document.createElement('article');
  row.className = 'task';
  row.id = task.task_id;

  const info = document.createElement('div');
  info.className = 'info';
  const name = document.createElement('strong');
  name.textContent = task.file_name;
  const status = document.createElement('span');
  const working = busy.has(task.task_id) || task.uploading;
  const automatic = task.auto_upload;
  status.textContent = taskUX.status(task, automatic, working);
  const mode = document.createElement('small');
  mode.className = 'hint';
  mode.textContent = taskUX.modeLabel(task);
  const source = document.createElement('small');
  source.className = 'task-source';
  source.textContent = [task.service, task.remote_path].filter(Boolean).join(' · ');
  info.append(name, mode, status);
  if (source.textContent) info.append(source);
  const reference = taskUX.referenceHint(task.file_name);
  if (reference) {
    const warning = document.createElement('small');
    warning.className = 'task-reference';
    warning.textContent = reference;
    info.append(warning);
  }

  if (automatic && automatic.pause_reason && !task.exclusive) {
    const reason = document.createElement('p');
    reason.className = 'upload-error';
    reason.textContent = automatic.pause_reason;
    info.append(reason);
  }

  const buttons = document.createElement('div');
  buttons.className = 'actions';
  if (task.exclusive) {
    renderExclusive(buttons, task, working);
  } else if (task.can_upload) {
    renderOptimistic(buttons, task, automatic, working);
  } else {
    buttons.append(openButton(task));
    const hint = document.createElement('small');
    hint.className = 'hint';
    hint.textContent = task.mode === 'local-view'
      ? '仅供查看，本地修改不会回传云端'
      : '旧会话仅支持本地打开；要上传请从网页重新发起普通编辑';
    buttons.append(hint);
  }

  row.append(info, buttons);
  return row;
}

const GROUPS = [
  ['needs-attention', '待处理', '需要你决定'],
  ['in-progress', '进行中', '本地已修改或正在上传'],
  ['done', '已完成', '与云端一致'],
];

function renderSummary(result) {
  lastSummary = result;
  counts.hidden = !result.tasks.length;
  const attention = result.counts['needs-attention'] || 0;
  document.querySelector('#attention-count').hidden = !attention;
  document.querySelector('#attention-count').textContent = String(attention);
  counts.replaceChildren();
  for (const [key, label] of GROUPS) {
    const cell = document.createElement('div');
    cell.className = 'count';
    const value = document.createElement('b');
    value.textContent = String(result.counts[key] || 0);
    const caption = document.createElement('span');
    caption.textContent = label;
    cell.append(value, caption);
    counts.append(cell);
  }

  document.querySelector('#cdc-hint').hidden = !result.cdc_enabled;

  if (!result.tasks.length) {
    renderWelcome();
    return;
  }

  tasks.replaceChildren();
  for (const [key, label, hint] of GROUPS) {
    const members = result.tasks.filter((task) => task.group === key);
    if (!members.length) continue;
    const header = document.createElement('div');
    header.className = 'group';
    const heading = document.createElement('h2');
    heading.textContent = label;
    const note = document.createElement('small');
    note.textContent = `${members.length} 项 · ${hint}`;
    header.append(heading, note);
    tasks.append(header);
    for (const task of members) tasks.append(renderTask(task));
  }
}

async function refresh() {
  if (refreshing) return;
  refreshing = true;
  try {
    const result = await invoke('task_summary');
    renderSummary(result);
    // The entry mirrors the same list, so a task can never be visible in one
    // view and hidden in the other.
    invoke('float_sync').catch(() => {});
  } catch (error) {
    say(String(error));
  } finally {
    refreshing = false;
  }
}

async function renderExitState() {
  try {
    const assessment = await invoke('exit_assessment');
    lastAssessment = assessment;
    const target = document.querySelector('#exit-state');
    target.textContent = assessment.safe_to_exit
      ? '没有待确认的提交'
      : `${assessment.active_checkouts} 个独占任务 · ${assessment.uncertain} 项待确认`;
  } catch {
    /* The banner already covers the actionable case. */
  }
}

function showExitBanner(assessment) {
  if (assessment && assessment.safe_to_exit) {
    exitBanner.hidden = true;
    return;
  }
  const risks = (assessment && assessment.risks) || [];
  exitText.textContent = risks.length
    ? risks
        .slice(0, 4)
        .map((risk) => {
          const reason = REASON_TEXT[risk.reasons[0]] || risk.reasons[0];
          return `${risk.file_name}：${reason}`;
        })
        .join('；')
    : '仍有独占编辑任务或未确认的上传。';
  exitBanner.hidden = false;
}

// 自启落在哪条途径上：企业机器上 Run 键常被策略锁死，实际用的是「启动」文件夹，
// 不说清楚的话用户会以为开关是假的。
const AUTOSTART_MECHANISMS = {
  'run-key': '已设为开机自启（当前用户注册表 Run 键）。',
  'startup-folder': '已设为开机自启，途径是「启动」文件夹（注册表被策略限制时的备用途径）。',
  'launch-agent': '已设为开机自启（登录项 LaunchAgent）。',
};

function showAutostartNote(mechanism) {
  const note = document.querySelector('#autostart_note');
  const text = mechanism ? (AUTOSTART_MECHANISMS[mechanism] || `已设为开机自启（${mechanism}）。`) : '';
  note.hidden = !text;
  note.textContent = text;
}

async function loadSettings() {
  const current = await invoke('settings_get');
  component = current;
  document.querySelector('#notify_on_saved').checked = current.settings.notify_on_saved;
  document.querySelector('#notify_on_review').checked = current.settings.notify_on_review;
  document.querySelector('#float_enabled').checked = current.settings.float_enabled;
  document.querySelector('#float_hidden_when_empty').checked =
    current.settings.float_hidden_when_empty;
  document.querySelector('#autostart').checked = current.autostart_enabled;
  showAutostartNote(current.autostart_mechanism);
  document.querySelector('#workspace_root').value = current.settings.workspace_root || '';
  document.querySelector('#host-extension-id').textContent = (setup && setup.extension_id) || '';
  document.querySelector('#version').textContent = `CloudFile Agent · ${current.version}`;
  document.querySelector('#sidebar-version').textContent = `v${current.version}`;
  document.querySelector('#install-path').textContent = `安装目录：${current.install_root}`;

  const registration = current.registered_host;
  if (!registration) {
    hostState.textContent = '未找到本机 Host 目录，无法注册。';
  } else if (registration.foreign) {
    hostState.textContent = `Host ${registration.host} 由其他程序注册，可备份后接管。`;
  } else if (registration.registered) {
    hostState.textContent = `Host ${registration.host} 已注册（扩展 ${registration.extension_id}）→ ${registration.manifest_path}`;
  } else {
    hostState.textContent = `Host ${registration.host} 尚未注册；点「重新注册」由 Agent 完成，不需要 Python。`;
  }
  renderWelcome();
  return current;
}

async function renderOrigins() {
  const values = await invoke('service_status');
  services = values;
  const paired = values.some((service) => service.state === 'paired');
  const connection = document.querySelector('#connection-state');
  connection.textContent = paired ? '● 设备已连接' : values.length ? '○ 设备尚未连接' : '○ 尚未添加服务';
  connection.classList.toggle('connected', paired);
  document.querySelector('#service-dot').hidden = !paired;
  const list = document.querySelector('#origins');
  list.replaceChildren();
  for (const service of values) {
    const item = document.createElement('li');
    const label = document.createElement('div');
    label.className = 'service-info';
    const stateTextMap = {
      paired: '已连接设备',
      pending: '等待 Chrome 完成配对',
      unpaired: '尚未连接设备',
    };
    const address = document.createElement('strong'); address.textContent = service.service_url;
    const state = document.createElement('span'); state.className = `service-status ${service.state}`;
    state.textContent = stateTextMap[service.state] || service.state;
    label.append(address, state);
    item.append(label);
    const actions = document.createElement('div');
    actions.className = 'row';
    if (service.state !== 'paired') {
      const element = document.createElement('button');
      element.textContent = '连接说明';
      element.className = 'secondary';
      element.onclick = async () => {
        try {
          setView('tasks');
          document.querySelector('#task-help').open = true;
          say('在 Chrome 登录此 CloudFile 服务，选择文件并点击“本地编辑”，按网页提示完成设备连接。');
        } catch (error) {
          say(String(error));
        }
      };
      actions.append(element);
    }
    // 默认项不给按钮：能不能删由核心层判定，界面上连入口都不留。
    if (!service.default) {
      actions.append(
        button(
          '移除',
          async () => {
            try {
              await invoke('untrust_origin', { origin: service.origin });
              say('已移除该服务。');
              await renderOrigins();
            } catch (error) {
              say(String(error));
            }
          },
          { secondary: true },
        ),
      );
    }
    item.append(actions);
    list.append(item);
  }
  if (!values.length) {
    const empty = document.createElement('li'); empty.className = 'hint';
    empty.textContent = '尚未添加服务。在下方填写组织提供的网盘地址即可开始。'; list.append(empty);
  }
  renderWelcome();
}

function wire() {
  for (const item of document.querySelectorAll('[data-view]')) {
    item.onclick = () => setView(item.dataset.view);
  }
  document.querySelector('#setup-run').onclick = runSetup;
  document.querySelector('#setup-extension').onclick = openExtensionPage;
  document.querySelector('#setup-open-dir').onclick = openExtensionDir;
  document.querySelector('#setup-finish').onclick = finishSetup;
  document.querySelector('#setup-ext-dir').onclick = (event) => event.target.select();

  document.querySelector('#refresh').onclick = () => {
    refresh();
    renderOrigins().catch((error) => say(String(error)));
    renderExitState();
  };

  document.querySelector('#open-workspace').onclick = openWorkspace;

  document.querySelector('#settings').onsubmit = async (event) => {
    event.preventDefault();
    try {
      await invoke('settings_set', {
        notifyOnSaved: document.querySelector('#notify_on_saved').checked,
        notifyOnReview: document.querySelector('#notify_on_review').checked,
        floatEnabled: document.querySelector('#float_enabled').checked,
        floatHiddenWhenEmpty: document.querySelector('#float_hidden_when_empty').checked,
        workspaceRoot: document.querySelector('#workspace_root').value,
      });
      const autostart = await invoke('autostart_set', {
        enabled: document.querySelector('#autostart').checked,
      });
      document.querySelector('#autostart').checked = autostart.autostart_enabled;
      showAutostartNote(autostart.autostart_mechanism);
      say('本机设置已保存。');
      await loadSettings();
      invoke('float_sync').catch(() => {});
    } catch (error) {
      say(String(error));
    }
  };

  document.querySelector('#workspace-reset').onclick = async () => {
    document.querySelector('#workspace_root').value = '';
    try {
      await invoke('settings_set', { workspaceRoot: '' });
      await loadSettings();
      say('已恢复默认工作目录。');
    } catch (error) {
      say(String(error));
    }
  };

  document.querySelector('#trust').onsubmit = async (event) => {
    event.preventDefault();
    try {
      await invoke('trust_origin', {
        origin: document.querySelector('#origin').value.trim(),
      });
      document.querySelector('#origin').value = '';
      say('服务地址已保存；设备连接状态见服务列表。');
      await renderOrigins();
    } catch (error) {
      say(String(error));
    }
  };

  document.querySelector('#host-register').onclick = async () => {
    try {
      const outcome = await invoke('register_host', { takeover: true });
      say(
        outcome.backed_up
          ? `Host 已注册，原注册已备份到 ${outcome.previous_manifest_path}。`
          : 'Host 已注册。',
      );
      await Promise.all([loadSettings(), loadSetup()]);
    } catch (error) {
      say(String(error));
    }
  };

  document.querySelector('#host-rollback').onclick = async () => {
    try {
      say(`已回退到原注册：${await invoke('rollback_host')}`);
      await Promise.all([loadSettings(), loadSetup()]);
    } catch (error) {
      say(String(error));
    }
  };

  document.querySelector('#host-unregister').onclick = async () => {
    try {
      await invoke('unregister_host');
      say('本机 Host 已卸载；不会删除本地工作副本。');
      await Promise.all([loadSettings(), loadSetup()]);
    } catch (error) {
      say(String(error));
    }
  };

  document.querySelector('#update-extension').onclick = async () => {
    try {
      say(await invoke('update_extension'));
      await loadSetup();
    } catch (error) {
      say(String(error));
    }
  };

  document.querySelector('#check-update').onclick = async () => {
    try {
      const result = await invoke('check_update');
      if (!result.update_source_set) {
        say('尚未配置升级地址；开发版不自动检查。');
        return;
      }
      if (!result.update_available) {
        say(result.update_error ? `检查未通过：${result.update_error}` : '当前已是最新版本。');
        return;
      }
      document.querySelector('#update-title').textContent = `发现新版本 ${result.latest_version}`;
      document.querySelector('#update-notes').textContent = result.latest_notes || '暂无版本说明。';
      updateBanner.hidden = false;
      say('发现可用升级；确认后再下载。');
    } catch (error) {
      say(String(error));
    }
  };

  document.querySelector('#update-apply').onclick = async () => {
    try {
      let result = await invoke('run_update', { force: false });
      if (result.state === 'blocked') {
        // Exactly the exit dialog's trade-off: an upgrade stops the process
        // that renews Checkouts, so it is never silent when something is at risk.
        const risks = (result.assessment && result.assessment.risks) || [];
        const detail = risks
          .slice(0, 4)
          .map((risk) => `${risk.file_name}：${REASON_TEXT[risk.reasons[0]] || risk.reasons[0]}`)
          .join('\n');
        const proceed = window.confirm(
          `仍有任务未处理，升级会中断后台续租：\n${detail}\n\n本地工作副本与待确认的 Intent 都会保留。是否继续升级？`,
        );
        if (!proceed) {
          say('已取消升级；当前版本保持不变。');
          return;
        }
        result = await invoke('run_update', { force: true });
      }
      say(stateText(result));
      updateBanner.hidden = true;
    } catch (error) {
      say(String(error));
    }
  };

  document.querySelector('#update-dismiss').onclick = () => {
    updateBanner.hidden = true;
  };

  document.querySelector('#exit-cancel').onclick = () => {
    exitBanner.hidden = true;
    say('已取消退出；后台续租继续运行。');
  };

  document.querySelector('#exit-force').onclick = async () => {
    try {
      await invoke('quit_app', { force: true });
    } catch (error) {
      say(String(error));
    }
  };

  listen('tasks-changed', () => refresh());
  listen('focus-task', async ({ payload }) => {
    setView('tasks');
    await refresh();
    const target = document.getElementById(payload);
    if (target) {
      target.classList.add('focused');
      target.scrollIntoView({ block: 'center' });
    }
  });
  // The tray and the window ask the same question, so the banner is driven by
  // the process, not by this script's own guess.
  listen('confirm-exit', ({ payload }) => {
    showExitBanner(payload);
    refresh();
  });
}

async function start() {
  wire();
  try {
    const state = await invoke('setup_state');
    renderSetup(state);
    // 首次运行（没走完引导）直接停在引导页；之后进任务中心，
    // 引导仍可从左侧打开做修复。
    showView(state.setup_completed ? 'tasks' : 'setup');
  } catch (error) {
    say(String(error));
    showView('tasks');
  }
  await Promise.all([
    loadSettings().catch((error) => say(String(error))),
    renderOrigins().catch((error) => say(String(error))),
    refresh(),
  ]);
  renderExitState();
  setInterval(() => {
    refresh();
    renderExitState();
    renderOrigins().catch(() => {});
  }, 10000);
}

start();
