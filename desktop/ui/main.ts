import './style.css';
import type { AppInfo } from './types';
import { api, el, esc } from './shared';
import { mountMember } from './member';
import { mountLeader } from './leader';

async function start(): Promise<void> {
  const info = await api<AppInfo>('app.info');
  const isLeader = info.role === 'Leader';
  document.title = `Codex 工作日报 · ${isLeader ? '组长端' : '成员端'}`;
  el('app').innerHTML = `<aside class="sidebar"><div class="brand"><span class="brand-mark">日</span><div>工作日报<small>CODEX DAILY</small></div></div><div class="role-card"><span class="role-indicator"></span>${isLeader ? '组长工作台' : '成员工作台'}</div><div class="sidebar-note"><span>工作留下记录<br>进展有迹可循</span><div class="sidebar-rule"></div><small>本机运行 · 部门独立</small></div><footer>版本 ${esc(info.version)}<br>关闭窗口后继续在托盘运行</footer></aside><main id="workspace"></main>`;
  if (isLeader) await mountLeader(info); else await mountMember(info);
}
void start().catch(error => {
  el('app').innerHTML = `<div class="boot boot-error"><h1>工作日报未能启动</h1><p>${esc(error instanceof Error ? error.message : String(error))}</p><button id="restart">重新加载</button></div>`;
  el('restart').addEventListener('click', () => location.reload());
});
