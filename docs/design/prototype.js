/* Low-fidelity, local-only prototype. All effects are in-memory examples. */
(function () {
  'use strict';
  const M = window.PeerGlideWireframe;
  let state = M.initial();
  let connectionTimer;
  let sketchPoints = [];
  const app = document.getElementById('app');
  const esc = v => String(v).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const btn = (label, action, data = '', kind = '') => `<button class="${kind}" data-action="${action}" ${data}>${label}</button>`;
  const badge = (text, planned) => `<span class="badge${planned ? ' dashed' : ''}">${text}</span>`;
  const navs = [['overview', '⌂', '总览'], ['devices', '▱', '设备与屏幕'], ['mouse', '⌁', '本机鼠标'], ['collaboration', '⇄', '剪贴板与文件'], ['settings', '⚙', '设置']];
  const connected = () => state.peer.status === 'connected';
  const controlLabel = () => ({ local: '当前由本机控制', remote: '正在控制工作电脑', receiving: '工作电脑正在控制本机', restoring: '正在恢复本机控制', failed: '恢复尚未完成' }[state.input.phase]);
  const peerStatus = () => ({ disconnected: '未连接', connecting: '正在连接', connected: '通信已连接' }[state.peer.status]);
  const gestureLabel = () => !state.gesture.enabled ? '未启用' : state.gesture.paused ? '已暂停' : '演示：已开启';
  function heading(title, text, actions = '') { return `<div class="page-heading"><div><div class="eyebrow">PEERGLIDE / 屏跃</div><h1>${title}</h1><p>${text}</p></div><div class="actions">${actions}</div></div>`; }
  function grant(key, title, note) {
    return `<label class="permission-row"><input type="checkbox" data-field="grant" data-key="${key}" ${state.peer.grants[key] ? 'checked' : ''} ${!state.peer.trusted ? 'disabled' : ''}><span><strong>${title}</strong><small>${note}</small></span></label>`;
  }
  function recovery() {
    if (!['restoring', 'failed'].includes(state.input.phase)) return '';
    return `<div class="strip attention section"><div><strong>${controlLabel()}</strong><p>${esc(state.input.reason)}。停止请求不代表恢复已经完成。</p><div class="recovery-tools">${btn('演示：收到恢复确认', 'RECOVERY_ACK')}${btn('演示：仍未确认', 'RECOVERY_FAIL')}${btn('查看恢复指引', 'OPEN', 'data-name="recovery"')}</div></div>${badge('需要处理')}</div>`;
  }
  function overview() {
    return heading('让鼠标连贯，也让协作清楚。', '一台电脑先用本机增强，多台电脑再打开协同。', btn('添加电脑', 'ADD_DEVICE', '', 'primary')) + `
    <div class="grid2">
      <section class="card"><div class="icon">⌁</div><h2>本机鼠标 ${badge('目标交互', true)}</h2><p>按住、划动、松开。为常用操作设置手势，不需要连接另一台电脑。</p><div class="statusline"><span class="dot"></span>鼠标手势：${gestureLabel()}</div><div class="actions">${btn('配置本机鼠标', 'NAV', 'data-page="mouse"', 'primary')}${btn('先在窗口里试画', 'OPEN', 'data-name="practice"')}</div><div class="meta">录制与测试只在本窗口，不会执行系统动作。</div></section>
      <section class="card"><div class="icon">▱</div><h2>多机协同</h2><p>把电脑放到与桌面一致的位置，分别选择键鼠、文字和文件权限。</p><div class="statusline"><span class="dot"></span>${connected() ? '1 台设备已连接，' + controlLabel() : '尚未连接另一台电脑'}</div><div class="actions">${btn(connected() ? '管理设备与屏幕' : '连接另一台电脑', connected() ? 'NAV' : 'ADD_DEVICE', connected() ? 'data-page="devices"' : '', 'primary')}</div><div class="meta">建立连接不会自动允许控制或同步内容。</div></section>
    </div>
    <section class="section"><div class="section-heading"><h2>下一步</h2><span class="hint">随当前状态变化，不堆功能开关</span></div><div class="card"><ol class="checklist">
      <li><span class="num">${state.peer.trusted ? '✓' : '1'}</span><span class="text">${state.peer.trusted ? '工作电脑身份已核验' : '添加并核验另一台电脑'}<small>发现列表只提供线索；在两端核对身份后再登记。</small></span>${btn(state.peer.trusted ? '查看设备' : '开始添加', state.peer.trusted ? 'NAV' : 'ADD_DEVICE', state.peer.trusted ? 'data-page="devices"' : '')}</li>
      <li><span class="num">2</span><span class="text">选择你需要的协作方式<small>键鼠、文字和文件分别授权；可以只连接，不开启共享。</small></span>${btn('查看权限', 'OPEN', 'data-name="permissions"')}</li>
      <li><span class="num">3</span><span class="text">摆好屏幕，试一次跨屏<small>先做通信测试，再明确开启键鼠。紧急恢复始终可用。</small></span>${btn('查看布局', 'NAV', 'data-page="devices"')}</li>
    </ol></div></section>${recovery()}
    <section class="section"><div class="strip"><div><strong>这台${state.platform === 'macOS' ? ' Mac' : ' Windows 电脑'}</strong><p>${state.systemReady ? '演示权限已就绪' : '尚未检查本机系统权限'} · ${state.configWritable ? '设置可编辑' : '设置只读'}</p></div>${btn('权限与设置', 'NAV', 'data-page="settings"')}</div></section>`;
  }
  function devices() {
    const directions = { left: '左侧', right: '右侧', top: '上方', bottom: '下方' };
    const drops = ['left', 'right', 'top', 'bottom'].filter(d => d !== state.position).map(d => `<div class="drop-target ${d}" data-drop="${d}">放到${directions[d]}</div>`).join('');
    return heading('设备与屏幕', '连接状态、屏幕位置和这台设备的权限放在一起。', btn('添加电脑', 'ADD_DEVICE', '', 'primary')) + `
    <div class="layout-area"><div class="screen-grid">${drops}<div class="screen local">这台电脑<small>${state.platform} · 本机</small></div><div class="screen peer ${state.position}" draggable="true" aria-label="拖动示例工作电脑">工作电脑<small>${state.peer.os} · ${peerStatus()}</small></div></div><p class="hint">拖动工作电脑，或用方向按钮摆放。精细多显示器布局属于目标设计。</p></div>
    <div class="row wrap"><span class="hint">工作电脑位于本机</span>${Object.entries(directions).map(([d, label]) => btn(label, 'POSITION', `data-value="${d}"`, state.position === d ? 'primary' : '')).join('')}<span class="hint">改变布局不会授予控制权限</span></div>
    <div class="grid2 section"><section class="card"><div class="device-head"><span class="device-name">▱ 工作电脑</span>${badge(state.peer.trusted ? '已核验' : '尚未信任')}</div><div class="definition"><span>网络状态</span><strong>${peerStatus()}</strong><span>本机允许它控制键鼠</span><strong>${state.peer.grants.input ? '允许' : '未允许'}</strong><span>它允许本机控制键鼠</span><strong>${state.peer.remoteInput ? '对端已允许' : '等待对端允许'}</strong><span>当前输入归属</span><strong>${controlLabel()}</strong></div><div class="actions">${state.peer.trusted ? btn(connected() ? '断开通信连接' : '连接设备', connected() ? 'DISCONNECT' : 'CONNECT', '', connected() ? '' : 'primary') : btn('核验并登记', 'ADD_DEVICE', '', 'primary')}${btn('设备权限', 'OPEN', 'data-name="permissions"')}</div><div class="meta">${btn('高级连接与诊断', 'OPEN', 'data-name="connection"', 'quiet')}${state.peer.trusted ? btn('撤销设备信任', 'OPEN', 'data-name="revoke"', 'quiet') : ''}</div></section>
    <section class="card"><h2>开始一次跨屏</h2><p>通信连接只是准备。开启键鼠前，还要确认本机权限与对端许可。</p><ol class="checklist"><li><span class="num">${connected() ? '✓' : '1'}</span><span class="text">通信连接${connected() ? '已建立' : '尚未建立'}</span></li><li><span class="num">${state.systemReady && state.nativeConsent ? '✓' : '2'}</span><span class="text">本机输入设置${state.systemReady && state.nativeConsent ? '已准备' : '待准备'}</span></li><li><span class="num">${state.peer.remoteInput ? '✓' : '3'}</span><span class="text">对端允许本机控制${state.peer.remoteInput ? '' : '（本机不能代授）'}</span></li></ol><div class="actions">${btn('开始跨屏体验', 'START_CONTROL', '', 'primary')}</div></section></div>${recovery()}`;
  }
  function mouse() {
    return heading('本机鼠标', '把一个手势做完整：画轨迹、选动作、试一下，再保存。', btn('添加手势', 'RULE_EDIT', '', 'primary')) + `
    <div class="strip"><div class="toggle-label"><button class="toggle ${state.gesture.enabled ? 'on' : ''}" data-action="GESTURE_ENABLE" aria-label="${state.gesture.enabled ? '关闭' : '准备启用'}本机鼠标手势"></button><div><strong>鼠标手势 ${badge(gestureLabel())}</strong><p>${state.gesture.enabled ? (state.gesture.reason || '目标交互演示中；真实软件全局功能尚未启用') : '默认关闭。无需添加设备，保存规则也不会自动开启。'}</p></div></div><div class="actions">${btn('本窗口试画', 'OPEN', 'data-name="practice"')}${state.gesture.enabled ? btn(state.gesture.paused ? '继续' : '暂停', 'GESTURE_PAUSE') : btn('查看启用准备', 'GESTURE_ENABLE')}</div></div>
    <div class="scope-bar">${[['all','全部规则'],['apps','应用规则'],['excluded','排除列表']].map(([value,label])=>btn(label,'MOUSE_VIEW',`data-value="${value}"`,state.mouseView===value?'primary':'')).join('')}<span class="hint">精确应用优先，排除始终优先</span></div>
    <div class="card"><div class="section-heading"><h2>我的手势</h2><span class="hint">轨迹和动作直接对应，不必记数字编号</span></div>${state.mouseView === 'excluded' ? `<div class="selection"><strong>全屏与远程控制期间暂停</strong><small>以下应用排除优先于所有手势规则。</small></div>${state.exclusions.map(app => `<div class="strip section"><span>${esc(app)}</span>${badge('已排除')}</div>`).join('')}<div class="actions section">${btn('选择示例应用加入排除','APP_PICKER','data-purpose="exclude"')}</div>` : `<table class="table"><thead><tr><th>手势</th><th>动作</th><th>作用范围</th><th>状态</th><th></th></tr></thead><tbody>${state.rules.filter(r=>state.mouseView!=='apps'||r.scope!=='所有应用').map(r => `<tr><td class="arrow">${esc(r.gesture)}</td><td><strong>${esc(r.action)}</strong></td><td>${esc(r.scope)}</td><td>${badge(r.enabled ? '已保存' : '已关闭')}</td><td>${btn('试一下', 'OPEN', 'data-name="practice"', 'quiet')}</td></tr>`).join('')}</tbody></table>${state.mouseView==='apps'?`<div class="actions section">${btn('添加应用规则','RULE_EDIT')}</div>`:''}`}<p class="hint section">测试只显示识别与生效规则，不执行关闭窗口等动作。真实平台暂不支持的动作在动作选择中说明。</p></div>
    <div class="grid2 section"><div class="card dashed"><h3>遇到影响操作的应用？</h3><p>游戏、绘图或需要右键拖动的应用可加入排除列表。全屏与远程控制期间暂停。</p><div class="actions">${btn('查看排除与优先级', 'OPEN', 'data-name="exclusions"')}</div></div><div class="card dashed"><h3>始终能停下来</h3><p>Ctrl+Alt+Esc 停止新输入并恢复本机控制。恢复未确认时保持提示。</p><div class="actions">${btn('查看恢复说明', 'OPEN', 'data-name="recovery"')}</div></div></div>`;
  }
  function collaboration() {
    const headingText = '按设备和方向选择内容。连接成功不会默认共享剪贴板或文件。';
    const tabs = `<div class="tabs">${btn('文字剪贴板', 'TAB', 'data-value="clipboard"', state.tab === 'clipboard' ? 'selected' : '')}${btn('文件收发', 'TAB', 'data-value="files"', state.tab === 'files' ? 'selected' : '')}</div>`;
    if (state.tab === 'files') return heading('剪贴板与文件', headingText, badge('目标交互', true)) + tabs + fileView();
    return heading('剪贴板与文件', headingText, badge('目标交互', true)) + tabs + `
    <div class="strip"><div><strong>工作电脑 · ${peerStatus()}</strong><p>${state.peer.grants.clipboardOut || state.peer.grants.clipboardIn ? '文字权限按方向管理' : '尚未授权文字同步'} · 不包含图片、文件或任意格式</p></div>${btn('查看本设备权限', 'OPEN', 'data-name="dataPermission"')}</div>
    <div class="grid2 section"><section class="card"><h2>文字同步</h2><div class="field section"><label>同步方向</label><select aria-label="同步方向" data-field="clipDirection"><option value="out" ${state.clipboard.direction === 'out' ? 'selected' : ''}>这台电脑 → 工作电脑</option><option value="in" ${state.clipboard.direction === 'in' ? 'selected' : ''}>工作电脑 → 这台电脑</option></select></div><div class="field"><label>工作方式</label><select aria-label="工作方式" data-field="clipMode"><option value="manual" ${state.clipboard.mode === 'manual' ? 'selected' : ''}>仅手动发送（默认）</option><option value="auto" ${state.clipboard.mode === 'auto' ? 'selected' : ''}>自动同步文字</option></select></div><div class="actions">${state.clipboard.mode === 'auto' ? btn(state.clipboard.enabled ? '暂停自动同步' : '开启自动同步', 'CLIP_ENABLE', '', 'primary') : `<span class="badge">仅在点击发送时发送</span>${btn('检查方向授权','OPEN','data-name="dataPermission"')}`}</div><p class="hint section">双向需要分别授权。暂停不会清空系统剪贴板，也不能保证自动识别全部敏感文字。</p></section>
    <section class="card"><h2>手动发送示例</h2><p>确认目标和这次要发送的文字，再发送一次。此原型不读取系统剪贴板。</p><div class="selection"><strong>示例文字</strong><small>把明天的议程发到工作电脑。</small></div><div class="actions">${btn('发送这段示例文字', 'SEND_TEXT', '', 'primary')}</div>${state.clipboard.sent ? '<div class="preview-result">演示：已发送示例，未读取或写入系统剪贴板。</div>' : ''}</section></div>`;
  }
  function fileView() {
    const t = state.transfer;
    const status = { awaiting: '等待本次接收确认', transferring: '正在接收', paused: '已暂停', verifying: '校验并保存中', done: '已校验并保存', cancelled: '已取消' }[t.phase];
    const toolbar = `<div class="section-heading"><h2>收件箱</h2><div class="actions">${btn('接收示例', 'INCOMING')}${btn('选择示例文件发送', 'OPEN', 'data-name="sendFile"', 'primary')}</div></div>`;
    if (t.phase === 'empty') return toolbar + `<div class="empty"><div class="icon">⇩</div><h3>没有待接收的文件</h3><p>对方发送后，会先询问你是否接受。不会自动把文件写入下载文件夹。</p><div class="actions">${btn('查看设备文件权限', 'OPEN', 'data-name="dataPermission"')}</div></div><p class="hint section">原生跨屏拖进 Finder／资源管理器仍是独立规划，不用普通发送区冒充。</p>`;
    let controls = '';
    if (t.phase === 'awaiting') controls = btn('拒绝', 'REJECT_FILE') + btn('接收这一次', 'ACCEPT_FILE', '', 'primary');
    if (t.phase === 'transferring') controls = btn('暂停', 'FILE_PAUSE') + btn('取消', 'FILE_CANCEL') + btn('演示：接收完成', 'FILE_PROGRESS', '', 'quiet');
    if (t.phase === 'paused') controls = btn('继续这次接收', 'FILE_RESUME', '', 'primary') + btn('取消', 'FILE_CANCEL');
    if (t.phase === 'verifying') controls = btn('演示：校验与保存成功', 'FILE_COMMIT', '', 'primary');
    return toolbar + `<div class="file-card"><div class="file-top"><span class="file-icon">▤</span><div class="file-name"><strong>项目资料.zip</strong><small>来自工作电脑 · 24 MB · 示例文件</small></div>${badge(status)}</div><div class="file-details"><span>保存到</span><strong>下载 / 屏跃接收（演示位置）</strong><span>同名文件</span><strong>不覆盖，接收前选择处理方式</strong><span>授权范围</span><strong>仅本次，不授予对方浏览本机文件的权限</strong></div>${['transferring', 'paused', 'verifying', 'done'].includes(t.phase) ? `<progress value="${t.progress}" max="100" aria-label="示例文件接收进度"></progress><p class="hint">${t.progress}% · ${status}</p>` : ''}<div class="actions">${controls}</div></div><p class="hint section">100% 接收不等于完成；只有校验和最终保存成功后，才显示完成。</p>`;
  }
  function settings() {
    return heading('设置', '系统权限、配置恢复和诊断；协作权限留在对应设备中。') + (!state.configWritable ? `<div class="strip attention"><div><strong>设置当前只读，原文件已保留</strong><p>配置损坏、版本过新或被另一实例占用时，不静默覆盖。</p></div>${btn('选择恢复方式', 'CONFIG_RESTORE')}</div>` : '') + `
    <div class="settings-list section"><section class="setting"><div><strong>系统输入权限</strong><p>${state.platform === 'macOS' ? '按能力需要辅助功能与输入监控；由你在系统设置中处理。' : '尊重Windows权限与安全桌面，不提高权限绕过限制。'}</p><div class="statusline">${state.systemReady ? '演示状态：已就绪' : '尚未检查 / 需要处理'}</div></div>${btn('查看权限指引', 'OPEN', 'data-name="system"')}</section>
    <section class="setting"><div><strong>外观与语言</strong><p>简体中文 · 跟随系统外观。线框阶段只验证结构与层级。</p></div><span class="badge">简体中文</span></section>
    <section class="setting"><div><strong>配置与备份</strong><p>恢复前先备份；保留本机身份，关闭旧授权。</p></div>${btn('备份与恢复', 'CONFIG_RESTORE')}</section>
    <section class="setting"><div><strong>后台运行</strong><p>托盘／菜单栏属于目标设计。实际版本未接通时不暗示关窗后仍在后台。</p></div>${badge('规划', true)}</section>
    <section class="setting"><div><strong>连接诊断</strong><p>通信探针不注入输入；高级详情保留准确原始错误，不记录按键内容。</p></div>${btn('查看诊断', 'OPEN', 'data-name="connection"')}</section>
    <section class="setting"><div><strong>实现状态与原型说明</strong><p>当前是可点击线框，不是已上线软件。不会操作设备、权限、剪贴板或磁盘。</p></div>${btn('查看实现对照', 'OPEN', 'data-name="implementation"')}</section></div>${recovery()}`;
  }
  function dialogFrame(title, subtitle, body, footer, wide) {
    return `<div class="overlay" data-overlay="true"><section class="dialog ${wide ? 'wide' : ''}" role="dialog" aria-modal="true" aria-label="${esc(title)}"><div class="dialog-head"><div><h2>${title}</h2><p>${subtitle}</p></div><div class="actions">${btn('紧急恢复本机控制','EMERGENCY','','danger')}${btn('×', 'CLOSE', 'aria-label="关闭弹窗"', 'quiet')}</div></div><div class="dialog-content">${body}${state.notice ? `<div class="mini-state" role="status">${esc(state.notice)}</div>` : ''}</div><div class="dialog-footer">${footer}</div></section></div>`;
  }
  function pairDialog() {
    const p = state.pairing;
    const steps = ['选择设备', '核验身份', '选择用途', '连接与布局'].map((label, i) => `<div class="step ${i === p.step ? 'active' : i < p.step ? 'done' : ''}">${i + 1}. ${label}</div>`).join('');
    let body = '', footer = btn('取消', 'CLOSE');
    if (p.step === 0) {
      body = `<p class="muted">发现的名字和地址不能证明身份，继续核验后才会登记。</p><button class="selection" data-action="SELECT_DEVICE"><strong>▱ 工作电脑</strong><small>Windows · 示例局域网地址 · 尚未核验 →</small></button><details><summary>找不到设备？手动输入地址</summary><div class="field section"><label for="address">对端地址与端口</label><input id="address" data-field="address" type="text" value="${esc(p.address)}" placeholder="例如 192.0.2.24:24870"></div>${btn('使用此示例地址', 'SELECT_DEVICE')}</details>`;
    } else if (p.step === 1) {
      body = `<h3>请在另一台电脑上核对身份</h3><p class="modal-note">名字相同不代表同一设备。只有两端独立显示的身份一致，才登记为可信设备。</p><div class="fingerprint">18A2 · 6C42 · 9E07 · B531</div><p class="hint">这是演示指纹，不是生产凭据。实际当前版本使用证书核验；配对码／扫码属于目标交互。</p><div class="spacer"></div><label class="check"><input type="checkbox" data-field="compare" ${p.compared ? 'checked' : ''}><span>我已在示例对端核对，显示一致</span></label><div class="actions section">${btn('演示：身份材料变化', 'CERT_CHANGED', '', 'quiet')}</div>`;
      footer = btn('上一步', 'PAIR_BACK') + btn('核验并登记', 'PAIR_NEXT', !p.compared ? 'disabled' : '', 'primary');
    } else if (p.step === 2) {
      body = `<h3>你允许工作电脑做什么？</h3><p class="modal-note">可以全部保持关闭，先建立通信连接。以下只代表本机授权，不能代替对端的同意。</p>${grant('input', '允许工作电脑控制本机键鼠', '不会因此允许它读取剪贴板或发送文件。')}${grant('clipboardOut', '允许发送本机文字到工作电脑', '仅文字，发送方式和方向仍需单独开启。')}${grant('files', '允许它发起文件接收请求', '每次保存文件前仍需本机确认。')}`;
      footer = btn('上一步', 'PAIR_BACK') + btn('继续', 'PAIR_NEXT', '', 'primary');
    } else {
      body = `<div class="selection"><strong>✓ 工作电脑身份已登记</strong><small>下一步建立通信连接，再摆好屏幕。此时尚未开启键鼠控制。</small></div><p class="modal-note">网络角色与控制方向无关。地址、端口和等待连接方式放在高级选项中。</p>`;
      footer = btn('稍后连接', 'CLOSE') + btn(state.peer.status === 'connecting' ? '正在连接…' : '连接并进入布局', 'CONNECT', state.peer.status === 'connecting' ? 'disabled' : '', 'primary');
    }
    return dialogFrame('添加电脑', '设备发现 → 两端核验 → 独立权限 → 连接', `<div class="steps">${steps}</div>${body}`, footer);
  }
  function modal() {
    if (!state.modal) return '';
    if (state.modal === 'pair') return pairDialog();
    if (state.modal === 'permissions' || state.modal === 'dataPermission') return dialogFrame('工作电脑的协作权限', '权限属于这台设备和对应方向，默认关闭。', `${!state.peer.trusted ? '<p class="modal-note">先添加并核验设备，才能设置它的协作权限。</p>' : ''}<div class="small-title">本机这端的授权（按方向）</div>${grant('input', '控制本机键鼠', '不代表对端也允许本机控制它。')}${grant('clipboardOut', '允许本机发出文字', '这台电脑 → 工作电脑')}${grant('clipboardIn', '向本机发送文字', '工作电脑 → 这台电脑')}${grant('files', '发起文件接收请求', '保存前仍须逐次确认，不授予文件浏览权限。')}<div class="small-title">对端允许本机（只读）</div><div class="strip"><span>${state.peer.remoteInput ? '对端已允许本机控制键鼠' : '对端尚未允许本机控制键鼠'}</span>${btn('查看对端确认流程', 'OPEN', 'data-name="remotePermission"')}</div><div class="strip section"><span>${state.peer.remoteText ? '对端已允许接收本机文字' : '对端尚未允许接收本机文字'}</span>${btn('演示：对端允许文字', 'REMOTE_DATA_ACCEPT')}</div>`, btn('完成', 'CLOSE'));
    if (state.modal === 'remotePermission') return dialogFrame('需要在工作电脑上确认', '本机不能替另一台电脑授予控制权限。', '<div class="selection"><strong>工作电脑上的请求示意</strong><small>“这台Mac请求控制本机键鼠。剪贴板与文件不会随之开放。”</small></div><p class="modal-note">原型通过下面按钮演示对端同意，不发送真实请求。</p>', btn('返回', 'CLOSE') + btn('演示：在对端同意', 'REMOTE_ACCEPT', '', 'primary'));
    if (state.modal === 'inputSetup' || state.modal === 'gestureSetup') {
      const gesture = state.modal === 'gestureSetup';
      return dialogFrame(gesture ? '准备使用本机鼠标手势' : '准备开启跨屏键鼠', '目标流程演示；不会申请系统权限或启用真实输入。', `<ol class="checklist"><li><span class="num">1</span><span class="text">检查本机系统输入权限<small>${state.systemReady ? '演示状态已就绪' : '在系统设置中按需处理，完成后返回检查。'}</small></span>${btn('演示权限就绪', 'SYSTEM_READY')}</li><li><span class="num">2</span><span class="text">明确选择这次开启的能力<small>与设备信任、剪贴板和文件权限无关。</small></span></li></ol><label class="check section"><input type="checkbox" data-field="${gesture ? 'gestureConsent' : 'inputConsent'}" ${(gesture ? state.gesture.consent : state.nativeConsent) ? 'checked' : ''}><span>${gesture ? '同意在本机拦截已启用手势，随时可暂停。' : '同意开启本次原生键鼠协同，Ctrl+Alt+Esc 可恢复本机。'}</span></label><p class="modal-note">实际软件的全局 G1 与部分能力仍在开发／验收中，不能因离线画布或此原型状态变为可用。</p>`, btn('取消', 'CLOSE') + (gesture ? btn('继续目标流程演示', 'GESTURE_ACTIVATE', '', 'primary') : btn('检查下一步', 'START_CONTROL', '', 'primary')));
    }
    if (state.modal === 'practice' || state.modal === 'rule') {
      const editor = state.modal === 'rule';
      const choices = ['返回', '前进', '复制', '粘贴', '最小化窗口', ...(state.platform === 'Windows' ? ['最大化窗口'] : []), '关闭窗口（需确认）'];
      return dialogFrame(editor ? '添加一条手势规则' : '本窗口试画', '画轨迹 → 选择范围与动作 → 预览反馈样式 → 保存', `<div class="form-grid"><div><h3>1. 在这里画轨迹</h3><p class="hint">按下并拖动即可，仅在此画布内记录。</p><canvas id="gesture-canvas" class="canvas" width="640" height="380" aria-label="窗口内手势画布"></canvas><div class="canvas-label">本窗口内测试 · 不做全局捕获 · 不执行动作</div>${btn('清除轨迹', 'CLEAR_CANVAS', '', 'quiet')}</div><div><div class="field"><label>2. 手势方向</label><select aria-label="手势方向" data-field="draft" data-key="gesture">${['←', '→', '↑', '↓', '↓→', '↓←', '↑→', '↑←'].map(v => `<option ${state.draft.gesture === v ? 'selected' : ''}>${v}</option>`).join('')}</select></div><div class="field"><label>适用范围</label><select aria-label="适用范围" data-field="draft" data-key="scope">${['所有应用', '浏览器', '指定应用', ...(!['所有应用','浏览器','指定应用'].includes(state.draft.scope)?[state.draft.scope]:[])].map(v => `<option ${state.draft.scope === v ? 'selected' : ''}>${v}</option>`).join('')}</select></div><div class="field"><label>3. 执行动作</label><select aria-label="执行动作" data-field="draft" data-key="action">${choices.map(v => `<option ${state.draft.action === v ? 'selected' : ''}>${v}</option>`).join('')}</select></div><p class="hint">动作列表随平台变化。画布只演示交互，真实匹配由Rust引擎完成。</p>${state.draft.action.startsWith('关闭') ? `<label class="check section"><input type="checkbox" data-field="dangerConfirm" ${state.draft.dangerConfirmed ? 'checked' : ''}><span>我确认这条规则可能关闭起点窗口；保存不授予全局执行权限。</span></label>` : ''}${state.draft.tested ? `<div class="preview-result">示例轨迹：${esc(state.draft.gesture)}<br>对应动作：${esc(state.draft.action)}<br>范围：${esc(state.draft.scope)}<br>此处演示反馈样式，不作真实识别或执行验收</div>` : '<div class="preview-result">先测试，查看匹配的轨迹、作用范围和冲突。</div>'}</div></div>`, btn('取消', 'CLOSE') + btn('演示匹配反馈，不执行', 'RULE_TEST') + (editor ? btn('保存规则，不启用', 'RULE_SAVE', '', 'primary') : ''), true);
    }
    if (state.modal === 'appPicker') return dialogFrame('选择示例应用', '不枚举本机程序；这里只演示应用选择方式。', `<div class="field"><label>应用名称</label><input type="text" placeholder="搜索应用（线框占位）" disabled></div>${(state.platform === 'macOS' ? ['Safari（示例）','访达（示例）','绘图应用（示例）'] : ['Microsoft Edge（示例）','资源管理器（示例）','绘图应用（示例）']).map(label=>btn(label,'APP_CHOOSE',`data-value="${esc(label)}"`,'selection')).join('')}<p class="hint">实际产品保存精确应用标识，普通用户不必输入路径或数字ID。</p>`, btn(state.pickerPurpose==='exclude'?'取消':'返回规则',state.pickerPurpose==='exclude'?'CLOSE':'KEEP_EDITING'));
    if (state.modal === 'discardRule') return dialogFrame('放弃未保存的规则？', '已保存的规则保持不变。', '<p>当前选择尚未保存。继续编辑会保留草稿，放弃只影响本次修改。</p>', btn('继续编辑', 'KEEP_EDITING') + btn('放弃本次修改', 'DISCARD_RULE'));
    if (state.modal === 'exclusions') return dialogFrame('规则生效顺序与排除', '先决定适用范围，再决定动作。', '<ol class="checklist"><li><span class="num">1</span><span class="text">排除应用<small>游戏、绘图、需要右键长按／拖动的应用优先排除。</small></span></li><li><span class="num">2</span><span class="text">精确应用规则<small>同一轨迹可在不同应用做不同的事。</small></span></li><li><span class="num">3</span><span class="text">应用组 → 全局规则<small>优先级与命中原因可预览，冲突不按文件顺序猜测。</small></span></li></ol>', btn('明白了', 'CLOSE', '', 'primary'));
    if (state.modal === 'system') return dialogFrame('本机系统权限', '只展示指引，原型不打开或修改系统权限。', `<div class="selection"><strong>${state.platform === 'macOS' ? 'macOS：辅助功能／输入监控' : 'Windows：输入桌面与权限边界'}</strong><small>${state.platform === 'macOS' ? '由你在系统设置中按需要授予；工作进程也须检查，GUI快照不能代替。' : '不绕过UAC、安全桌面或防火墙，不自动提升权限。'}</small></div><p class="modal-note">没有对应授权时仍可以配置、窗口内试画和查看帮助，不把开关显示成已生效。</p>`, btn('返回', 'CLOSE') + btn('演示：权限就绪', 'SYSTEM_READY', '', 'primary'));
    if (state.modal === 'recovery') return dialogFrame('停止与恢复', '先停止新输入，再确认所属输入已经释放。', `${['restoring','failed'].includes(state.input.phase)?`<div class="strip attention"><div><strong>${controlLabel()}</strong><p>${esc(state.input.reason)}</p><div class="recovery-tools">${btn('演示：收到恢复确认','RECOVERY_ACK')}${btn('演示：仍未确认','RECOVERY_FAIL')}</div></div></div>`:''}<ol class="checklist"><li><span class="num">1</span><span class="text">按 Ctrl+Alt+Esc 或点击紧急恢复<small>不等待二次确认，不再提交新动作。</small></span></li><li><span class="num">2</span><span class="text">观察恢复状态<small>仍在恢复或未确认时，不显示“正常”。</small></span></li><li><span class="num">3</span><span class="text">按对应错误指引处理<small>保留必要诊断，不修改系统安全设置，不记录按键内容。</small></span></li></ol>`, btn('返回', 'CLOSE') + btn('演示紧急恢复', 'EMERGENCY', '', 'primary'));
    if (state.modal === 'revoke') return dialogFrame('撤销工作电脑的信任？', '停止会话，并清除此设备的协作授权。', '<p>本机身份保留。以后重新连接这台设备时，需要重新核验；不会把其旧权限继承给另一台同名电脑。</p>', btn('保留信任', 'CLOSE') + btn('撤销并停止协作', 'REVOKE', '', 'danger'));
    if (state.modal === 'restoreConfig') return dialogFrame('备份并恢复设置', '原文件和本机身份不会被静默覆盖。', '<p>目标流程先保留原文件／生成备份，再恢复有效设置，并关闭旧授权。此原型只演示状态，不接触磁盘。</p><p class="modal-note">未来版本配置或写入冲突时，优先选择兼容版本或有效备份，不强行覆盖。</p>', btn('取消', 'CLOSE') + btn('演示：备份后恢复', 'CONFIG_RESTORE_ACK', '', 'primary'));
    if (state.modal === 'connection') return dialogFrame('高级连接与诊断', '首选用户任务；网络细节只在需要时展开。', '<div class="field"><label>连接方式</label><select><option>主动连接工作电脑</option><option>在本机等待对端连接</option></select></div><div class="field"><label>示例地址与端口</label><input value="192.0.2.24:24870" type="text"></div><div class="selection"><strong>通信探针：只测连接</strong><small>不捕获、不注入输入，不授予控制或数据权限。</small></div><details><summary>原始诊断详情</summary><p class="hint section">原型没有启动工作进程；真实软件显示有界诊断，不输出按键或剪贴板内容。</p></details>', btn('返回', 'CLOSE'));
    if (state.modal === 'sendFile') return dialogFrame('发送文件', '目标流程示意，不打开真实文件选择器。', '<div class="field"><label>发送到</label><select><option>工作电脑</option></select></div><div class="selection"><strong>项目资料.zip · 24 MB</strong><small>这是内置示例，不读取本机文件。发送后仍等待对端确认。</small></div><p class="modal-note">不把普通发送区描述为原生跨屏拖拽。</p>', btn('返回', 'CLOSE') + btn('查看接收确认示例', 'SCENARIO', 'data-name="incoming"', 'primary'));
    if (state.modal === 'implementation') return dialogFrame('线框与真实实现的边界', '这里是交互评审原型，不是运行中的屏跃。', '<div class="selection"><strong>当前基础</strong><small>中文GUI、可信证书登记、真实通信worker、诊断、相对布局、离线手势画布与规则编辑。</small></div><div class="selection"><strong>仍在开发／验收</strong><small>全局本机手势、配对码、多设备与精细拓扑、生产剪贴板／文件协作、托盘与覆盖层。</small></div><p class="modal-note">原型全部内存状态，不调用系统权限、原生捕获、剪贴板、文件、网络或脚本。所有演示数据刷新即清除。</p>', btn('明白了', 'CLOSE', '', 'primary'));
    return '';
  }
  function render() {
    const oldFocus = document.activeElement;
    const focusedField = oldFocus?.dataset?.field; const focusedKey = oldFocus?.dataset?.key;
    const page = { overview, devices, mouse, collaboration, settings }[state.page] || overview;
    app.innerHTML = `<div class="prototype-bar"><span>中文交互线框 · v${state.version} · 目标流程演示，非运行软件</span><div class="tools"><span>平台视图</span><select data-field="platform" aria-label="切换平台视图"><option ${state.platform === 'macOS' ? 'selected' : ''}>macOS</option><option ${state.platform === 'Windows' ? 'selected' : ''}>Windows</option></select><select data-field="scenario" aria-label="切换演示场景"><option value="">演示场景…</option><option value="initial">首次打开</option><option value="single">只有一台电脑</option><option value="connected">已连接，尚未授权</option><option value="control">正在跨屏控制</option><option value="receiving">对端正在控制本机</option><option value="permission">权限被撤销</option><option value="recovery">恢复未完成</option><option value="readonly">配置只读</option><option value="incoming">待接收文件</option></select><a href="README.zh-CN.md">设计说明</a></div></div>
    <div class="window" ${state.modal ? 'inert' : ''}><div class="titlebar"><span class="window-dots"><i></i><i></i><i></i></span>屏跃 · 屏幕相连，协作无间</div><div class="workspace"><aside class="sidebar"><div class="brand"><strong>屏跃</strong><small>PEERGLIDE</small></div><nav class="nav" aria-label="主导航">${navs.map(([id, icon, label]) => btn(`<span class="glyph">${icon}</span>${label}`, 'NAV', `data-page="${id}"`, state.page === id ? 'active' : '')).join('')}</nav><div class="sidebar-foot">一台电脑也能开始<br>无云账号 · 授权分开<br><br>线框 / 示例数据<br>未连接真实设备</div></aside><main class="main"><header class="globalbar"><div class="summary"><span><i class="dot"></i>${controlLabel()}</span><span>工作电脑：${peerStatus()}</span></div><div class="actions">${state.gesture.enabled ? btn(state.gesture.paused ? '继续本机手势' : '暂停本机手势', 'GESTURE_PAUSE') : ''}${state.input.phase==='receiving'?btn('演示：本机输入接管','PHYSICAL_TAKEOVER'):''}${connected() ? btn('停止协同', 'DISCONNECT') : ''}${['restoring','failed'].includes(state.input.phase) ? btn('恢复状态','OPEN','data-name="recovery"') : ''}${btn('紧急恢复本机控制', 'EMERGENCY', '', 'danger')}</div></header><div class="content">${page()}</div>${state.notice && !state.modal ? `<div class="toast" role="status">${esc(state.notice)}</div>` : ''}</main></div><footer class="footbar"><span>本机手势：${gestureLabel()}　·　输入：${controlLabel()}　·　文字：${state.clipboard.mode === 'auto' && state.clipboard.enabled ? '自动同步（演示）' : '仅手动发送'}</span><span>Ctrl+Alt+Esc 紧急恢复　·　全部状态仅为原型演示</span></footer></div>${modal()}`;
    installCanvas();
    const dialog = app.querySelector('[role=dialog]');
    if (dialog) {
      const same = focusedField ? [...dialog.querySelectorAll('[data-field]')].find(e => e.dataset.field === focusedField && e.dataset.key === focusedKey) : null;
      (same || dialog.querySelector('[data-action="CLOSE"]') || dialog.querySelector('button,input,select'))?.focus();
    }
  }
  function dispatch(type, data) {
    if (['RULE_EDIT','SCENARIO'].includes(type)) sketchPoints = [];
    state = M.reduce(state, { type, data }); render();
    if (type === 'CONNECT' && state.peer.status === 'connecting') {
      clearTimeout(connectionTimer); const generation = state.generation;
      connectionTimer = setTimeout(() => dispatch('CONNECTED', { generation }), 550);
    }
  }
  app.addEventListener('click', e => {
    const button = e.target.closest('[data-action]'); if (!button || button.disabled) return;
    const type = button.dataset.action;
    if (type === 'CLEAR_CANVAS') { const c = document.getElementById('gesture-canvas'); sketchPoints = []; if (c) c.getContext('2d').clearRect(0, 0, c.width, c.height); return; }
    const d = button.dataset;
    dispatch(type, { page: d.page, name: d.name, value: d.value, purpose: d.purpose, confirmed: type === 'CONFIG_RESTORE_ACK' });
  });
  app.addEventListener('input', e => { if (e.target.dataset.field === 'address') state = M.reduce(state,{type:'ADDRESS',data:{value:e.target.value}}); });
  app.addEventListener('change', e => {
    const t = e.target; const f = t.dataset.field;
    if (f === 'platform') dispatch('PLATFORM', { value: t.value });
    if (f === 'scenario') { clearTimeout(connectionTimer); dispatch('SCENARIO', { name: t.value }); }
    if (f === 'grant') dispatch('GRANT', { key: t.dataset.key, value: t.checked });
    if (f === 'dangerConfirm') dispatch('DANGER_CONFIRM', { value: t.checked });
    if (f === 'compare') dispatch('COMPARE', { value: t.checked });
    if (f === 'gestureConsent') dispatch('GESTURE_CONSENT', { value: t.checked });
    if (f === 'inputConsent') dispatch('INPUT_CONSENT', { value: t.checked });
    if (f === 'clipDirection') dispatch('CLIP_DIRECTION', { value: t.value });
    if (f === 'clipMode') dispatch('CLIP_MODE', { value: t.value });
    if (f === 'draft') dispatch('DRAFT', { key: t.dataset.key, value: t.value });
  });
  app.addEventListener('dragstart', e => { if (e.target.closest('.screen.peer')) e.dataTransfer.setData('text/plain', 'prototype-peer'); });
  app.addEventListener('dragover', e => { const t = e.target.closest('[data-drop]'); if (t) { e.preventDefault(); t.classList.add('dragover'); } });
  app.addEventListener('dragleave', e => { const t = e.target.closest('[data-drop]'); if (t) t.classList.remove('dragover'); });
  app.addEventListener('drop', e => { const t = e.target.closest('[data-drop]'); if (t && e.dataTransfer.getData('text/plain') === 'prototype-peer') { e.preventDefault(); dispatch('POSITION', { value: t.dataset.drop }); } });
  document.addEventListener('keydown', e => {
    if (e.key === 'Tab' && state.modal) {
      const dialog = app.querySelector('[role=dialog]');
      const items = [...dialog.querySelectorAll('button:not(:disabled),input:not(:disabled),select:not(:disabled),textarea:not(:disabled),a[href]')];
      if (items.length && e.shiftKey && document.activeElement === items[0]) { e.preventDefault(); items.at(-1).focus(); }
      else if (items.length && !e.shiftKey && document.activeElement === items.at(-1)) { e.preventDefault(); items[0].focus(); }
    }
    if (e.key === 'Escape' && e.ctrlKey && e.altKey) { e.preventDefault(); dispatch('EMERGENCY'); }
    else if (e.key === 'Escape' && state.modal) dispatch('CLOSE');
  });
  function installCanvas() {
    const canvas = document.getElementById('gesture-canvas'); if (!canvas) return;
    const ctx = canvas.getContext('2d'); let drawing = false;
    const paint = () => { ctx.clearRect(0,0,canvas.width,canvas.height); ctx.lineWidth=4;ctx.lineCap='round';ctx.strokeStyle='#343d4b';ctx.beginPath();for(let i=0;i<sketchPoints.length;i++){const p=sketchPoints[i];if(i===0)ctx.moveTo(...p);else ctx.lineTo(...p);}ctx.stroke(); };
    paint();
    const point = e => { const r = canvas.getBoundingClientRect(); return [(e.clientX - r.left) * canvas.width / r.width, (e.clientY - r.top) * canvas.height / r.height]; };
    canvas.addEventListener('pointerdown', e => { e.preventDefault(); drawing = true; canvas.setPointerCapture(e.pointerId); sketchPoints=[point(e)]; state=M.reduce(state,{type:'SKETCH_EDIT'}); paint(); });
    canvas.addEventListener('pointermove', e => { if (drawing && sketchPoints.length<256) { sketchPoints.push(point(e)); paint(); } });
    const end = () => { drawing = false; };
    canvas.addEventListener('pointerup', end); canvas.addEventListener('pointercancel', end);
    canvas.addEventListener('contextmenu', e => e.preventDefault());
  }
  render();
})();
