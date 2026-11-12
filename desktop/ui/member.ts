import type { AppInfo, Invitation, MemberInit, MemberState, Pairing, Report, SourceStatus } from './types';
import { actions, api, ask, badge, button, consent, dateTime, el, empty, esc, folder, notify, perform, reportHtml, savePdf, showModal } from './shared';
import { selectSessions } from './session-picker';

export async function mountMember(info: AppInfo): Promise<void> {
  const initial = await api<MemberInit>('member.init');
  let pairing = initial.pairing, sources = initial.sources || [], day = info.day;
  let state: MemberState = { queue: initial.queue || [], syncMessage: '', draft: '' };
  let displayed: Report | undefined, reportKey = '', refreshing = false, selecting = false;
  let draftTimer: ReturnType<typeof setTimeout>;
  const localDrafts = new Map<string, string>();
  let draftChain: Promise<unknown> = Promise.resolve();

  el('workspace').innerHTML = `<div class="page-heading"><div><div class="eyebrow">MEMBER WORKSPACE</div><h1>今日工作，清晰记录</h1><p id="identity" class="muted"></p></div><label class="date-field">报告日期<input id="day" type="date" value="${esc(day)}"></label></div><div class="toolbar"><div class="button-group">${button('member-submit', '采集并选择会话', 'primary')}${button('member-refresh', '刷新状态')}</div><div class="button-group">${button('member-pair', '绑定部门', 'quiet')}${button('member-root', '记录来源', 'quiet')}${button('member-queue', '待发送项', 'quiet')}</div></div><section id="member-status" class="status-strip" aria-live="polite"></section><div id="member-notices"></div><section class="paper"><div class="section-heading"><div><h2>工作日报</h2><span class="muted">组长生成并下发 · 正文只读</span></div><div>${button('member-warnings', '采集说明', 'quiet')}${button('member-download', '下载正式 PDF')}</div></div><div id="member-report" class="report-body"></div></section><section class="paper supplement"><div class="section-heading"><div><h2>个人补充说明</h2><p class="muted">复核正文后填写补充，提交内容将按原文附在报告末尾。</p></div><span id="draft-state" class="muted"></span></div><label class="sr-only" for="supplement">个人补充说明</label><textarea id="supplement" rows="5" placeholder="需要补充的进展、说明或修正…" disabled></textarea><div class="toolbar bottom-toolbar"><span id="feedback-state" class="muted"></span><div class="button-group">${button('member-draft', '保存草稿')}${button('member-none', '确认无补充')}${button('member-feedback', '提交补充', 'primary')}</div></div></section>`;

  function persistDraft(report: Report, text: string): Promise<unknown> {
    localDrafts.set(report.id, text);
    draftChain = draftChain.catch(() => {}).then(() => api('member.draft', { reportId: report.id, day: report.day, text }));
    return draftChain;
  }
  function saveDraft(): Promise<unknown> {
    const report = displayed;
    if (!report || report.finalizedAt) return draftChain;
    return persistDraft(report, el<HTMLTextAreaElement>('supplement').value);
  }
  function render(): void {
    el('identity').textContent = pairing ? `${pairing.identity.departmentName} / ${pairing.identity.memberName}` : '首次使用，请绑定组长提供的部门邀请。';
    const s = state.status;
    el('member-status').innerHTML = `${badge(s?.submissionStatus || '尚未提交', !!s?.receipt)}${badge(s?.reportStatus || '等待报告', !!s?.report?.finalizedAt)}<div class="status-copy"><span>${esc(state.syncMessage || (pairing ? '手动提交记录后，等待部门端接收。' : '绑定部门后即可提交工作记录。'))}</span>${s?.capturedAt ? `<small>采集截至 ${esc(dateTime(s.capturedAt, s.timeZone))} · ${s.sessionCount} 个会话</small>` : ''}${s?.analysisProgress ? `<small>${esc(s.analysisProgress)}</small>` : ''}</div>${state.queue.length ? `<span class="queue-count">${state.queue.length} 项待发送</span>` : ''}`;
    const notices = (s?.notices || []).filter(n => !n.fulfilled);
    el('member-notices').innerHTML = notices.map(n => `<div class="notice"><div><strong>需要重新提交 · ${esc(n.day)}</strong><p>${esc(n.reason)}</p></div>${button('member-notice', '查看该日', '', `data-day="${esc(n.day)}"`)}</div>`).join('') + (s && pairing && s.modelDestination !== pairing.identity.modelDestination ? `<div class="notice">部门模型处理地址已变更：${esc(s.modelDestination)}</div>` : '');
    const report = s?.report, key = JSON.stringify(report ?? null);
    if (reportKey !== key) {
      if (displayed?.id !== report?.id) {
        const old = displayed;
        if (old) {
          clearTimeout(draftTimer);
          localDrafts.set(old.id, el<HTMLTextAreaElement>('supplement').value);
          if (!old.finalizedAt) void perform(async () => { await saveDraft(); });
        }
        let draft = report ? (localDrafts.get(report.id) ?? state.draft ?? report.feedback?.text ?? '') : '';
        if (report && old && old.day === report.day && !localDrafts.has(report.id) && (state.draftFromPrevious || !draft)) {
          draft = localDrafts.get(old.id) ?? old.feedback?.text ?? '';
          if (!report.finalizedAt) void perform(async () => { await persistDraft(report, draft); });
          if (draft) notify('上一版补充已保留为草稿，请复核新正文后重新提交。');
        }
        el<HTMLTextAreaElement>('supplement').value = draft || report?.feedback?.text || '';
        if (state.draftFromPrevious && draft) notify('上一版补充已保留为草稿，请复核新正文后重新提交。');
        el('draft-state').textContent = '';
      }
      displayed = report; reportKey = key;
      el('member-report').innerHTML = report ? reportHtml(report) : empty('等待组长下发日报', '提交工作记录后，组长生成并下发的报告会显示在这里。');
    }
    const locked = !report || !!report.finalizedAt;
    el<HTMLTextAreaElement>('supplement').disabled = locked;
    document.querySelector<HTMLButtonElement>('[data-action="member-draft"]')!.disabled = locked;
    for (const action of ['member-none', 'member-feedback']) {
      const control = document.querySelector<HTMLButtonElement>(`[data-action="${action}"]`)!;
      control.dataset.stateDisabled = String(locked || !!state.feedbackBlockedReason);
      control.disabled = control.dataset.stateDisabled === 'true' || !!control.dataset.busy;
    }
    document.querySelector<HTMLButtonElement>('[data-action="member-download"]')!.disabled = !report?.finalizedAt;
    document.querySelector<HTMLButtonElement>('[data-action="member-submit"]')!.disabled = !pairing || selecting;
    el('feedback-state').textContent = state.feedbackBlockedReason || (report?.finalizedAt ? '已定稿。如需修改，请联系组长重新开启。' : report?.feedback ? '本版补充已由部门端接收' : '没有补充时，请选择“确认无补充”。');
  }
  async function refresh(): Promise<void> {
    if (refreshing || !pairing) { render(); return; }
    refreshing = true; const requestedDay = day;
    try { const result = await api<MemberState>('member.status', { day: requestedDay }); if (requestedDay === day) { state = result; render(); } }
    finally { refreshing = false; }
  }
  async function changeDay(next: string): Promise<void> {
    if (!next || next === day) return;
    clearTimeout(draftTimer); await saveDraft();
    day = next; el<HTMLInputElement>('day').value = day;
    state = { queue: state.queue, syncMessage: '', draft: '' }; displayed = undefined; reportKey = ''; render();
    // A previous poll can finish after the date changes; it is discarded above.
    while (refreshing) await new Promise(resolve => setTimeout(resolve, 30));
    await refresh();
  }
  el('day').addEventListener('change', () => { void perform(() => changeDay(el<HTMLInputElement>('day').value)); });
  el('supplement').addEventListener('input', () => {
    el('draft-state').textContent = '正在保存草稿…'; clearTimeout(draftTimer);
    draftTimer = setTimeout(() => { void perform(async () => { await saveDraft(); el('draft-state').textContent = '草稿已保存在本机'; }); }, 450);
  });
  actions['member-refresh'] = refresh;
  actions['member-notice'] = target => changeDay(target.dataset.day!);
  actions['member-submit'] = async () => {
    if (selecting) return;
    selecting = true; notify('正在本机采集，随后选择要上传的会话…');
    try {
      const submitted = await selectSessions(day);
      if (!submitted) return;
      await refresh();
      notify(submitted.received ? '部门端已接收所选会话记录。' : submitted.syncMessage || '所选记录已保存到本机队列，等待部门端接收。');
    } finally { selecting = false; render(); }
  };
  actions['member-draft'] = async () => { clearTimeout(draftTimer); await saveDraft(); el('draft-state').textContent = '草稿已保存在本机'; };
  const feedback = async (none: boolean): Promise<void> => {
    const report = displayed;
    if (!report || report.day !== day || report.finalizedAt) throw new Error('当前没有可提交补充的报告。');
    if (state.feedbackBlockedReason) throw new Error(state.feedbackBlockedReason);
    const text = none ? '无补充说明' : el<HTMLTextAreaElement>('supplement').value;
    if (!text.trim()) throw new Error('请填写补充，或选择“确认无补充”。');
    el<HTMLTextAreaElement>('supplement').value = text; await saveDraft();
    const result = await api<{ syncMessage?: string }>('member.feedback', { reportId: report.id, text, noSupplement: none || text.trim() === '无补充说明' }); await refresh();
    notify(result?.syncMessage || state.syncMessage || '补充已提交；接收结果请查看状态。');
  };
  actions['member-none'] = () => feedback(true); actions['member-feedback'] = () => feedback(false);
  actions['member-root'] = () => {
    const draft = sources.map(source => ({ ...source }));
    const dialog = showModal('记录来源', `<p class="muted">成员端只读取下列本机来源中当天的可见对话、工具动作和结果；系统提示、内部推理和内部记录不采集。未勾选的来源不会读取。</p><div id="source-list"></div><p class="muted small">保存后下次采集生效。“Trae 本地记录”读取国际版 Trae 的明文工作区记录；“Trae 导出”需要先在 Trae（含 Trae CN）中导出对话文件，再选择导出文件所在目录。导出文件按角色段落解析，缺少时间戳时按文件日期计入，上传前请在预览中核对。选择目录可以指定该来源的自定义记录位置。</p><div class="modal-actions"><button type="button" id="source-save" class="primary">保存来源</button></div>`, true);
    const renderRows = (): void => {
      el('source-list').innerHTML = draft.map(source => `<div class="source-row"><label class="source-toggle"><input type="checkbox" data-source-tool="${esc(source.tool)}" ${source.enabled ? 'checked' : ''}><strong>${esc(source.label)}</strong></label><small class="${source.detected ? 'muted' : 'danger-text'}">${source.detected ? `已检测到：${esc(source.effectivePath)}` : (source.tool === 'trae' ? '尚未选择 Trae 导出目录' : source.tool === 'traeLocal' ? '未检测到国际版 Trae 工作区' : '未检测到本机记录目录')}</small><div class="source-actions"><span class="mono small">${esc(source.path || '默认位置')}</span><button type="button" data-source-path="${esc(source.tool)}">${source.tool === 'trae' ? '选择导出目录' : '选择目录'}</button></div></div>`).join('');
      dialog.querySelectorAll<HTMLInputElement>('[data-source-tool]').forEach(input => input.addEventListener('change', () => {
        const source = draft.find(item => item.tool === input.dataset.sourceTool);
        if (source) source.enabled = input.checked;
      }));
      dialog.querySelectorAll<HTMLButtonElement>('[data-source-path]').forEach(node => node.addEventListener('click', () => { void (async () => {
        const source = draft.find(item => item.tool === node.dataset.sourcePath);
        if (!source) return;
        const next = await folder(`选择${source.label}${source.tool === 'trae' ? '导出文件' : '记录'}目录（当前：${source.effectivePath || '未选择'}）`);
        if (!next) return;
        source.path = next; source.effectivePath = next; source.detected = true; renderRows();
      })(); }));
    };
    renderRows();
    el('source-save').addEventListener('click', () => { void perform(async () => {
      const result = await api<{ sources: SourceStatus[] }>('member.sources', { sources: draft.map(source => ({ tool: source.tool, enabled: source.enabled, path: source.path })) });
      sources = result.sources; notify('记录来源已保存，下次采集生效。'); dialog.close();
    }); });
  };
  actions['member-pair'] = async () => {
    const raw = await ask('绑定部门', '粘贴组长提供的完整配对邀请（30 分钟内有效）', '', true); if (!raw?.trim()) return;
    let invite: Invitation;
    try { invite = JSON.parse(new TextDecoder().decode(Uint8Array.from(atob(raw.trim()), c => c.charCodeAt(0)))); }
    catch { throw new Error('邀请格式无效，请复制完整的邀请内容。'); }
    if (invite.protocol !== 1 || !invite.url || !invite.fingerprint || !invite.code) throw new Error('邀请不完整或版本不兼容。');
    if (!await consent('确认部门身份与数据范围', `<dl class="info-list"><dt>部门</dt><dd>${esc(invite.department)}</dd><dt>接收地址</dt><dd>${esc(invite.url)}</dd><dt>模型处理地址</dt><dd>${esc(invite.modelDestination)}</dd><dt>时区</dt><dd>${esc(invite.timeZone)}</dd><dt>证书指纹</dt><dd class="mono">${esc(invite.fingerprint)}</dd></dl><p>提交内容包括已启用本机来源（如 Codex、DeepSeek Harness、国际版 Trae 本地明文记录）中用户与助手可见对话、工具动作及结果，以及手动导入的 Trae 导出文件，可能包含代码和路径。不会主动读取项目文件、附件和登录配置；系统提示、内部推理和内部记录不采集。Trae 导出文件按角色段落解析，缺少时间戳时按文件日期计入。凭据脱敏不能保证覆盖所有秘密。</p><p>请核对邀请来自可信组长。确认后，这些内容会交给该部门及其模型服务处理。</p>`, '同意并绑定')) return;
    pairing = await api<Pairing>('member.pair', { invitation: raw.trim() }); await refresh(); notify('部门绑定成功。');
  };
  actions['member-download'] = async () => {
    const report = displayed; if (!report?.finalizedAt || report.day !== day) throw new Error('报告尚未定稿。');
    const path = await savePdf(`${report.day}-工作日报-V${report.version}.pdf`); if (!path) return;
    const result = await api<{ path: string }>('member.download', { reportId: report.id, path }); await api('app.openPath', { path: result.path }); notify('正式 PDF 已保存。');
  };
  actions['member-warnings'] = () => { const summary = sources.map(source => `<p><strong>${esc(source.label)}</strong> ${esc(source.enabled ? (source.detected ? `读取 ${source.effectivePath}` : '已启用但未检测到记录目录') : '未启用')}</p>`).join(''); showModal('采集说明', `${summary}${(state.status?.captureWarnings || []).map(x => `<p>${esc(x)}</p>`).join('') || '<p>尚无采集范围说明。</p>'}`); };
  const queueModal = (): void => {
    showModal('待发送项', state.queue.length ? state.queue.map(item => `<div class="queue-item"><div><strong>${item.kind === 'upload' ? '工作记录' : '个人补充'}</strong><small>${esc(dateTime(item.createdAt))}</small><p>${esc(item.error || '等待发送')}</p></div><div class="button-group">${button('member-retry', '重试', '', `data-id="${esc(item.id)}"`)}${button('member-discard', '移除', 'quiet danger-text', `data-id="${esc(item.id)}"`)}</div></div>`).join('') : empty('没有待发送内容', '只有部门端确认接收后，提交才算完成。'));
  };
  actions['member-queue'] = queueModal;
  actions['member-retry'] = async target => { await api('member.retry', { id: target.dataset.id }); await refresh(); queueModal(); };
  actions['member-discard'] = async target => {
    const id = target.dataset.id;
    if (!await consent('移除待发送项', '<p>该内容将不再自动发送。个人补充草稿会保留在本机。</p>', '确认移除')) return;
    await api('member.discard', { id }); await refresh(); queueModal();
  };
  render(); await refresh();
  setInterval(() => { if (!document.hidden) void perform(refresh); }, 15000);
}
