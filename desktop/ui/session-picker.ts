import type { CaptureDraft, CapturePreview, CaptureSession, SubmissionResult } from './types';
import { api, dateTime, el, empty, esc, notify, showModal } from './shared';

// Serialize local selection writes, including a close followed immediately by another collection.
let selectionSave: Promise<void> = Promise.resolve();

export async function selectSessions(day: string): Promise<SubmissionResult | null> {
  selectionSave = selectionSave.catch(() => {});
  await selectionSave;
  let draft = await api<CaptureDraft>('member.collect', { day });
  let selected = new Set(draft.sessions.filter(session => session.selected).map(session => session.id));
  let activeId = draft.sessions.find(session => selected.has(session.id))?.id || draft.sessions[0]?.id || '';
  let alive = true, busy = false, previewRequest = 0;
  let submission: SubmissionResult | null = null;
  let saveMessage = '选择仅保存在本机', actionError = '';

  const dialog = showModal('选择要上传的会话', `<div class="capture-heading"><div><strong id="capture-date"></strong><p id="capture-cutoff" class="muted"></p><p class="muted">当前内容已脱敏。确认后仅上传所选会话，新增记录需刷新采集。</p></div><button type="button" id="capture-recollect">刷新采集</button></div><div id="capture-warnings"></div><div class="capture-layout"><section class="capture-sidebar" aria-label="会话选择"><label class="sr-only" for="capture-search">搜索会话或项目</label><input id="capture-search" type="search" placeholder="搜索会话或项目"><label class="sr-only" for="capture-project">按项目筛选</label><select id="capture-project"></select><div class="capture-list-tools"><button type="button" id="capture-select-all">全选当前列表</button><button type="button" id="capture-select-none">取消当前列表</button></div><div id="capture-list" class="capture-list"></div></section><section id="capture-preview" class="capture-preview" aria-label="待上传内容预览"></section></div><div class="capture-footer"><div><strong id="capture-selected-count"></strong><small id="capture-save-state" class="muted"></small></div><div class="button-group"><button type="button" id="capture-cancel">取消</button><button type="button" id="capture-confirm" class="primary"></button></div></div>`, true);
  dialog.classList.add('capture-dialog');

  const sessionName = (session: CaptureSession): string => session.title || session.id;
  const selectedIds = (): string[] => draft.sessions.filter(session => selected.has(session.id)).map(session => session.id);
  const filtered = (): CaptureSession[] => {
    const search = el<HTMLInputElement>('capture-search').value.trim().toLocaleLowerCase();
    const project = el<HTMLSelectElement>('capture-project').value;
    return draft.sessions.filter(session => (!project || session.project === project) && (!search || `${sessionName(session)} ${session.project}`.toLocaleLowerCase().includes(search)));
  };
  function footer(): void {
    const count = selected.size;
    const events = draft.sessions.filter(session => selected.has(session.id)).reduce((total, session) => total + session.eventCount, 0);
    el('capture-selected-count').textContent = `已选 ${count} / ${draft.sessions.length} 个会话 · ${events} 条记录`;
    el('capture-save-state').textContent = actionError || saveMessage;
    el('capture-save-state').className = actionError ? 'danger-text' : 'muted';
    el('capture-confirm').textContent = busy ? '处理中…' : count ? `确认上传 ${count} 个会话` : '提交空记录';
    const state = document.getElementById('capture-preview-selection');
    if (state) { state.textContent = selected.has(activeId) ? '已选，将上传' : '未选，不上传'; state.className = selected.has(activeId) ? 'badge good' : 'badge pending'; }
  }
  function setBusy(value: boolean): void {
    busy = value;
    dialog.querySelectorAll<HTMLInputElement | HTMLButtonElement | HTMLSelectElement>('button,input,select').forEach(node => { node.disabled = value; });
    dialog.setAttribute('aria-busy', String(value));
    footer();
  }
  function saveSelection(): void {
    const draftId = draft.draftId, sessionIds = selectedIds();
    saveMessage = '正在保存本日选择…'; footer();
    selectionSave = selectionSave.catch(() => {}).then(() => api<void>('member.saveSelection', { draftId, sessionIds }));
    const current = selectionSave;
    void current.then(() => {
      if (alive && current === selectionSave) { saveMessage = '本日选择已保存；新增会话需重新勾选'; footer(); }
    }, error => {
      if (alive && current === selectionSave) { saveMessage = `选择保存失败：${String(error)}`; footer(); }
    });
  }
  function renderList(): void {
    const list = el('capture-list'), scroll = list.scrollTop;
    const visible = filtered();
    list.innerHTML = visible.map(session => `<div class="capture-session ${session.id === activeId ? 'active' : ''} ${selected.has(session.id) ? 'selected' : ''}"><input type="checkbox" data-session-id="${esc(session.id)}" aria-label="选择会话：${esc(sessionName(session))}" ${selected.has(session.id) ? 'checked' : ''}><button type="button" data-preview-id="${esc(session.id)}" aria-pressed="${session.id === activeId}"><strong>${esc(sessionName(session))}</strong><small class="capture-project-name">${esc(session.project || '未标注项目')}</small><small>${esc(dateTime(session.firstAt, draft.timeZone))} — ${esc(dateTime(session.lastAt, draft.timeZone))}</small><span class="capture-session-tags"><span>${session.eventCount} 条记录</span>${session.isNew ? '<span class="capture-new">新增</span>' : ''}${session.warnings.length ? `<span class="danger-text">${session.warnings.length} 项采集提示</span>` : ''}</span></button></div>`).join('') || empty(draft.sessions.length ? '没有匹配的会话' : '未发现可选会话', draft.sessions.length ? '试试其他关键词或项目。' : '可以查看采集说明，或提交空记录。');
    list.scrollTop = scroll;
  }
  async function preview(id: string): Promise<void> {
    activeId = id; renderList();
    const session = draft.sessions.find(item => item.id === id);
    const panel = el('capture-preview');
    const request = ++previewRequest, draftId = draft.draftId;
    if (!session) { panel.innerHTML = empty('选择会话查看内容', '上传前可在这里核对完整的当日记录。'); return; }
    panel.innerHTML = '<p class="muted">正在读取本次采集的预览…</p>';
    try {
      const result = await api<CapturePreview>('member.previewSession', { draftId, sessionId: id });
      if (!alive || request !== previewRequest || draft.draftId !== draftId) return;
      const labels: Record<string, string> = { user: '用户消息', assistant: '助手回复', tool_call: '工具执行', tool_result: '工具结果' };
      const events = result.events.map(event => {
        const heading = `<strong>${esc(labels[event.kind] || event.kind)}</strong><time>${esc(dateTime(event.timestamp, draft.timeZone))}</time>`;
        const content = `<pre class="capture-event-text">${esc(event.text)}</pre><small class="capture-event-source">${esc(event.source)}</small>`;
        return event.kind.startsWith('tool_') ? `<details class="capture-event capture-tool"><summary>${heading}<span>展开完整内容</span></summary>${content}</details>` : `<article class="capture-event"><header>${heading}</header>${content}</article>`;
      }).join('');
      panel.innerHTML = `<div class="capture-preview-heading"><div><h3>${esc(sessionName(session))}</h3><p class="muted">${esc(session.project || '未标注项目')}</p></div><span id="capture-preview-selection"></span></div>${result.warnings.length ? `<div class="notice capture-session-warnings">${result.warnings.map(warning => `<p>${esc(warning)}</p>`).join('')}</div>` : ''}<div class="capture-events" aria-label="只读会话记录">${events || empty('该会话没有可上传事件', '采集提示仍会随选中的会话保留。')}</div>`;
      panel.scrollTop = 0; footer();
    } catch (error) {
      if (alive && request === previewRequest) panel.innerHTML = `<div class="notice error-notice">${esc(String(error))}</div>`;
    }
  }
  function renderCapture(): void {
    el('capture-date').textContent = `${draft.day} · 会话级筛选`;
    el('capture-cutoff').textContent = `采集截至 ${dateTime(draft.capturedAt, draft.timeZone)}`;
    el('capture-warnings').innerHTML = draft.warnings.length ? `<details class="capture-range"><summary>采集范围与提示 · ${draft.warnings.length}</summary>${draft.warnings.map(warning => `<p>${esc(warning)}</p>`).join('')}</details>` : '';
    const projects = [...new Set(draft.sessions.map(session => session.project).filter(Boolean))].sort();
    const project = el<HTMLSelectElement>('capture-project'), previous = project.value;
    project.innerHTML = `<option value="">全部项目</option>${projects.map(name => `<option value="${esc(name)}">${esc(name)}</option>`).join('')}`;
    if (projects.includes(previous)) project.value = previous;
    renderList(); footer(); void preview(activeId);
  }
  const changeSelection = (choose: boolean): void => {
    filtered().forEach(session => { if (choose) selected.add(session.id); else selected.delete(session.id); });
    renderList(); saveSelection();
  };
  el('capture-search').addEventListener('input', renderList);
  el('capture-project').addEventListener('change', renderList);
  el('capture-select-all').addEventListener('click', () => changeSelection(true));
  el('capture-select-none').addEventListener('click', () => changeSelection(false));
  el('capture-list').addEventListener('change', event => {
    const input = event.target as HTMLInputElement, id = input.dataset.sessionId;
    if (!id) return;
    if (input.checked) selected.add(id); else selected.delete(id);
    renderList(); saveSelection();
  });
  el('capture-list').addEventListener('click', event => {
    const button = (event.target as HTMLElement).closest<HTMLElement>('[data-preview-id]');
    if (button) void preview(button.dataset.previewId!);
  });
  el('capture-cancel').addEventListener('click', () => dialog.close());
  const onCancel = (event: Event): void => { if (busy) event.preventDefault(); };
  dialog.addEventListener('cancel', onCancel);
  el('capture-recollect').addEventListener('click', () => { void (async () => {
    actionError = '';
    setBusy(true);
    try {
      await selectionSave;
      draft = await api<CaptureDraft>('member.collect', { day: draft.day });
      selected = new Set(draft.sessions.filter(session => session.selected).map(session => session.id));
      if (!draft.sessions.some(session => session.id === activeId)) activeId = draft.sessions.find(session => selected.has(session.id))?.id || draft.sessions[0]?.id || '';
      saveMessage = '已刷新采集；新增会话默认不选'; renderCapture();
    } catch (error) { actionError = `刷新未完成：${String(error)}`; }
    finally { if (alive) setBusy(false); }
  })(); });
  el('capture-confirm').addEventListener('click', () => { void (async () => {
    if (busy) return;
    actionError = '';
    setBusy(true);
    try {
      await selectionSave;
      submission = await api<SubmissionResult>('member.submit', { draftId: draft.draftId, sessionIds: selectedIds() });
      dialog.close();
    } catch (error) { actionError = `上传未完成：${String(error)}`; }
    finally { if (alive) setBusy(false); }
  })(); });
  renderCapture();
  return new Promise(resolve => {
    dialog.addEventListener('close', () => {
      alive = false; ++previewRequest; dialog.removeEventListener('cancel', onCancel);
      void selectionSave.then(() => resolve(submission), error => { notify(`本日选择未保存：${String(error)}`, true); resolve(submission); });
    }, { once: true });
  });
}
