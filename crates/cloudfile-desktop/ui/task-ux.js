// Presentation-only policies. Nothing here grants permission, starts uploads,
// releases locks or overrides the journal/remote state.
(function (root) {
  'use strict';
  const CAD = new Set(['dwg', 'dxf', 'sldprt', 'sldasm', 'slddrw', 'prt', 'asm',
    'iam', 'ipt', 'idw', 'ipn', 'rvt', 'rfa', 'step', 'stp', 'igs', 'iges', '3dm',
    'catpart', 'catproduct', 'nwd', 'nwc', 'nwf', 'indd']);

  function modeLabel(task) {
    if (task.mode === 'local-view') return '本地打开 · 不自动回传';
    if (task.exclusive) return '独占编辑 · 手动提交';
    if (task.can_upload) return '普通编辑 · 本地保存后自动回传';
    return '旧版本地会话 · 仅可打开';
  }
  function referenceHint(name) {
    const ext = String(name || '').split('.').pop().toLowerCase();
    if (CAD.has(ext)) {
      return '设计文件可能依赖其他参照文件；当前任务仅管理此文件，不会自动同步整个项目。';
    }
    return '';
  }
  function status(task, automatic, working, now = Date.now()) {
    // An uncertain publish takes priority over everything else. Never imply
    // that an expired ticket or a missing lease erased a pending write.
    if (task.pending_upload) {
      if (task.pending_action === 'checkin') return '签入结果未确认 · 本地副本已保留，请先查询';
      if (task.pending_action === 'commit') return '提交结果未确认 · 本地副本已保留，请先查询';
      return '上传结果未确认 · 本地副本已保留，请先查询';
    }
    if (task.needs_review) return '任务状态需要检查 · 不会自动覆盖本地副本';
    if (working) return task.exclusive ? '正在提交新版本…' : '正在上传新版本…';
    if (task.mode === 'local-view') {
      return task.local_changed
        ? '本地查看副本已被修改 · 不会自动上传云端'
        : '本地查看副本 · 不会自动上传云端';
    }
    if (task.exclusive) {
      if (task.checkout_closed) return '已签入并释放独占编辑；本地副本仍保留';
      if (!task.has_lease || !task.lease_acknowledged)
        return '独占授权尚未就绪 · 请从网页重新打开文件';
      if (task.authorization_expires_at) {
        const remaining = task.authorization_expires_at * 1000 - now;
        if (remaining <= 0) return '编辑授权已过期 · 本地文件仍保留，请从网页重新授权';
        if (remaining < 10 * 60 * 1000) return '编辑授权即将过期 · 建议从网页重新授权';
      }
      return task.local_changed
        ? '本地已有修改 · 可提交版本，或签入后释放独占'
        : '独占编辑中 · 无待提交的本地修改';
    }
    if (automatic && automatic.pause_reason) return '自动回传已暂停 · 本地文件仍保留';
    if (task.local_changed) {
      return automatic?.enabled
        ? '本地已有修改 · 等待保存稳定后自动回传'
        : '本地已有修改 · 自动回传未启用';
    }
    if (automatic?.last_synced_at) {
      return '云端已确认新版本 · ' +
        new Date(automatic.last_synced_at * 1000).toLocaleTimeString();
    }
    return '本地内容与上次确认的云端版本一致';
  }
  function releaseWarning(task) {
    if (!task.exclusive || task.checkout_closed || task.pending_upload) return null;
    if (task.local_changed) {
      return '此文件还有未提交的本地修改。释放独占后其他人可能修改云端文件；本地副本会保留，但不会自动上传。\n\n建议先“提交版本”或“签入并释放”。仍然只释放独占权限吗？';
    }
    return '确认释放此文件的独占权限？本地文件会保留，其他用户随后可以编辑云端版本。';
  }
  const policy = { modeLabel, referenceHint, status, releaseWarning };
  root.CloudFileTaskUX = policy;
  if (typeof module !== 'undefined' && module.exports) module.exports = policy;
})(typeof window !== 'undefined' ? window : globalThis);
