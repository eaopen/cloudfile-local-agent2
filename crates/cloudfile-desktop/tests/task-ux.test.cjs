const test = require('node:test');
const assert = require('node:assert/strict');
const ux = require('../ui/task-ux.js');

function task(overrides = {}) {
  return {
    file_name: 'test.docx', mode: 'optimistic-edit', exclusive: false,
    can_upload: true, local_changed: false, pending_upload: false,
    needs_review: false, has_lease: true, lease_acknowledged: true,
    checkout_closed: false, ...overrides,
  };
}

test('three modes communicate actual write-back behavior', () => {
  assert.match(ux.modeLabel(task({mode:'local-view', can_upload:false})), /不自动回传/);
  assert.match(ux.modeLabel(task()), /普通编辑.*手动上传/);
  assert.doesNotMatch(ux.modeLabel(task()), /自动回传/);
  assert.match(ux.modeLabel(task({mode:'exclusive-edit', exclusive:true})), /手动提交/);
  assert.match(ux.status(task({mode:'local-view',local_changed:true}), false), /不会自动上传/);
});

test('uncertain publication stays highest priority even during token expiry and broken state', () => {
  const broken = task({exclusive:true,pending_upload:true,pending_action:'checkin',
    needs_review:true,authorization_expires_at:1});
  assert.match(ux.status(broken, false, 9999999999), /签入结果未确认/);
  assert.doesNotMatch(ux.status(broken, false, 9999999999), /过期/);
});

// The caller passes a boolean uploading flag, not the retired auto-upload settings.
test('ordinary edits remain pending until a user uploads and the task is confirmed', () => {
  assert.match(ux.status(task({local_changed:true}), false), /尚未上传.*立即上传/);
  assert.match(ux.status(task({local_changed:true}), true), /正在上传新版本/);
  assert.match(ux.status(task({needs_review:true,local_changed:true}), false), /需要检查/);
  assert.match(ux.status(task({pending_upload:true,local_changed:true}), false), /上传结果未确认/);
  assert.match(ux.status(task(), false), /与上次确认的云端版本一致/);
});

test('exclusive release is deliberately explicit and never deletes a local copy', () => {
  assert.match(ux.releaseWarning(task({exclusive:true,local_changed:true})), /不会自动上传/);
  assert.match(ux.releaseWarning(task({exclusive:true})), /确认释放/);
  assert.equal(ux.releaseWarning(task({exclusive:true,pending_upload:true})),null);
  assert.equal(ux.releaseWarning(task({exclusive:false})),null);
});

test('design files warn about references while ordinary documents stay quiet', () => {
  assert.match(ux.referenceHint('ASSEMBLY.SLDASM'), /参照/);
  assert.match(ux.referenceHint('drawing.dwg'), /仅管理此文件/);
  assert.equal(ux.referenceHint('report.xlsx'), '');
});
