import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import { marked } from 'marked';
import DOMPurify from 'dompurify';
import type { Report } from './types';

export const api = <T = unknown>(op: string, args: Record<string, unknown> = {}): Promise<T> => invoke<T>('dispatch', { op, args });
export const esc = (s: unknown): string => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
export const el = <T extends HTMLElement = HTMLElement>(id: string): T => document.getElementById(id) as T;
export const dateTime = (value?: string, zone?: string): string => {
  if (!value) return '—';
  try { return new Intl.DateTimeFormat('zh-CN', { dateStyle: 'short', timeStyle: 'medium', timeZone: zone || 'Asia/Shanghai' }).format(new Date(value)); } catch { return value; }
};
export const button = (action: string, label: string, cls = '', data = '', disabled = false): string => `<button type="button" data-action="${action}" class="${cls}" ${data} ${disabled ? 'disabled' : ''}>${esc(label)}</button>`;
export const badge = (text: string, good: boolean): string => `<span class="badge ${good ? 'good' : 'pending'}"><i></i>${esc(text || '待处理')}</span>`;
export const empty = (title: string, detail = ''): string => `<div class="empty"><div class="empty-icon">▤</div><h3>${esc(title)}</h3><p>${esc(detail)}</p></div>`;
export const actions: Record<string, (target: HTMLElement) => Promise<void> | void> = {};
let messageTimer: ReturnType<typeof setTimeout>;
export function notify(text: unknown, error = false): void {
  const node = el('message'); node.textContent = typeof text === 'string' ? text : String(text); node.className = error ? 'error' : ''; node.hidden = false;
  clearTimeout(messageTimer); if (!error) messageTimer = setTimeout(() => { node.hidden = true; }, 6500);
}
export async function perform(fn: () => Promise<void>, target?: HTMLElement): Promise<void> {
  if (target?.dataset.busy) return;
  if (target) { target.dataset.busy = 'true'; (target as HTMLButtonElement).disabled = true; }
  try { await fn(); } catch (error) { notify(error instanceof Error ? error.message : String(error), true); }
  finally { if (target) { delete target.dataset.busy; (target as HTMLButtonElement).disabled = target.dataset.stateDisabled === 'true'; } }
}
document.addEventListener('click', event => {
  const target = event.target as HTMLElement;
  if (target.closest('.markdown a')) { event.preventDefault(); return; }
  const node = target.closest<HTMLElement>('[data-action]');
  if (node && actions[node.dataset.action!]) void perform(async () => { await actions[node.dataset.action!](node); }, node);
});
el('message').addEventListener('click', () => { el('message').hidden = true; });
export const folder = async (title: string): Promise<string | null> => {
  const result = await open({ directory: true, multiple: false, title }); return typeof result === 'string' ? result : null;
};
export const savePdf = (name: string): Promise<string | null> => save({ title: '保存正式 PDF', defaultPath: name, filters: [{ name: 'PDF 文档', extensions: ['pdf'] }] });
export const backupFile = async (): Promise<string | null> => {
  const result = await open({ title: '选择部门备份', multiple: false, filters: [{ name: '部门备份', extensions: ['zip'] }] }); return typeof result === 'string' ? result : null;
};
export function showModal(title: string, content: string, wide = false): HTMLDialogElement {
  const dialog = el<HTMLDialogElement>('modal'); if (dialog.open) dialog.close();
  dialog.className = wide ? 'wide' : '';
  dialog.innerHTML = `<div class="modal-heading"><h2 id="modal-title">${esc(title)}</h2>${button('modal-close', '关闭', 'quiet')}</div><div class="modal-content">${content}</div>`;
  dialog.showModal(); return dialog;
}
actions['modal-close'] = () => el<HTMLDialogElement>('modal').close();
export function ask(title: string, label: string, initial = '', multiline = false): Promise<string | null> {
  const field = multiline ? `<textarea id="modal-input" rows="6">${esc(initial)}</textarea>` : `<input id="modal-input" value="${esc(initial)}" />`;
  const dialog = showModal(title, `<form id="modal-form"><label for="modal-input">${esc(label)}</label>${field}<div class="modal-actions"><button type="submit" class="primary">确定</button></div></form>`);
  const input = el<HTMLInputElement | HTMLTextAreaElement>('modal-input'); input.focus();
  return new Promise(resolve => {
    let value: string | null = null;
    dialog.addEventListener('close', () => resolve(value), { once: true });
    el('modal-form').addEventListener('submit', e => { e.preventDefault(); value = input.value; dialog.close(); });
  });
}
export function consent(title: string, content: string, label = '确认继续'): Promise<boolean> {
  const dialog = showModal(title, `${content}<form id="consent-form" class="modal-actions"><button type="submit" class="primary">${esc(label)}</button></form>`);
  return new Promise(resolve => {
    let accepted = false; dialog.addEventListener('close', () => resolve(accepted), { once: true });
    el('consent-form').addEventListener('submit', e => { e.preventDefault(); accepted = true; dialog.close(); });
  });
}
export function reportHtml(report: Report, summary = false): string {
  const headings: Record<string, string> = summary ? { overview: '总体进展', members: '成员工作摘要', blockers: '共同阻塞', nextSteps: '后续安排' } : { tasks: '当日任务', outputs: '实际执行与产出', verification: '验证情况', blockers: '未完成事项与阻塞', nextSteps: '后续计划' };
  const fallback = Object.entries(headings).map(([key, heading]) => `## ${heading}\n\n${(report.content?.[key] || []).map(x => `${x.status ? `【${x.status}】` : ''}${x.personalStatement ? '【成员陈述，未经独立核验】' : ''}${x.text}\n\n来源：${(x.evidenceIds || x.reportIds || []).join('、')}`).join('\n\n') || '记录中无可确认内容'}`).join('\n\n');
  const body = DOMPurify.sanitize(marked.parse(report.body ?? fallback, { async: false, breaks: true }) as string, { USE_PROFILES: { html: true }, FORBID_TAGS: ['img', 'video', 'audio', 'iframe', 'form', 'input', 'button', 'style'], FORBID_ATTR: ['style', 'srcset', 'contenteditable'] });
  return `<div class="report-meta"><span>${esc(report.department)}${report.memberName ? ` / ${esc(report.memberName)}` : ''}</span><span>${esc(report.day)} · V${report.version}${report.snapshotVersion ? ` · 来源 V${report.snapshotVersion}` : ''}</span><span>${summary ? `生成于 ${esc(dateTime(report.createdAt))}` : `采集截至 ${esc(dateTime(report.capturedAt, report.timeZone))} · ${report.sessionCount ?? 0} 个会话`}</span></div><article class="markdown" aria-label="只读报告正文">${body}</article>${!summary ? `<section class="report-feedback"><h3>个人补充说明</h3><p>${report.feedback ? esc(report.feedback.noSupplement ? '无补充说明' : report.feedback.text) : '<span class="muted">等待成员复核与补充</span>'}</p>${report.feedback ? `<small>${esc(report.feedback.author)} · ${esc(dateTime(report.feedback.submittedAt, report.timeZone))}</small>` : ''}</section>` : ''}${report.missingMembers?.length ? `<div class="notice">未纳入汇总：${esc(report.missingMembers.join('、'))}</div>` : ''}${report.warnings?.length ? `<details class="report-sources"><summary>采集范围说明 · ${report.warnings.length}</summary><p>${report.warnings.map(esc).join('<br>')}</p></details>` : ''}<details class="report-sources"><summary>来源索引 · ${Object.keys(report.sources || {}).length}</summary>${Object.entries(report.sources || {}).map(([key, value]) => `<p><strong>${esc(key)}</strong> ${esc(value)}</p>`).join('') || '<p>无来源索引</p>'}</details>`;
}
