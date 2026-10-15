const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

test('dev packaging preserves the popup and uses the isolated Host without a pairing button', () => {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'cloudfile-extension-'));
  try {
    const source = path.join(temporary, 'source');
    const output = path.join(temporary, 'output');
    fs.mkdirSync(source);
    const html = '<body><main>CloudFile</main><script src="popup.js"></script></body>';
    fs.writeFileSync(path.join(source, 'popup.html'), html);
    fs.writeFileSync(path.join(source, 'background.js'), "const host = 'com.cloudfile.agent2';");
    fs.writeFileSync(path.join(source, 'manifest.json'), JSON.stringify({ name: 'CloudFile', permissions: ['nativeMessaging'] }));
    execFileSync(process.platform === 'win32' ? 'python' : 'python3', [
      path.join(__dirname, 'prepare-extension.py'), '--extension-src', source, '--out', output,
    ]);
    assert.equal(fs.readFileSync(path.join(output, 'popup.html'), 'utf8'), html);
    assert.equal(fs.existsSync(path.join(output, 'dev-pairing.js')), false);
    assert.match(fs.readFileSync(path.join(output, 'background.js'), 'utf8'), /com\.cloudfile\.agent22\.dev/);
    assert.deepEqual(JSON.parse(fs.readFileSync(path.join(output, 'manifest.json'))).permissions, ['nativeMessaging']);
    assert.match(fs.readFileSync(path.join(source, 'background.js'), 'utf8'), /com\.cloudfile\.agent2'/);
  } finally {
    fs.rmSync(temporary, { recursive: true, force: true });
  }
});
