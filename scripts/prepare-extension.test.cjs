const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

test('dev packaging preserves Windows fixed Host and isolates macOS Rust Host', () => {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'cloudfile-extension-'));
  try {
    const source = path.join(temporary, 'source');
    const output = path.join(temporary, 'output');
    fs.mkdirSync(source);
    const html = '<body><main>CloudFile</main><script src="popup.js"></script></body>';
    fs.writeFileSync(path.join(source, 'popup.html'), html);
    fs.writeFileSync(path.join(source, 'background.js'), [
      "const AGENT2_HOST = 'com.cloudfile.agent2';",
      "const CURRENT_HOST = 'com.cloudfile.current_agent';",
      "const HOST = 'com.cloudfile.local_agent';"
    ].join('\n'));
    fs.writeFileSync(path.join(source, 'manifest.json'), JSON.stringify({ name: 'CloudFile', permissions: ['nativeMessaging'] }));
    execFileSync(process.platform === 'win32' ? 'python' : 'python3', [
      path.join(__dirname, 'prepare-extension.py'), '--extension-src', source, '--out', output,
    ]);
    assert.equal(fs.readFileSync(path.join(output, 'popup.html'), 'utf8'), html);
    assert.equal(fs.existsSync(path.join(output, 'dev-pairing.js')), false);
    const generated = fs.readFileSync(path.join(output, 'background.js'), 'utf8');
    assert.match(generated, /const AGENT2_HOST = 'com\.cloudfile\.agent22\.dev';/);
    assert.match(generated, /const CURRENT_HOST = 'com\.cloudfile\.current_agent';/);
    assert.match(generated, /const HOST = 'com\.cloudfile\.local_agent';/);
    assert.doesNotMatch(generated, /const CURRENT_HOST = 'com\.cloudfile\.agent22\.dev';/);
    assert.deepEqual(JSON.parse(fs.readFileSync(path.join(output, 'manifest.json'))).permissions, ['nativeMessaging']);
    assert.match(fs.readFileSync(path.join(source, 'background.js'), 'utf8'), /const AGENT2_HOST = 'com\.cloudfile\.agent2';/);
  } finally {
    fs.rmSync(temporary, { recursive: true, force: true });
  }
});


test('source Host contract change fails before replacing a previously prepared copy', () => {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'cloudfile-extension-'));
  try {
    const source = path.join(temporary, 'source');
    const output = path.join(temporary, 'output');
    fs.mkdirSync(source);
    fs.mkdirSync(output);
    fs.writeFileSync(path.join(output, 'last-good.txt'), 'preserve');
    fs.writeFileSync(path.join(source, 'manifest.json'), '{"name":"CloudFile"}');
    fs.writeFileSync(path.join(source, 'background.js'), [
      "const AGENT2_HOST = 'com.cloudfile.agent2';",
      "const CURRENT_HOST = 'com.cloudfile.unexpected';"
    ].join('\n'));
    assert.throws(() => execFileSync(process.platform === 'win32' ? 'python' : 'python3', [
      path.join(__dirname, 'prepare-extension.py'), '--extension-src', source, '--out', output
    ], { stdio: 'pipe' }));
    assert.equal(fs.readFileSync(path.join(output, 'last-good.txt'), 'utf8'), 'preserve');
    assert.equal(fs.existsSync(path.join(output, 'background.js')), false);
  } finally {
    fs.rmSync(temporary, { recursive: true, force: true });
  }
});
