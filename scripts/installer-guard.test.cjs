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
    // 2026-11-09 shipped Windows compatibility path: external PowerShell is
    // only a last-resort downloader, not the mandatory hashing engine.
    // The old tests incorrectly required Get-FileHash and made CI fail even
    // though real Win10/11 customer installers already use certutil.
    assert.match(contents, /set "CF_CURL=%SystemRoot%\\System32\\curl\.exe"/);
    assert.match(contents, /set "CF_CERTUTIL=%SystemRoot%\\System32\\certutil\.exe"/);
    assert.match(contents, /-hashfile "%~1" SHA256/);
    assert.match(contents, /call :hash_file "%TARGET%"/);
    assert.match(contents, /if \/i not "%CF_HASH%"=="%CLOUDFILE_INSTALL_BOOTSTRAP_SHA256%"/);
    assert.match(contents, /call :verify/);
    assert.match(contents, /exit \/b 3/);
    assert.doesNotMatch(contents, /ExecutionPolicy Bypass/i);
    assert.doesNotMatch(contents, /LSS 1000000/i);
  });
}
