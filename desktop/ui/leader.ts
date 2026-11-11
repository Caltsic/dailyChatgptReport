import type { AppInfo, Dashboard, DashboardRow, Job, Report, Settings } from './types';
import { actions, api, ask, badge, backupFile, button, consent, dateTime, el, empty, esc, folder, notify, perform, reportHtml, showModal } from './shared';

type NetworkAddress = { name: string; address: string };
type Initialization = { settings?: Settings; fingerprint: string; serviceError?: string; networkAddresses: NetworkAddress[]; networkError?: string };
export async function mountLeader(info: AppInfo): Promise<void> {
  let init = await api<Initialization>('leader.init'), settings = init.settings;
  let day = info.day, activeTab = settings ? 'dashboard' : 'settings', refreshing = false;
  let data: Dashboard = { rows: [], jobs: [], summaryStale: false }, jobId = '';
  const selected = new Set<string>(), cancelling = new Set<string>();
  el('workspace').innerHTML = `<div class="page-heading"><div><div class="eyebrow">DEPARTMENT WORKSPACE</div><h1 id="department-heading"></h1><p id="service-status" class="muted"></p></div><label class="date-field">报告日期<input id="day" type="date" value="${esc(day)}"></label></div><nav class="tabs" aria-label="组长工作区"><button data-action="leader-tab" data-tab="dashboard">当日看板</button><button data-action="leader-tab" data-tab="jobs">Agent 执行 <span id="jobs-count"></span></button><button data-action="leader-tab" data-tab="settings">部门与模型配置</button></nav><section id="tab-dashboard"><div id="dashboard-stats" class="stat-grid"></div><section class="paper"><div class="section-heading"><div><h2>成员日报</h2><p class="muted">生成成功后自动下发，成员上线即可查看。</p><span id="selection-label" class="muted">未选成员时，批量操作作用于全部成员</span></div><div class="button-group">${button('leader-add', '新增成员')}${button('leader-invite', '配对邀请')}${button('leader-refresh', '刷新', 'quiet')}</div></div><div class="toolbar batch-toolbar"><div class="button-group">${button('leader-generate', '生成日报', 'primary')}${button('leader-preview', '预览选中报告')}${button('leader-resubmit', '要求重交')}${button('leader-reopen', '重新开启')}</div><div class="button-group">${button('leader-export', '导出 PDF')}${button('leader-revoke', '撤销设备', 'quiet danger-text')}</div></div><div class="table-scroll"><table class="members-table"><thead><tr><th><input id="select-all" type="checkbox" aria-label="选择全部成员"></th><th>成员</th><th>记录提交</th><th>日报状态</th><th>个人补充</th><th>Agent 进度</th><th>报告</th></tr></thead><tbody id="member-rows"></tbody></table></div><div id="dashboard-empty"></div></section><section class="paper"><div class="section-heading"><div><h2>部门汇总</h2><p id="summary-state" class="muted"></p></div><div class="button-group"><span id="summary-cancel"></span>${button('leader-summary-generate', '生成汇总')}${button('leader-summary-preview', '查看汇总')}${button('leader-summary-export', '导出 PDF')}</div></div></section></section><section id="tab-jobs" hidden><div class="section-heading"><div><h2>报告 Agent 执行</h2><p id="job-counts" class="muted"></p></div>${button('leader-refresh', '刷新')}</div><div class="jobs-layout"><div id="job-list" class="job-list"></div><section id="job-detail" class="paper job-detail"></section></div></section><section id="tab-settings" hidden></section>`;

  function renderIdentity(): void {
    el('department-heading').textContent = settings ? `${settings.name} · 工作日报` : '建立部门工作日报';
    el('service-status').textContent = init.serviceError ? `接收服务未启动：${init.serviceError}` : settings ? `接收服务运行中 · ${settings.advertisedUrl} · 关闭窗口后继续在托盘运行` : '完成部门配置后，邀请成员提交工作记录。';
    el('service-status').classList.toggle('danger-text', !!init.serviceError);
  }
  function setTab(tab: string): void {
    activeTab = tab;
    for (const name of ['dashboard', 'jobs', 'settings']) el(`tab-${name}`).hidden = name !== tab;
    document.querySelectorAll<HTMLElement>('[data-tab]').forEach(node => { node.classList.toggle('active', node.dataset.tab === tab); node.setAttribute('aria-current', String(node.dataset.tab === tab)); });
  }
  function selectionLabel(): void {
    el('selection-label').textContent = selected.size ? `已选 ${selected.size} 位成员` : '未选成员时，批量操作作用于全部成员';
    document.querySelector<HTMLButtonElement>('.batch-toolbar [data-action="leader-preview"]')!.disabled = selected.size !== 1 || !data.rows.find(row => selected.has(row.memberId))?.reportId;
    const check = el<HTMLInputElement>('select-all'); check.checked = !!data.rows.length && selected.size === data.rows.length; check.indeterminate = selected.size > 0 && selected.size < data.rows.length;
  }
  const jobState = (state: string): string => ({ queued: '排队中', running: '生成中', failed: '失败', completed: '已完成', cancelled: '已取消' })[state] || state;
  const owner = (job: Job): string => job.kind === 'department' ? '部门汇总' : data.rows.find(row => row.memberId === job.memberId)?.name || '成员日报';
  const activeJob = (job?: Job): boolean => !!job && ['queued', 'running'].includes(job.state);
  const jobBadge = (job: Job): string => job.state === 'cancelled' ? '<span class="badge neutral"><i></i>已取消</span>' : badge(jobState(job.state), job.state === 'completed');
  const cancelButton = (job?: Job, cls = 'quiet danger-text'): string => job && activeJob(job) ? button('leader-cancel', cancelling.has(job.id) ? '取消中…' : '取消生成', cls, `data-id="${esc(job.id)}"`, cancelling.has(job.id)) : '';
  const progress = (job: Job): string => job.state === 'cancelled' ? '已取消' : job.progress ? `${job.progress.stage}${job.progress.total > 0 ? ` · ${job.progress.completed}/${job.progress.total}` : ''}${job.progress.activity ? ` · ${job.progress.activity}` : ''}` : job.state === 'queued' ? '等待启动 Agent' : jobState(job.state);
  function percent(job: Job): number { return job.progress?.total ? Math.max(0, Math.min(100, job.progress.completed / job.progress.total * 100)) : job.state === 'completed' ? 100 : 0; }
  function pieceProgress(job: Job): string {
    const p = job.progress; if (!p?.pieces?.length) return '';
    const states: Record<string, string> = { queued: '等待', pending: '等待', waiting: '等待配额', retrying: '等待重试', running: '分析中', completed: '完成', cached: '已复用', failed: '失败', cancelled: '已停止' };
    const pieces = [...p.pieces].sort((a, b) => Number(['running', 'failed', 'waiting', 'retrying'].includes(b.state)) - Number(['running', 'failed', 'waiting', 'retrying'].includes(a.state)));
    return `<section class="piece-progress"><h3>分片进度</h3><p class="muted">已完成 ${p.completed || 0} / ${p.total || pieces.length} · 运行 ${job.state === 'cancelled' ? 0 : p.active || 0} · 失败 ${p.failed || 0}${p.estimatedTokens ? ` · 输入估算 ${p.estimatedTokens.toLocaleString()} Token` : ''}</p><div class="piece-list" tabindex="0"><table><thead><tr><th>分片</th><th>状态</th><th>说明</th></tr></thead><tbody>${pieces.map(piece => `<tr><td>${esc(piece.label || piece.id)}</td><td>${esc(job.state === 'cancelled' && ['running', 'queued', 'pending', 'waiting', 'retrying'].includes(piece.state) ? '已停止' : states[piece.state] || piece.state)}</td><td class="${piece.error ? 'danger-text' : ''}">${esc(piece.error || '')}</td></tr>`).join('')}</tbody></table></div></section>`;
  }
  function renderJobDetail(): void {
    const job = data.jobs.find(j => j.id === jobId);
    if (!job) { el('job-detail').innerHTML = empty('选择任务查看进度', '执行记录会自动刷新。'); return; }
    const p = job.progress;
    const end = job.state === 'running' ? Date.now() : new Date(job.finishedAt || p?.updatedAt || job.createdAt).getTime();
    const seconds = Math.max(0, Math.floor((end - new Date(p?.startedAt || job.createdAt).getTime()) / 1000));
    const detail = el('job-detail'), previousScroll = detail.scrollTop, pieceScroll = detail.querySelector('.piece-list')?.scrollTop || 0;
    detail.innerHTML = `<div class="section-heading"><div><h2>${esc(owner(job))}</h2><p class="muted">${esc(job.day)} · ${job.kind === 'department' ? '部门汇总' : '个人日报'}</p></div>${job.state === 'failed' ? button('leader-retry', '重试任务', 'primary', `data-id="${esc(job.id)}"`) : `<div class="button-group">${jobBadge(job)}${cancelButton(job)}</div>`}</div><p class="job-stage">${esc(progress(job))}</p><progress max="100" value="${percent(job)}" aria-label="阶段进度"></progress><p class="muted">耗时 ${Math.floor(seconds / 60)} 分 ${seconds % 60} 秒${p ? ` · 请求 ${p.requests || 0} · 重试 ${p.retries || 0}` : ''}</p>${job.state === 'failed' && job.error ? `<div class="notice error-notice"><strong>生成失败</strong><p>${esc(job.error)}</p></div>` : ''}${job.state !== 'cancelled' && p?.current ? `<p>${esc(p.current)}</p>` : ''}${pieceProgress(job)}<h3>执行记录</h3><ol class="event-log">${(p?.events || []).map(event => `<li><time>${esc(dateTime(event.at))}</time><span>${esc(event.message)}</span></li>`).join('') || '<li>尚无执行记录</li>'}</ol><p class="muted small mono">任务编号：${esc(job.id)}</p>`;
    if (job.state === 'completed') {
      const summary = job.kind === 'department';
      const action = summary ? 'leader-summary-preview' : 'leader-preview';
      const label = summary ? (job.reportId ? '预览本次汇总' : '预览当前汇总') : (job.reportId ? '预览本次日报' : '预览当前日报');
      detail.insertAdjacentHTML('beforeend', `<div class="modal-actions">${button(action, label, 'primary', `data-id="${esc(job.memberId || '')}" data-report-id="${esc(job.reportId || '')}"`)}</div>`);
    }
    detail.scrollTop = previousScroll; const pieceList = detail.querySelector('.piece-list'); if (pieceList) pieceList.scrollTop = pieceScroll;
  }
  function renderDashboard(): void {
    const ids = new Set(data.rows.map(row => row.memberId)); for (const id of selected) if (!ids.has(id)) selected.delete(id);
    el('dashboard-stats').innerHTML = [['部门成员', data.rows.length], ['已收到记录', data.rows.filter(row => row.snapshotVersion > 0).length], ['待成员反馈', data.rows.filter(row => row.reportStatus === '待反馈').length], ['已定稿', data.rows.filter(row => row.reportStatus === '已定稿').length]].map(([label, count]) => `<div class="stat"><span>${label}</span><strong>${count}<small>人</small></strong></div>`).join('');
    el('member-rows').innerHTML = data.rows.map(row => `<tr class="${selected.has(row.memberId) ? 'selected' : ''}"><td><input type="checkbox" data-member="${esc(row.memberId)}" aria-label="选择 ${esc(row.name)}" ${selected.has(row.memberId) ? 'checked' : ''}></td><td><strong>${esc(row.name)}</strong><small>记录 V${row.snapshotVersion || 0} · 日报 V${row.reportVersion || 0}</small></td><td>${badge(row.submissionStatus, row.snapshotVersion > 0 && !row.submissionStatus.includes('重交'))}<small>${esc(row.capturedAt || '尚无采集记录')}</small></td><td>${badge(row.reportStatus, row.reportStatus === '已定稿')}</td><td><span class="${row.feedbackStatus === '待反馈' || row.feedbackStatus === '未提交' ? 'danger-text' : ''}">${esc(row.feedbackStatus || '—')}</span></td><td class="progress-cell"><span>${esc(row.analysisProgress || '—')}</span>${row.jobId ? `<progress max="100" value="${Math.min(100, Math.max(0, row.stagePercent || 0))}" aria-label="${esc(row.name)} 生成进度"></progress>${button('leader-show-job', '查看任务', 'text-button', `data-id="${esc(row.jobId)}"`)}${cancelButton(data.jobs.find(job => job.id === row.jobId), 'text-button danger-text')}` : ''}</td><td>${button('leader-preview', '预览报告', 'text-button', `data-id="${esc(row.memberId)}"`, !row.reportId)}</td></tr>`).join('');
    el('dashboard-empty').innerHTML = !data.rows.length ? empty('从第一位成员开始', '新增成员，再创建邀请完成设备配对。') : '';
    selectionLabel();
    const summaryJob = data.jobs.find(job => job.kind === 'department' && ['queued', 'running'].includes(job.state));
    el('summary-cancel').innerHTML = cancelButton(summaryJob);
    el('summary-state').textContent = summaryJob ? progress(summaryJob) : data.summary ? `V${data.summary.version} · ${data.summaryStale ? '来源已更新，请重新生成' : '来源有效'}` : '将成员日报汇总为部门当日进展。';
    document.querySelector<HTMLButtonElement>('[data-action="leader-summary-preview"]')!.disabled = !data.summary;
    document.querySelector<HTMLButtonElement>('[data-action="leader-summary-export"]')!.disabled = !data.summary || data.summaryStale;
    el('jobs-count').textContent = String(data.jobs.filter(job => ['queued', 'running'].includes(job.state)).length || '');
    el('job-counts').textContent = `${day} · 生成中 ${data.jobs.filter(j => j.state === 'running').length} · 排队 ${data.jobs.filter(j => j.state === 'queued').length} · 失败 ${data.jobs.filter(j => j.state === 'failed').length}`;
    if (!data.jobs.some(job => job.id === jobId)) jobId = data.jobs[0]?.id || '';
    el('job-list').innerHTML = data.jobs.map(job => `<button type="button" data-action="leader-select-job" data-id="${esc(job.id)}" class="job-card ${job.id === jobId ? 'active' : ''}"><span class="job-card-heading"><strong>${esc(owner(job))}</strong>${jobBadge(job)}</span><span>${esc(progress(job))}</span><small>${esc(dateTime(job.createdAt))}</small>${job.state === 'failed' && job.error ? `<span class="danger-text">${esc(job.error)}</span>` : ''}</button>`).join('') || empty('还没有生成任务', '提交记录后，在看板中选择成员生成日报。');
    renderJobDetail();
  }
  async function refresh(): Promise<void> {
    if (refreshing || !settings) return;
    refreshing = true; const requestedDay = day;
    try { const next = await api<Dashboard>('leader.dashboard', { day: requestedDay }); if (day === requestedDay) { data = next; renderDashboard(); } }
    finally { refreshing = false; }
  }
  function targets(): DashboardRow[] { return selected.size ? data.rows.filter(row => selected.has(row.memberId)) : data.rows; }
  function selectedOne(): DashboardRow {
    if (selected.size !== 1) throw new Error('请勾选一位成员。'); return data.rows.find(row => selected.has(row.memberId))!;
  }
  async function batch(op: string, extra: Record<string, unknown> = {}): Promise<void> {
    if (!targets().length) throw new Error('请先新增成员。');
    const result = await api<{ errors: unknown[] }>(op, { memberIds: [...selected], day, ...extra }); await refresh();
    if (result.errors?.length) showModal('部分操作未完成', `<div class="notice">${result.errors.map(error => `<p>${esc(typeof error === 'string' ? error : JSON.stringify(error))}</p>`).join('')}</div>`);
    else notify('操作已完成。');
  }
  function renderSettings(): void {
    const s = settings;
    const field = (id: string, label: string, value: string | number, type = 'text', note = ''): string => `<label class="field" for="${id}"><span>${esc(label)}</span><input id="${id}" name="${id}" type="${type}" value="${esc(value)}" ${type === 'password' ? 'autocomplete="new-password"' : ''} ${id === 'setting-port' ? 'min="1" max="65535"' : ''} ${type !== 'password' ? 'required' : ''}>${note ? `<small>${esc(note)}</small>` : ''}</label>`;
    el('tab-settings').innerHTML = `<form id="settings-form" class="paper settings-form"><div class="section-heading"><div><h2>部门与接收服务</h2><p class="muted">部门独立收集与生成，成员通过内网或 VPN 提交。</p></div></div><div class="form-grid">${field('setting-name', '部门名称', s?.name || '')}${field('setting-zone', '时区', s?.timeZone || 'Asia/Shanghai', 'text', '已有记录后，时区不能更改。')}${field('setting-port', 'HTTPS 接收端口', s?.port || 18443, 'number')}<label class="field" for="setting-network"><span>成员连接网络</span><select id="setting-network"></select><small>默认自动使用内网 IP；跨网访问时选择对应 VPN 网卡。</small></label>${field('setting-address', '成员连接地址', s?.advertisedUrl || '', 'url')}<div class="field">${button('leader-detect-addresses', '重新检测 IP', 'quiet')}<small id="network-error" class="danger-text">${esc(init.networkError || '')}</small></div></div><div class="settings-divider"></div><h2>模型服务</h2><p class="muted">本端会将成员提交内容发往以下模型服务。API Key 仅在当前 Windows 用户下保存。</p><div class="form-grid">${field('setting-api', 'API Base URL', s?.apiBaseUrl || '', 'url')}${field('setting-model', '模型名称', s?.model || '')}${field('setting-key', 'API Key', '', 'password', '留空保留现有密钥。')}</div><details class="analysis-settings"><summary>长记录处理</summary><p class="muted">按 Token 估算决定是否分段，所有任务共用 4 个模型请求名额。规划值应以接口实际容量为准；未知时使用默认值，明确超限的分片会自动缩小。</p><div class="form-grid">${field('setting-context', '上下文规划值（Token）', s?.contextTokens || 65536, 'number')}${field('setting-reserve', '生成预留空间（Token）', s?.outputReserveTokens || 8192, 'number', '用于容量计算，不设置模型输出长度上限。')}${field('setting-chunk', '单片输入目标（Token）', s?.chunkTokens || 48000, 'number', '实际分片还会扣除提示词和消息包装。')}</div></details><div class="toolbar"><div class="button-group"><button type="submit" class="primary">保存并启动接收</button>${button('leader-test', '测试已保存模型配置')}</div>${button('leader-open-root', '打开数据目录', 'quiet')}</div><details><summary>部门身份与连接说明</summary><p class="mono small">证书指纹：${esc(init.fingerprint || '保存配置后生成')}</p><p class="muted">请为接收端口配置内网／VPN 防火墙规则。关闭窗口后仍在托盘接收，明确退出或关机后暂停。提示词随软件版本发布，不提供编辑入口。</p></details></form><section class="paper backup-panel"><div><h2>备份与恢复</h2><p class="muted">恢复写入新目录，保留当前数据。备份不包含 API Key 和可用设备凭据。</p></div><div class="button-group">${button('leader-backup', '创建备份')}${button('leader-restore', '恢复到新目录')}</div></section>`;
    const automatic = !s || s.addressMode === 'auto' || (!s.addressMode && /^https:\/\/(localhost|127\.0\.0\.1|\[::1\])(:|\/|$)/i.test(s.advertisedUrl));
    let choice = automatic ? 'auto' : 'manual';
    if (!automatic && s) choice = init.networkAddresses.find(a => s.advertisedUrl.replace(/\/$/, '') === `https://${a.address}:${s.port}`)?.address || 'manual';
    renderNetworks(choice);
    el('setting-network').addEventListener('change', updateAddress);
    el('setting-port').addEventListener('input', updateAddress);
    el('settings-form').addEventListener('submit', event => {
      event.preventDefault(); const submit = el('settings-form').querySelector<HTMLButtonElement>('[type="submit"]')!;
      void perform(async () => {
        const value = (id: string): string => el<HTMLInputElement>(`setting-${id}`).value.trim();
        const next: Settings = { id: s?.id || crypto.randomUUID().replace(/-/g, ''), name: value('name'), timeZone: value('zone'), port: Number(value('port')), advertisedUrl: value('address'), addressMode: el<HTMLSelectElement>('setting-network').value === 'auto' ? 'auto' : 'manual', apiBaseUrl: value('api'), model: value('model'), chunkCharacters: s?.chunkCharacters || 18000, contextTokens: Number(value('context')), outputReserveTokens: Number(value('reserve')), chunkTokens: Number(value('chunk')) };
        if (![next.contextTokens, next.outputReserveTokens, next.chunkTokens].every(v => Number.isInteger(v) && v! > 0) || next.outputReserveTokens! >= next.contextTokens!) throw new Error('容量规划值需为正整数，生成预留空间需小于上下文规划值。');
        if (!Number.isInteger(next.port) || next.port < 1 || next.port > 65535) throw new Error('端口应为 1–65535 的整数。');
        await api('leader.configure', { settings: next, apiKey: el<HTMLInputElement>('setting-key').value });
        el<HTMLInputElement>('setting-key').value = ''; init = await api<Initialization>('leader.init'); settings = init.settings; renderIdentity(); renderSettings();
        if (init.serviceError) notify(`配置已保存，接收服务启动失败：${init.serviceError}`, true);
        else { setTab('dashboard'); await refresh(); notify('配置已保存，接收服务已启动。'); }
      }, submit);
    });
  }
  function updateAddress(): void {
    const mode = el<HTMLSelectElement>('setting-network').value;
    const address = el<HTMLInputElement>('setting-address');
    const ip = mode === 'auto' ? init.networkAddresses[0]?.address : mode;
    address.readOnly = mode !== 'manual';
    if (mode !== 'manual') address.value = ip ? `https://${ip}:${el<HTMLInputElement>('setting-port').value}` : '';
    el('network-error').textContent = mode === 'auto' && !ip ? (init.networkError || '未检测到可用内网 IP，请连接网络后重新检测，或选择手动填写。') : '';
  }
  function renderNetworks(choice: string): void {
    const select = el<HTMLSelectElement>('setting-network');
    select.innerHTML = `<option value="auto">自动检测内网 IP${init.networkAddresses[0] ? ` · ${esc(init.networkAddresses[0].name)} · ${esc(init.networkAddresses[0].address)}` : ''}</option>${init.networkAddresses.map(a => `<option value="${esc(a.address)}">${esc(a.name)} · ${esc(a.address)}</option>`).join('')}<option value="manual">手动填写 IP 或主机名</option>`;
    select.value = choice;
    if (!select.value) select.value = 'manual';
    updateAddress();
  }
  actions['leader-detect-addresses'] = async () => {
    const result = await api<{ addresses: NetworkAddress[] }>('leader.networkAddresses');
    init.networkAddresses = result.addresses; init.networkError = undefined;
    renderNetworks(el<HTMLSelectElement>('setting-network').value);
    notify(result.addresses.length ? 'IP 已更新，保存后生效。' : '未检测到可用内网 IP。', !result.addresses.length);
  };
  actions['leader-tab'] = target => setTab(target.dataset.tab!);
  actions['leader-refresh'] = refresh;
  actions['leader-add'] = async () => { const name = await ask('新增成员', '成员姓名（部门内唯一）'); if (!name?.trim()) return; await api('leader.addMember', { name: name.trim() }); await refresh(); };
  actions['leader-invite'] = async () => {
    const row = selectedOne(); const result = await api<{ invitation: string; details: { url: string } }>('leader.invite', { memberId: row.memberId });
    if (settings) settings.advertisedUrl = result.details.url; renderIdentity();
    showModal(`${row.name} · 配对邀请`, `<p>邀请在 30 分钟内有效，请通过可信渠道发送完整内容。</p><p>成员连接地址：<strong>${esc(result.details.url)}</strong></p><label for="invitation">完整邀请</label><textarea id="invitation" rows="7" readonly class="mono">${esc(result.invitation)}</textarea><div class="modal-actions">${button('leader-copy-invite', '复制邀请', 'primary')}</div>`);
    el<HTMLTextAreaElement>('invitation').select();
  };
  actions['leader-copy-invite'] = async () => {
    const field = el<HTMLTextAreaElement>('invitation'); field.focus(); field.select();
    try { await navigator.clipboard.writeText(field.value); }
    catch { if (!document.execCommand('copy')) throw new Error('自动复制失败，已选中完整邀请，请按 Ctrl+C 复制。'); }
    notify('邀请已复制。');
  };
  actions['leader-revoke'] = async () => {
    if (!selected.size) throw new Error('请勾选需要撤销设备的成员。');
    if (!await consent('撤销选中成员设备', `<p>将撤销 ${selected.size} 位成员的设备连接。后续请求会被拒绝，可创建新邀请重新配对。</p>`, '确认撤销')) return;
    await api('leader.revoke', { memberIds: [...selected] }); await refresh(); notify('选中成员设备已撤销。');
  };
  actions['leader-generate'] = () => batch('leader.generate');
  actions['leader-resubmit'] = async () => { const reason = await ask('要求重新提交', `填写原因（${targets().length} 位成员）；已定稿成员需先重新开启。`, '', true); if (!reason?.trim()) return; await batch('leader.resubmit', { reason: reason.trim() }); };
  actions['leader-reopen'] = async () => {
    if (!await consent('重新开启日报', `<p>将重新开启 ${targets().length} 位成员的 ${esc(day)} 日报。后续需要生成新版，成员重新复核并提交补充。</p>`, '确认重新开启')) return;
    await batch('leader.reopen');
  };
  actions['leader-export'] = async () => {
    const rows = targets(); if (!rows.length) throw new Error('没有可导出的成员日报。');
    const directory = await folder(`导出 ${rows.length} 位成员的 PDF`); if (!directory) return;
    const errors: string[] = []; let count = 0;
    for (const row of rows) {
      try { if (!row.reportId) throw new Error('尚无报告'); await api('app.exportPersonal', { reportId: row.reportId, directory }); count++; }
      catch (error) { errors.push(`${row.name}：${String(error)}`); }
    }
    await refresh(); notify(`已导出并定稿 ${count} 份报告${errors.length ? `，${errors.length} 份未完成` : ''}。`);
    if (errors.length) showModal('导出结果', errors.map(error => `<p>${esc(error)}</p>`).join(''));
    if (count) await api('app.openPath', { path: directory });
  };
  actions['leader-preview'] = async target => {
    const id = target.dataset.id || selectedOne().memberId, row = data.rows.find(r => r.memberId === id);
    const reports = await api<Report[]>('leader.history', { memberId: id, day });
    if (!reports.length) throw new Error('该日尚无报告。');
    showModal(`${row?.name || '成员'} · 日报预览`, `<label for="history-version">报告版本</label><select id="history-version">${reports.map((report, index) => `<option value="${index}" ${report.id === target.dataset.reportId ? 'selected' : ''}>V${report.version} / 来源 V${report.snapshotVersion} / ${report.finalizedAt ? '已定稿' : report.releasedAt ? '已下发' : '待下发'} / ${esc(dateTime(report.createdAt))}</option>`).join('')}</select><div id="history-preview" class="report-body"></div>`, true);
    const draw = (): void => { el('history-preview').innerHTML = reportHtml(reports[Number(el<HTMLSelectElement>('history-version').value)]); }; draw(); el('history-version').addEventListener('change', draw);
  };
  actions['leader-select-job'] = target => { jobId = target.dataset.id!; renderDashboard(); };
  actions['leader-show-job'] = target => { jobId = target.dataset.id!; setTab('jobs'); renderDashboard(); };
  actions['leader-cancel'] = async target => {
    const id = target.dataset.id!; if (cancelling.has(id)) return;
    cancelling.add(id); renderDashboard();
    try {
      const job = await api<Job>('leader.cancel', { jobId: id });
      const index = data.jobs.findIndex(item => item.id === id); if (index >= 0) data.jobs[index] = job;
      await refresh();
      notify(job.state === 'cancelled' ? '生成已取消，可重新生成。' : `任务${jobState(job.state)}。`);
    } finally { cancelling.delete(id); renderDashboard(); }
  };
  actions['leader-retry'] = async target => { await api('leader.retry', { jobId: target.dataset.id }); await refresh(); notify('任务已重新排队。'); };
  actions['leader-summary-generate'] = async () => { const job = await api<Job>('leader.summary', { day }); jobId = job.id; await refresh(); setTab('jobs'); notify('部门汇总已加入生成队列。'); };
  actions['leader-summary-preview'] = async target => {
    const reports = await api<Report[]>('leader.summaries', { day });
    if (!reports.length) throw new Error('尚无部门汇总。');
    showModal('部门汇总预览', `<label for="summary-version">汇总版本</label><select id="summary-version">${reports.map((report, index) => `<option value="${index}" ${report.id === target.dataset.reportId ? 'selected' : ''}>V${report.version} / ${esc(dateTime(report.createdAt))}</option>`).join('')}</select><div class="toolbar"><span id="summary-source-state" class="muted">正在核对来源…</span>${button('leader-summary-history-export', '导出此版本 PDF', 'primary', 'id="summary-version-export"', true)}</div><div id="summary-history-preview"></div>`, true);
    const selector = el<HTMLSelectElement>('summary-version');
    const draw = async (): Promise<void> => {
      const report = reports[Number(selector.value)];
      el('summary-history-preview').innerHTML = reportHtml(report, true);
      const state = el('summary-source-state'), exportButton = el<HTMLButtonElement>('summary-version-export');
      state.textContent = '正在核对来源…'; state.className = 'muted'; exportButton.disabled = true; exportButton.dataset.id = report.id;
      try {
        await api<Report>('leader.getSummary', { reportId: report.id });
        if (!selector.isConnected || reports[Number(selector.value)].id !== report.id) return;
        state.textContent = '来源有效，可导出此版本'; exportButton.disabled = false;
      } catch (error) {
        if (!selector.isConnected || reports[Number(selector.value)].id !== report.id) return;
        state.textContent = String(error); state.className = 'danger-text';
      }
    };
    selector.addEventListener('change', () => { void draw(); }); await draw();
  };
  actions['leader-summary-history-export'] = async target => {
    const reportId = target.dataset.id;
    if (!reportId) throw new Error('请先选择汇总版本。');
    const directory = await folder('导出所选汇总版本的 PDF'); if (!directory) return;
    await api('app.exportSummary', { reportId, directory }); await api('app.openPath', { path: directory }); notify('所选汇总版本已导出。');
  };
  actions['leader-summary-export'] = async () => {
    if (!data.summary || data.summaryStale) throw new Error('请先生成来源有效的部门汇总。');
    const reportId = data.summary.id, directory = await folder('导出部门汇总 PDF'); if (!directory) return;
    await api('app.exportSummary', { reportId, directory }); await api('app.openPath', { path: directory }); notify('部门汇总已导出。');
  };
  actions['leader-test'] = async () => { notify('正在测试已保存模型配置…'); const result = await api<{ message: string }>('leader.testConnection'); notify(result.message || '模型连接测试成功。'); };
  actions['leader-open-root'] = async () => { await api('app.openPath', { path: info.root }); };
  actions['leader-backup'] = async () => { const path = await folder('选择备份保存文件夹'); if (!path) return; const result = await api<{ path: string }>('leader.backup', { path }); notify(`备份已完成：${result.path}`); };
  actions['leader-restore'] = async () => {
    const path = await backupFile(); if (!path) return; const target = await folder('选择空文件夹作为恢复目标'); if (!target) return;
    const result = await api<{ path: string }>('leader.restore', { path, target });
    showModal('恢复完成', `<p>数据已恢复到新目录：</p><p class="mono">${esc(result.path)}</p><p>恢复后的部门需重新配置 API Key、接收地址与设备配对。</p>`);
    await api('app.openPath', { path: result.path });
  };
  el('member-rows').addEventListener('change', event => {
    const input = event.target as HTMLInputElement; if (!input.dataset.member) return;
    if (input.checked) selected.add(input.dataset.member); else selected.delete(input.dataset.member);
    input.closest('tr')?.classList.toggle('selected', input.checked); selectionLabel();
  });
  el('select-all').addEventListener('change', () => {
    selected.clear(); if (el<HTMLInputElement>('select-all').checked) data.rows.forEach(row => selected.add(row.memberId)); renderDashboard();
  });
  el('day').addEventListener('change', () => { void perform(async () => {
    const next = el<HTMLInputElement>('day').value; if (!next) return; day = next; data = { rows: [], jobs: [], summaryStale: false }; selected.clear(); jobId = ''; renderDashboard();
    while (refreshing) await new Promise(resolve => setTimeout(resolve, 30)); await refresh();
  }); });
  renderIdentity(); renderSettings(); renderDashboard(); setTab(activeTab); await refresh();
  setInterval(() => { if (!document.hidden && activeTab !== 'settings') void perform(refresh); }, 2500);
}
