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
    if (task.can_upload) return '普通编辑 · 本地保存后手动上传';
    return '旧版本地会话 · 仅可打开';
  }
  function referenceHint(name) {
    const ext = String(name || '').split('.').pop().toLowerCase();
    if (CAD.has(ext)) {
      // 这是一句「同步范围」的说明，不是失败原因。原来的「可能依赖其他参照文件」
      // 在打不开的时候会被读成「所以打不开」，现场就误解过一次。
      return '设计文件可能引用其他零件或图纸，本任务仅管理此文件，不会同步参照件。';
    }
    return '';
  }
  function status(task, working, now = Date.now()) {
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
    // 自动上传已整体移除（2026-10-27）：本地改完不会自己往外写，只能由用户手动上传。
    if (task.local_changed) return '本地已有修改 · 尚未上传，可在任务里点「立即上传」';
    return '本地内容与上次确认的云端版本一致';
  }
  function releaseWarning(task) {
    if (!task.exclusive || task.checkout_closed || task.pending_upload) return null;
    if (task.local_changed) {
      return '此文件还有未提交的本地修改。释放独占后其他人可能修改云端文件；本地副本会保留，但不会自动上传。\n\n建议先“提交版本”或“签入并释放”。仍然只释放独占权限吗？';
    }
    return '确认释放此文件的独占权限？本地文件会保留，其他用户随后可以编辑云端版本。';
  }
  // 卡片只留一行状态：模式名 + 真正变化的那件事。
  //
  // 修改逻辑（2026-11-04）：原来「本地打开 · 不自动回传」和
  // 「本地查看副本 · 不会自动上传云端」是两行，说的是同一件事，又各自占一行，
  // 一张只读任务的卡片要占掉四五行。现在合成一句，并且只在一处说「不会回传」。
  //
  // 异常状态（待确认 / 需检查 / 授权问题 / 上传中）**优先整行显示原文**：
  // 那才是用户要做的事，前面再挂一个模式名只会把一句有用的提示挤成两行。
  const PROBLEM = /未确认|需要检查|正在上传|正在提交|尚未就绪|已过期|即将过期/;

  function summary(task, working, now = Date.now()) {
    const state = status(task, working, now);
    if (PROBLEM.test(state)) return state;
    if (task.mode === 'local-view') {
      return task.local_changed ? '本地打开 · 本地副本已改，不会回传' : '本地打开 · 不会回传';
    }
    if (task.exclusive) {
      return task.local_changed ? '独占编辑 · 有改动待提交' : '独占编辑 · 无待提交改动';
    }
    if (!task.can_upload) return '旧会话 · 仅可打开';
    return task.local_changed ? '普通编辑 · 已有改动，待上传' : '普通编辑 · 与云端一致';
  }

  // 库标识：优先用服务端给的库名（`library_name`），没有就退到路径首段的
  // EAP 库号（`/10220942/a.dta` 里的 `10220942`）。
  //
  // 为什么不能一律取首段：现场有 `/..临时存档/顾文涛/x.xls` 这种路径，首段是目录名，
  // 当库名显示就是错的。只认「纯数字且够长」的形态，其余情况宁可不显示。
  //
  // 真正的库名现在链路里没有：网页发起本地会话时只传 server / ticket / remote_path，
  // 票据里的 `sub` 是 `10220942@auth.local`。要显示「方案设计库」这类名字，
  // 需要服务端在票据或会话消息里带上库名 —— 这里先把 `library_name` 的位子留好。
  function libraryLabel(task) {
    const named = String(task.library_name || '').trim();
    if (named) return named;
    const first = String(task.remote_path || '').replace(/^\/+/, '').split('/')[0];
    return /^\d{4,}$/.test(first) ? first : '';
  }

  function hostOf(service) {
    return String(service || '').trim()
      .replace(/^https?:\/\//i, '')
      .replace(/\/+$/, '');
  }

  // 底行：`主机 · 库 <库标识> · <库内路径>`。
  // 只写路径时，不同库里的同名文件长得一模一样（现场 `/10220942/a.dta`），
  // 这就是「不好区分」的来源；把库单独提出来才有得比。
  function sourceLine(task) {
    const host = hostOf(task.service);
    const named = String(task.library_name || '').trim();
    const library = libraryLabel(task);
    let rest = String(task.remote_path || '');
    if (library && !named) {
      const prefix = '/' + library;
      if (rest === prefix) rest = '';
      else if (rest.startsWith(prefix + '/')) rest = rest.slice(prefix.length);
    }
    rest = rest.replace(/^\/+/, '');
    return [host, library ? '库 ' + library : '', rest].filter(Boolean).join(' · ');
  }

  // 悬停给出完整来源：压缩过的底行不能让人丢失「这份文件到底在哪」。
  function sourceTitle(task) {
    return [String(task.service || ''), String(task.remote_path || '')]
      .filter(Boolean).join('');
  }

  const policy = {
    modeLabel, referenceHint, status, releaseWarning,
    summary, libraryLabel, sourceLine, sourceTitle,
  };
  root.CloudFileTaskUX = policy;
  if (typeof module !== 'undefined' && module.exports) module.exports = policy;
})(typeof window !== 'undefined' ? window : globalThis);
