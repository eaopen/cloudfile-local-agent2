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

function fakeApp(serviceList) {
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
        core: { invoke: async name => {
          if (name === 'service_status') return serviceList;
          throw new Error('unexpected invocation: ' + name);
        } },
        event: { listen() {} },
      },
      CloudFileTaskUX: {},
    },
    setInterval() {},
  };
  vm.createContext(ctx);
  // Exercise the actual shipped JS, excluding only its asynchronous startup.
  vm.runInContext(script.replace(/\nstart\(\);\s*$/, '\n'), ctx, { filename: 'app.js' });
  return { ctx, elements };
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
  for (const name of ['tasks', 'services', 'settings', 'setup']) {
    assert.ok(declaredIds.has('view-' + name));
    assert.match(html, new RegExp('data-view="' + name + '"'));
  }
});

test('task centre binds all controls before connecting to any service', () => {
  const { ctx } = fakeApp([]);
  assert.doesNotThrow(() => vm.runInContext('wire()', ctx));
});

test('added services render without losing per-service remove action', async () => {
  const services = [
    { origin: 'https://files.example.com', service_url: 'https://files.example.com/seafile', state: 'unpaired', default: false },
    { origin: 'https://cloud.example.com', service_url: 'https://cloud.example.com', state: 'paired', default: false },
  ];
  const { ctx, elements } = fakeApp(services);
  await vm.runInContext('renderOrigins()', ctx);
  const rows = elements.get('origins').children;
  assert.equal(rows.length, 2);
  assert.equal(rows[0].children[0].children[0].textContent, services[0].service_url);
  assert.equal(rows[1].children[0].children[0].textContent, services[1].service_url);
  assert.ok(rows[0].children.some(child => child.className === 'row'));
  assert.ok(rows[1].children.some(child => child.className === 'row'));
});
