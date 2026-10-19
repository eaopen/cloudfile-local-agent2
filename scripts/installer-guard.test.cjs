const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

for (const script of ['install.cmd', 'install.dev.cmd']) {
  test(script + ' rejects a network bootstrap without a pinned digest', () => {
    // The scripts retain their original Windows encoding. The test only reads
    // ASCII control-flow instructions.
    const contents = fs.readFileSync(path.join(__dirname, script), 'latin1');
    const digestGate = contents.indexOf('if not defined CLOUDFILE_INSTALL_BOOTSTRAP_SHA256');
    const networkFetch = contents.indexOf('call :fetch');
    assert.ok(digestGate > 0 && networkFetch > digestGate);
    assert.match(contents, /Invoke-WebRequest/);
    assert.match(contents, /Get-FileHash -Algorithm SHA256/);
    assert.match(contents, /-ine \$want/);
    assert.match(contents, /call :verify/);
    assert.doesNotMatch(contents, /ExecutionPolicy Bypass/i);
    assert.doesNotMatch(contents, /LSS 1000000/i);
  });
}
