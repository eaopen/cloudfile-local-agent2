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
