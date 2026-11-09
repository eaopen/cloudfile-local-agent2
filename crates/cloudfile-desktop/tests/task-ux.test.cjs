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

// 一张卡片只占一行状态：模式名和「不回传」原来分两行说同一件事，卡片被撑得很高。
test('one summary line states the mode and its write-back rule once', () => {
  for (const [overrides, expected] of [
    [{ mode: 'local-view', can_upload: false }, /^本地打开 · 不会回传$/],
    [{ mode: 'local-view', can_upload: false, local_changed: true }, /^本地打开 · 本地副本已改，不会回传$/],
    [{}, /^普通编辑 · 与云端一致$/],
    [{ local_changed: true }, /^普通编辑 · 已有改动，待上传$/],
    [{ mode: 'exclusive-edit', exclusive: true }, /^独占编辑 · 无待提交改动$/],
    [{ mode: 'exclusive-edit', exclusive: true, local_changed: true }, /^独占编辑 · 有改动待提交$/],
  ]) {
    const line = ux.summary(task(overrides), false);
    assert.match(line, expected);
    assert.doesNotMatch(line, /\n/);
    // 同一句「不会回传」不许出现两次。
    assert.ok((line.match(/不会回传|不会自动上传/g) || []).length <= 1, line);
  }
});

// 异常状态整行接管：这些文案本身就是用户要做的事，前面再挂模式名就又变回两行。
test('problem states replace the summary line instead of stacking under it', () => {
  assert.match(ux.summary(task({ pending_upload: true, local_changed: true }), false), /上传结果未确认/);
  assert.match(ux.summary(task({ needs_review: true }), false), /需要检查/);
  assert.match(ux.summary(task({ local_changed: true }), true), /正在上传/);
  assert.match(ux.summary(task({ exclusive: true, mode: 'exclusive-edit', has_lease: false }), false), /尚未就绪/);
  assert.match(ux.summary(task({
    exclusive: true, mode: 'exclusive-edit', authorization_expires_at: 1,
  }), false, 9999999999), /已过期/);
  // 普通状态不会被误判成异常。
  assert.doesNotMatch(ux.summary(task({ mode: 'local-view', can_upload: false }), false), /未确认|需要检查/);
});

// 底行要能区分「同一个文件名在哪个库」。现场就是 /10220942/a.dta 这种纯路径，
// 三个库里的 a.dta 长得一模一样。
test('source line surfaces the library so same-named files stay distinguishable', () => {
  const shared = {
    service: 'http://10.9.8.162:6111', remote_path: '/10220942/a.dta_for_mod1',
  };
  assert.equal(ux.sourceLine(task(shared)), '10.9.8.162:6111 · 库 10220942 · a.dta_for_mod1');
  // 有服务端库名时优先用它；路径仍按库内相对路径显示，避免前缀重复刷屏。
  assert.equal(
    ux.sourceLine(task({ ...shared, library_name: '方案设计库' })),
    '10.9.8.162:6111 · 库 方案设计库 · 10220942/a.dta_for_mod1',
  );
  // 首段是普通目录名（现场那个 `..临时存档`）时不能当库名显示。
  assert.equal(
    ux.sourceLine(task({ service: 'http://10.9.8.162:6111', remote_path: '/..临时存档/顾文涛/a.xls' })),
    '10.9.8.162:6111 · ..临时存档/顾文涛/a.xls',
  );
  assert.equal(ux.sourceTitle(task(shared)), 'http://10.9.8.162:6111/10220942/a.dta_for_mod1');
});
