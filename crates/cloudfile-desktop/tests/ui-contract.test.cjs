const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const ui = path.join(__dirname, '../ui');
const html = fs.readFileSync(path.join(ui, 'index.html'), 'utf8');
const script = fs.readFileSync(path.join(ui, 'app.js'), 'utf8');
const css = fs.readFileSync(path.join(ui, 'style.css'), 'utf8');

class FakeElement {
  constructor(tag = 'div') {
    this.tag = tag;
    this.children = [];
    this.className = '';
    this.hidden = false;
    this.textContent = '';
    this.classList = { toggle() {}, add() {}, remove() {} };
    this.dataset = {};
  }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
}

const declaredIds = new Set(
  [...html.matchAll(/\bid\s*=\s*"([^"]+)"/g)].map(match => match[1])
);

function fakeApp(serviceList, options = {}) {
  const calls = [];
  const confirmations = [];
  const elements = new Map(
    [...declaredIds].map(id => [id, new FakeElement()])
  );
  const document = {
    createElement: tag => new FakeElement(tag),
    querySelector: selector => {
      if (selector === 'main' || selector === '.header-actions') return new FakeElement();
      if (selector === '.advanced') return new FakeElement();
      if (selector.startsWith('#')) return elements.get(selector.slice(1)) ?? null;
      return null;
    },
    querySelectorAll: selector =>
      selector === '[data-view]' ? [] : [],
    getElementById: id => elements.get(id) ?? null,
  };
  const ctx = {
    document,
    window: {
      __TAURI__: {
        core: { invoke: async (name, args) => {
          calls.push({ name, args });
          if (name === 'service_status') return serviceList;
          if (name === 'settings_set') return { version: 'test' };
          if (name === 'autostart_set') return { autostart_enabled: Boolean(args && args.enabled), autostart_mechanism: null };
          if (name === 'task_summary') return {
            ok: true, tasks: options.tasks || [],
            counts: { 'in-progress': 0, 'needs-attention': 0, done: 0 },
          };
          if (name === 'setup_state') return {
            host: 'com.cloudfile.current_agent',
            host_foreign: Boolean(options.hostForeign),
            version: '2.2.0-alpha.2', install_root: '/test/bin',
            trusted_count: 0, trusted_ready: false,
            workspace_exists: false, workspace_root: '/test/workspace',
            host_registered: !options.hostForeign,
            autostart_enabled: false,
            extension_version: null, extension_dir: '/test/extension',
            extension_id: 'abcdefghijklmnopabcdefghijklmnop',
            setup_completed: false,
          };
          if (name === 'register_host') return {
            backed_up: Boolean(options.hostForeign), previous_manifest_path: '/test/previous.json'
          };
          if (name === 'settings_get') return {
            version: '2.2.0-alpha.2', install_root: '/test/bin',
            autostart_enabled: false, autostart_mechanism: null,
            registered_host: { registered: false, foreign: false, host: 'com.cloudfile.agent22.dev' },
            settings: {
              notify_on_saved: true, notify_on_review: true, float_enabled: true,
              float_hidden_when_empty: false, workspace_root: '/test/workspace'
            }
          };
          if (name === 'app_profile') {
            if (!options.appProfile) throw new Error('unexpected invocation: ' + name);
            return options.appProfile;
          }
          if (name === 'set_profile_environment') {
            if (!options.switchTo) throw new Error('unexpected invocation: ' + name);
            return options.switchTo(args && args.useTest);
          }
          throw new Error('unexpected invocation: ' + name);
        } },
        event: { listen() {} },
      },
      CloudFileTaskUX: {},
      confirm: (message) => {
        confirmations.push(message);
        return Boolean(options.confirmTakeover);
      },
    },
    setInterval() {},
  };
  vm.createContext(ctx);
  // Exercise the actual shipped JS, excluding only its asynchronous startup.
  vm.runInContext(script.replace(/\nstart\(\);\s*$/, '\n'), ctx, { filename: 'app.js' });
  return { ctx, elements, calls, confirmations };
}

test('all literal JS element IDs and navigation views exist in the HTML', () => {
  const referenced = [...script.matchAll(
    /(?:querySelector|querySelectorAll|getElementById)\(\s*['"]#([\w-]+)['"]/g
  )].map(match => match[1]);
  assert.deepEqual(
    [...new Set(referenced.filter(id => !declaredIds.has(id)))],
    [],
    'app.js references missing HTML IDs'
  );
  // 服务连接页已删除（服务地址内置，不需要界面配置）。
  for (const name of ['tasks', 'settings', 'setup']) {
    assert.ok(declaredIds.has('view-' + name));
    assert.match(html, new RegExp('data-view="' + name + '"'));
  }
});

test('task centre binds all controls before connecting to any service', () => {
  const { ctx } = fakeApp([]);
  assert.doesNotThrow(() => vm.runInContext('wire()', ctx));
});

test('settings load renders autostart state and host state without missing elements', async () => {
  const { ctx, elements } = fakeApp([]);
  await vm.runInContext('loadSettings()', ctx);
  assert.equal(elements.get('autostart_note').hidden, true);
  assert.equal(elements.get('workspace_root').value, '/test/workspace');
  assert.match(elements.get('host-state').textContent, /尚未注册/);
  assert.match(elements.get('version').textContent, /2.2.0/);
});

// 应用信息（ApplicationProfile V1）：有绑定就显示站点品牌，没有就保持 CloudFile。
// 这两条钉住「品牌只是显示」——拿不到 profile 时设置页必须照常可用。
// 两套配置：开关决定当前生效哪一套，状态行要能说清"哪个环境、开了哪些功能"。
function profileFixture(useTest) {
  const environment = useTest ? 'test' : 'prod';
  return {
    use_test: useTest,
    brand: { application_name: useTest ? '测试助手' : '生产助手',
      profile_id: 'etech-' + environment, environment, stale: false },
    source: `http://${environment}.example.test/prod-api/admin-api/local-bridge/profile/v1`,
    configured_source: null,
    active: { profile_id: 'etech-' + environment, environment, stale: false,
      features: ['cloudfile'], web_entries: [] },
    profiles: [{ profile_id: 'etech-prod', application_name: '生产助手',
      environment: 'prod', expires_at: 4102444800, stale: false }],
  };
}

// 地址不允许手改：它由当前环境决定，手工填只会让界面与实际不同步。
// 「保存设置 / 恢复默认目录」属于工作副本目录那一块，收进折叠里之后仍要在同一块内。
test('the save row lives inside the workspace section', () => {
  const section = html.match(
    /<details class="settings-group"><summary>工作副本目录<\/summary>[\s\S]*?<\/details>/,
  );
  assert.ok(section, '找不到工作副本目录这一块');
  assert.match(section[0], /保存设置/);
  assert.match(section[0], /恢复默认目录/);
  assert.equal((html.match(/class="form-footer"/g) || []).length, 1, '不能留两份按钮行');
});

// 按钮藏进折叠块后，上面两个分区的开关必须改了即时保存，否则用户找不到保存入口。
test('toggling a switch saves immediately', async () => {
  const { ctx, elements, calls } = fakeApp([]);
  vm.runInContext('wire()', ctx);
  const toggle = elements.get('notify_on_saved');
  toggle.checked = true;
  await toggle.onchange();
  assert.ok(
    calls.some((call) => call.name === 'settings_set' && call.args.notifyOnSaved === true),
    '开关变化应当立刻写入设置',
  );
});

// 设置页的偏好项默认折叠，和「关于与更新」里的高级项保持一致（页面别一进来就拉得很长）。
test('the preference sections are collapsed by default', () => {
  for (const title of ['通知提醒', '常驻与启动', '工作副本目录']) {
    assert.match(
      html,
      new RegExp(`<details class="settings-group"><summary>${title}</summary>`),
      `${title} 应当是默认折叠的 details`,
    );
  }
  assert.doesNotMatch(html, /<div class="settings-group"><h3>/, '不要再留展开的旧结构');
});

// 引导页五步必须一行一条。曾经和欢迎卡共用 .setup-steps 类，三列网格把第 3 步顶出界面。
test('the setup step list is one row per step', () => {
  assert.doesNotMatch(css, /\.setup-steps\{display:grid/);
  assert.match(css, /ol\.setup-steps\{/);
  assert.match(css, /\.welcome-steps\{display:grid/);
});

test('the application profile address is read-only in the markup', () => {
  assert.match(html, /id="app-profile-source"[^>]*readonly/);
  // 折叠标题就是「应用信息」，不再带「（测试 / 生产）」；也不再单独成块。
  assert.match(html, /<summary>应用信息<\/summary>/);
  assert.equal((html.match(/<summary>应用信息/g) || []).length, 1);
});

test('the active environment drives the brand and the sidebar', async () => {
  const { ctx, elements } = fakeApp([], { appProfile: profileFixture(false) });
  await vm.runInContext('loadSettings()', ctx);
  assert.match(elements.get('brand-name').textContent, /生产助手/);
  assert.match(elements.get('app-profile-state').textContent, /当前生效：生产环境/);
  assert.match(elements.get('app-profile-state').textContent, /开放功能：cloudfile/);
  assert.equal(elements.get('use_test_server').checked, false);
  // 输入框被当前环境的地址预填，用户点同步前就能看到会去问谁。
  assert.match(elements.get('app-profile-source').value, /profile\/v1$/);
  // 「左上角」不止侧栏：标题栏与版本行也要用同一个名字。
  // 三处都要看得出当前环境：左上角品牌、左下角版本、关于与更新里的版本行。
  // 左上角只放品牌名（带环境会撑破侧栏），环境只出现在两处版本行上。
  assert.equal(ctx.document.title, '生产助手 · 任务中心');
  assert.equal(elements.get('brand-name').textContent, '生产助手');
  assert.match(elements.get('version').textContent, /^生产助手 · 2\.2\.0.*（生产）$/);
  assert.match(elements.get('sidebar-version').textContent, /^v2\.2\.0.*（生产）$/);
});

test('ticking the test switch loads the test configuration and its features', async () => {
  const { ctx, elements } = fakeApp([], {
    appProfile: profileFixture(false),
    switchTo: (useTest) => ({ ok: true, use_test: useTest, sync_error: null, profile: profileFixture(useTest) }),
  });
  vm.runInContext('wire()', ctx);
  await vm.runInContext('loadSettings()', ctx);
  assert.equal(elements.get('use_test_server').checked, false);

  await elements.get('use_test_server').onchange({ target: { checked: true } });
  assert.match(elements.get('brand-name').textContent, /测试助手/);
  assert.match(elements.get('app-profile-state').textContent, /当前生效：测试环境/);
  assert.match(elements.get('app-profile-hint').textContent, /已切换到测试环境/);
  // 地址是推出来的、不可手改：切换环境后必须自动换成对应环境那条。
  assert.ok(elements.get('app-profile-source').value.startsWith('http://test.example.test/'),
    elements.get('app-profile-source').value);

  // 切回生产：只应剩下生产那套。
  await elements.get('use_test_server').onchange({ target: { checked: false } });
  assert.match(elements.get('brand-name').textContent, /生产助手/);
});

// 拉取失败不是切换失败：必须继续用本地缓存，并把原因说清楚。
test('a failed pull still switches and says it kept the local copy', async () => {
  const { ctx, elements } = fakeApp([], {
    appProfile: profileFixture(false),
    switchTo: (useTest) => ({ ok: true, use_test: useTest,
      sync_error: 'Profile discovery unavailable', profile: profileFixture(useTest) }),
  });
  vm.runInContext('wire()', ctx);
  await vm.runInContext('loadSettings()', ctx);
  await elements.get('use_test_server').onchange({ target: { checked: true } });
  assert.match(elements.get('app-profile-hint').textContent, /继续使用本地配置/);
  assert.match(elements.get('brand-name').textContent, /测试助手/);
});

test('without a profile the brand stays CloudFile and the page says it is not synced', async () => {
  const { ctx, elements } = fakeApp([], {
    appProfile: { use_test: false, brand: null, source: null, configured_source: null, active: null, profiles: [] },
  });
  await vm.runInContext('loadSettings()', ctx);
  assert.equal(elements.get('brand-name').textContent, 'CloudFile');
  assert.match(elements.get('app-profile-state').textContent, /尚未同步/);
});

// 读不到应用信息（老 Agent / 配置损坏）时必须静默降级，不能把整个设置页拖垮。
test('a failing application profile read never breaks the settings page', async () => {
  const { ctx, elements } = fakeApp([]);
  await vm.runInContext('loadSettings()', ctx);
  // 没有 profile 时不改写品牌节点，页面显示的仍是 HTML 里的静态兜底名。
  assert.equal(elements.get('brand-name').textContent, '');
  assert.match(html, /<span id="brand-name">CloudFile<\/span>/);
  assert.match(elements.get('version').textContent, /2.2.0/);
});

// 悬浮入口与轻提示是独立 webview，各自用 `app_brand` 取同一个名字。
// 这里只钉「不再写死品牌文字」——它们没有可用的 DOM 环境跑整段脚本。
test('float and toast titles are driven by the shared brand command', () => {
  const toast = fs.readFileSync(path.join(ui, 'toast.js'), 'utf8');
  const float = fs.readFileSync(path.join(ui, 'float.js'), 'utf8');
  for (const [name, source, wanted] of [
    ['toast.js', toast, '${brand} · 已保存到云端'],
    ['float.js', float, '${brand} 文件任务'],
  ]) {
    assert.match(source, /app_brand/, name + ' 没有读取品牌名');
    assert.ok(source.includes(wanted), name + ' 没有用品牌名拼标题');
    assert.doesNotMatch(source, /'CloudFile [·文]/, name + ' 仍然写死 CloudFile');
  }
});

test('setup status renders first-run steps without registering an external service', () => {
  const { ctx, elements } = fakeApp([]);
  vm.runInContext(`renderSetup({
    version: '2.2.0-alpha.2', install_root: '/test/bin',
    trusted_ready: false, trusted_count: 0,
    workspace_exists: false, workspace_root: '/test/workspace',
    host_registered: false, host_foreign: false, host: 'com.cloudfile.agent22.dev',
    autostart_enabled: false, extension_version: null, extension_dir: '/test/extension',
    extension_id: 'abcdefghijklmnopabcdefghijklmnop', setup_completed: false
  })`, ctx);
  // 服务地址内置后引导少了一步（不再有「受信任的 CloudFile 服务」）。
  assert.equal(elements.get('setup-steps').children.length, 5);
  assert.equal(elements.get('setup-ext-dir').textContent, '/test/extension');
  assert.doesNotMatch(html, /服务连接/);
  assert.doesNotMatch(html, /打开 Chrome 扩展管理页/);
});


test('Host repair never takes over a foreign controller without explicit consent', async () => {
  const { ctx, elements, calls, confirmations } = fakeApp([], { hostForeign: true });
  vm.runInContext('wire()', ctx);
  await elements.get('host-register').onclick();
  assert.equal(confirmations.length, 1);
  assert.match(confirmations[0], /旧|\.NET/);
  assert.equal(calls.some(call => call.name === 'register_host'), false);
  assert.match(elements.get('message').textContent, /已取消/);
});

test('Host takeover passes true only after the user confirms an existing foreign controller', async () => {
  const { ctx, elements, calls, confirmations } = fakeApp(
    [], { hostForeign: true, confirmTakeover: true }
  );
  vm.runInContext('wire()', ctx);
  await elements.get('host-register').onclick();
  assert.equal(confirmations.length, 1);
  const registrations = calls.filter(call => call.name === 'register_host');
  assert.equal(registrations.length, 1);
  assert.equal(registrations[0].args.takeover, true);
});

test('ordinary Host re-registration never needs a takeover confirmation', async () => {
  const { ctx, elements, calls, confirmations } = fakeApp([]);
  vm.runInContext('wire()', ctx);
  await elements.get('host-register').onclick();
  assert.equal(confirmations.length, 0);
  const registrations = calls.filter(call => call.name === 'register_host');
  assert.equal(registrations.length, 1);
  assert.equal(registrations[0].args.takeover, false);
});
