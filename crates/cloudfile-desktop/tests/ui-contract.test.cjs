const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const ui = path.join(__dirname, '../ui');
const html = fs.readFileSync(path.join(ui, 'index.html'), 'utf8');
const script = fs.readFileSync(path.join(ui, 'app.js'), 'utf8');

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
test('a bound application profile replaces the sidebar brand and names its source', async () => {
  const { ctx, elements } = fakeApp([], {
    appProfile: {
      brand: { application_name: 'Etech文件助手', profile_id: 'etech-prod',
        environment: 'prod', stale: false },
      source: 'http://10.9.8.162:6111/admin-api/local-bridge/profile/v1',
      configured_source: null,
      profiles: [{ profile_id: 'etech-prod', application_name: 'Etech文件助手',
        environment: 'prod', expires_at: 4102444800, stale: false }],
    },
  });
  await vm.runInContext('loadSettings()', ctx);
  assert.match(elements.get('brand-name').textContent, /Etech文件助手/);
  assert.match(elements.get('app-profile-state').textContent, /Etech文件助手.*prod/);
  assert.match(elements.get('app-profile-state').textContent, /有效至/);
  // 输入框被默认地址预填，用户点同步前就能看到会去问谁。
  assert.match(elements.get('app-profile-source').value, /profile\/v1$/);
  // 「左上角」不止侧栏：标题栏与版本行也要用同一个名字。
  assert.equal(ctx.document.title, 'Etech文件助手 · 任务中心');
  assert.match(elements.get('version').textContent, /^Etech文件助手 · 2\.2\.0/);
});

test('without a profile the brand stays CloudFile and the page says it is not synced', async () => {
  const { ctx, elements } = fakeApp([], {
    appProfile: { brand: null, source: null, configured_source: null, profiles: [] },
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
