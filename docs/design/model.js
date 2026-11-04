/* Original, in-memory UI prototype model. No production authentication or I/O. */
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.PeerGlideWireframe = api;
})(typeof globalThis === 'object' ? globalThis : this, function () {
  'use strict';
  const pages = ['overview', 'devices', 'mouse', 'collaboration', 'settings'];
  function initial() {
    return {
      version: '0.3.0', page: 'overview', mouseView: 'all', exclusions: ['绘图应用（示例）'], pickerPurpose: 'rule', platform: 'macOS', tab: 'clipboard', modal: null,
      notice: '', configWritable: true, generation: 0, position: 'right', selected: false,
      systemReady: false, nativeConsent: false,
      peer: { name: '工作电脑', os: 'Windows', online: true, trusted: false, status: 'disconnected',
        fingerprint: '演示指纹：18A2 · 6C42 · 9E07 · B531', remoteInput: false, remoteText: false,
        grants: { input: false, clipboardOut: false, clipboardIn: false, files: false } },
      input: { phase: 'local', reason: '' },
      gesture: { enabled: false, consent: false, paused: false, manualPaused: false, reason: '' },
      clipboard: { enabled: false, direction: 'out', mode: 'manual', sent: false },
      transfer: { phase: 'empty', progress: 0, accepted: false },
      rules: [
        { id: 1, gesture: '←', name: '向左', action: '返回', scope: '所有应用', enabled: true },
        { id: 2, gesture: '↓', name: '向下', action: '最小化窗口', scope: '所有应用', enabled: true }
      ],
      draft: { gesture: '→', action: '前进', scope: '所有应用', tested: false, dirty: false, dangerConfirmed: false },
      pairing: { step: 0, compared: false, address: '192.0.2.24:24870' }
    };
  }
  const clone = s => JSON.parse(JSON.stringify(s));
  function stop(s, reason, all) {
    const active = ['remote', 'receiving', 'restoring', 'failed'].includes(s.input.phase) || (all && s.gesture.enabled);
    s.input = { phase: active ? 'restoring' : 'local', reason: active ? reason : '' };
    if (all) { s.gesture.enabled = false; s.gesture.consent = false; s.gesture.paused = false; }
    return s;
  }
  function scenario(name, platform) {
    const s = initial(); s.platform = platform || s.platform;
    if (name === 'single') { s.page = 'mouse'; return s; }
    if (name === 'readonly') { s.page = 'settings'; s.configWritable = false; return s; }
    if (['connected', 'control', 'receiving', 'permission', 'recovery', 'incoming'].includes(name)) {
      s.selected = true; s.peer.trusted = true; s.peer.status = 'connected';
    }
    if (name === 'control' || name === 'permission' || name === 'recovery') {
      s.systemReady = true; s.nativeConsent = true; s.peer.remoteInput = true; s.input.phase = 'remote';
    }
    if (name === 'receiving') { s.systemReady = true; s.nativeConsent = true; s.peer.grants.input = true; s.input.phase = 'receiving'; }
    if (name === 'permission') { s.systemReady = false; s.input = { phase: 'restoring', reason: '系统输入权限已撤销' }; }
    if (name === 'recovery') s.input = { phase: 'failed', reason: '尚未收到输入释放确认' };
    if (name === 'incoming') {
      s.page = 'collaboration'; s.tab = 'files'; s.peer.grants.files = true;
      s.transfer = { phase: 'awaiting', progress: 0, accepted: false };
    }
    return s;
  }
  function reduce(current, event) {
    let s = clone(current); s.notice = '';
    const data = event.data || {};
    const configWrite = event.type === 'POSITION' || (event.type === 'PAIR_NEXT' && s.pairing.step === 1)
      || (event.type === 'APP_CHOOSE' && s.pickerPurpose === 'exclude');
    if (!s.configWritable && configWrite) { s.notice = '设置只读，不能新增或覆盖配置；仍可停止与撤销'; return s; }
    switch (event.type) {
      case 'NAV': if (pages.includes(data.page)) s.page = data.page; s.modal = null; break;
      case 'PLATFORM': { const next = data.value === 'Windows' ? 'Windows' : 'macOS'; if (next !== s.platform) { stop(s, '切换平台视图后重新检查权限', true); s.systemReady = false; s.nativeConsent = false; if (next === 'macOS' && s.draft.action === '最大化窗口') { s.draft.action = '最小化窗口'; s.draft.tested = false; } } s.platform = next; break; }
      case 'SCENARIO': return scenario(data.name, s.platform);
      case 'TAB': s.tab = data.value === 'files' ? 'files' : 'clipboard'; break;
      case 'OPEN': s.modal = data.name; break;
      case 'CLOSE': if (s.modal === 'appPicker') { s.modal = s.pickerPurpose === 'exclude' ? null : 'rule'; break; } s.modal = s.modal === 'rule' && s.draft.dirty ? 'discardRule' : null; break;
      case 'KEEP_EDITING': s.modal = 'rule'; break;
      case 'DISCARD_RULE': s.draft.dirty = false; s.modal = null; break;
      case 'ADD_DEVICE': s.page = 'devices'; s.modal = 'pair'; s.pairing.step = 0; s.pairing.compared = false; break;
      case 'SELECT_DEVICE': s.selected = true; s.pairing.step = 1; s.notice = '已选择示例设备，尚未建立信任'; break;
      case 'PAIR_BACK': s.pairing.step = Math.max(0, s.pairing.step - 1); break;
      case 'COMPARE': s.pairing.compared = !!data.value; break;
      case 'CERT_CHANGED': s.pairing.compared = false; s.notice = '身份材料已变化，请重新核验'; break;
      case 'PAIR_NEXT':
        if (s.pairing.step === 0) { s.notice = '请先选择设备或填写地址'; break; }
        if (s.pairing.step === 1 && !s.pairing.compared) { s.notice = '请先在另一台电脑上核对身份'; break; }
        if (s.pairing.step === 1) { s.peer.trusted = true; s.peer.grants = { input: false, clipboardOut: false, clipboardIn: false, files: false }; }
        s.pairing.step = Math.min(3, s.pairing.step + 1); break;
      case 'GRANT':
        if (!s.peer.trusted || !Object.hasOwn(s.peer.grants, data.key)) { s.notice = '先核验并登记设备'; break; }
        if (!s.configWritable && data.value) { s.notice = '设置只读，不能新增授权；仍可撤销或停止'; break; }
        s.peer.grants[data.key] = !!data.value;
        if (!data.value && data.key === 'input' && s.input.phase === 'receiving') stop(s, '键鼠许可已撤回，等待释放确认', false);
        if (!data.value && (data.key === 'clipboardOut' || data.key === 'clipboardIn')) s.clipboard.enabled = false;
        if (!data.value && data.key === 'files' && ['transferring','verifying'].includes(s.transfer.phase)) s.transfer.phase = 'paused';
        break;
      case 'CONNECT':
        if (!s.peer.trusted) { s.notice = '设备尚未核验，不能连接'; s.modal = 'pair'; break; }
        s.peer.status = 'connecting'; s.generation += 1; break;
      case 'CONNECTED':
        if (data.generation !== s.generation || s.peer.status !== 'connecting' || !s.peer.trusted) break;
        s.peer.status = 'connected'; s.modal = null; s.input.phase = 'local';
        s.notice = '通信连接已建立；键鼠、剪贴板和文件权限分别管理'; break;
      case 'DISCONNECT':
        s.generation += 1; s.peer.status = 'disconnected'; s.clipboard.enabled = false;
        if (['transferring','verifying'].includes(s.transfer.phase)) s.transfer.phase = 'paused';
        stop(s, '正在停止协同并恢复本机控制', false); break;
      case 'REVOKE':
        s.generation += 1; s.peer.trusted = false; s.peer.remoteInput = false; s.peer.remoteText = false; s.peer.status = 'disconnected';
        s.peer.grants = { input: false, clipboardOut: false, clipboardIn: false, files: false };
        s.clipboard.enabled = false; s.transfer.phase = s.transfer.phase === 'empty' ? 'empty' : 'cancelled';
        s.modal = null; stop(s, '信任已撤销，正在恢复', false); break;
      case 'POSITION': if (['left', 'right', 'top', 'bottom'].includes(data.value)) s.position = data.value; break;
      case 'START_CONTROL':
        if (s.input.phase === 'receiving') { s.notice = '先恢复本机控制，再切换到另一台电脑'; break; }
        if (['restoring', 'failed'].includes(s.input.phase)) { s.notice = '本机恢复尚未确认，不能重新开始控制'; break; }
        if (s.peer.status !== 'connected') { s.notice = '请先连接设备'; break; }
        if (!s.configWritable || !s.systemReady || !s.nativeConsent) { s.modal = 'inputSetup'; break; }
        if (!s.peer.remoteInput) { s.modal = 'remotePermission'; break; }
        s.input = { phase: 'remote', reason: '' };
        if (s.gesture.enabled) { s.gesture.paused = true; s.gesture.reason = '远程控制期间暂停'; }
        s.notice = '演示：正在控制工作电脑；Ctrl+Alt+Esc 随时恢复本机'; break;
      case 'SYSTEM_READY': s.systemReady = true; break;
      case 'INPUT_CONSENT': s.nativeConsent = !!data.value; break;
      case 'REMOTE_DATA_ACCEPT': s.peer.remoteText = true; s.notice = '演示：对端已允许接收本机文字'; break;
      case 'REMOTE_ACCEPT': s.peer.remoteInput = true; s.modal = null; s.notice = '演示对端已允许本机控制；尚未开始控制'; break;
      case 'EMERGENCY': stop(s, '已停止新输入，等待释放确认', true); s.modal = null; break;
      case 'RECOVERY_ACK': if (s.input.phase === 'restoring' || s.input.phase === 'failed') {
        s.input = { phase: 'local', reason: '' }; s.gesture.paused = !!s.gesture.manualPaused; s.gesture.reason = s.gesture.paused ? '已手动暂停' : ''; s.notice = '演示回执：本机控制已恢复';
      } break;
      case 'RECOVERY_FAIL': s.input = { phase: 'failed', reason: '输入释放尚未确认，请按恢复指引处理' }; break;
      case 'PHYSICAL_TAKEOVER': if (s.input.phase === 'receiving') stop(s, '演示：本机物理输入已接管，等待释放确认', false); break;
      case 'PERMISSION_REVOKED':
        s.systemReady = false; s.nativeConsent = false; stop(s, '系统权限被撤销，正在停止输入', true); break;
      case 'GESTURE_ENABLE': if (s.gesture.enabled) { s.gesture.enabled = false; s.gesture.consent = false; } else s.modal = 'gestureSetup'; break;
      case 'GESTURE_CONSENT': s.gesture.consent = !!data.value; break;
      case 'GESTURE_ACTIVATE':
        if (['restoring', 'failed'].includes(s.input.phase)) { s.notice = '先确认本机输入恢复，再启用手势'; break; }
        if (!s.configWritable || !s.systemReady || !s.gesture.consent) { s.notice = '请先完成本机权限演示并明确同意'; break; }
        s.gesture.enabled = true; s.gesture.manualPaused = false; s.gesture.paused = ['remote','receiving'].includes(s.input.phase);
        s.gesture.reason = s.gesture.paused ? '远程控制期间暂停' : ''; s.modal = null;
        s.notice = '仅为原型状态演示，真实软件的全局手势仍未启用'; break;
      case 'GESTURE_PAUSE': if (s.gesture.paused && (s.input.phase !== 'local' || !s.systemReady || !s.gesture.consent)) { s.notice = '当前控制或权限状态不允许继续本机手势'; break; } if (s.gesture.enabled) { s.gesture.manualPaused = !s.gesture.manualPaused; s.gesture.paused = s.gesture.manualPaused; s.gesture.reason = s.gesture.paused ? '已手动暂停' : ''; } break;
      case 'MOUSE_VIEW': s.mouseView = ['all', 'apps', 'excluded'].includes(data.value) ? data.value : 'all'; break;
      case 'APP_PICKER': s.pickerPurpose = data.purpose === 'exclude' ? 'exclude' : 'rule'; s.modal = 'appPicker'; break;
      case 'APP_CHOOSE':
        if (!['Safari（示例）', 'Microsoft Edge（示例）', '访达（示例）', '资源管理器（示例）', '绘图应用（示例）'].includes(data.value)) break;
        if (s.pickerPurpose === 'exclude') { if (!s.exclusions.includes(data.value)) s.exclusions.push(data.value); s.mouseView = 'excluded'; s.modal = null; }
        else { s.draft.scope = data.value; s.draft.dirty = true; s.draft.tested = false; s.modal = 'rule'; }
        break;
      case 'RULE_EDIT': s.modal = 'rule'; s.draft = { gesture: '→', action: '前进', scope: '所有应用', tested: false, dirty: false, dangerConfirmed: false }; break;
      case 'DRAFT':
        if (['gesture', 'action', 'scope'].includes(data.key)) { s.draft[data.key] = String(data.value).slice(0, 80); s.draft.tested = false; s.draft.dirty = true; s.draft.dangerConfirmed = false; if (data.key === 'scope' && data.value === '指定应用') { s.pickerPurpose = 'rule'; s.modal = 'appPicker'; } }
        break;
      case 'ADDRESS': s.pairing.address = String(data.value).slice(0, 120); break;
      case 'SKETCH_EDIT': s.draft.dirty = true; s.draft.tested = false; break;
      case 'DANGER_CONFIRM': s.draft.dangerConfirmed = !!data.value; break;
      case 'RULE_TEST': s.draft.tested = true; s.notice = '原型反馈示意：不进行真实识别或系统执行'; break;
      case 'RULE_SAVE':
        if (s.draft.scope === '指定应用' || s.exclusions.includes(s.draft.scope)) { s.notice = '请选择具体应用，且不能与排除列表冲突'; break; }
        if (!s.configWritable) { s.notice = '设置当前只读，原文件不会被覆盖'; break; }
        if (s.draft.action.startsWith('关闭') && !s.draft.dangerConfirmed) { s.notice = '请先确认危险动作，规则仍不会因此获得全局执行授权'; break; }
        if (!s.draft.tested) { s.notice = '先在本窗口测试这个规则'; break; }
        if (s.rules.some(r => r.enabled && r.gesture === s.draft.gesture && r.scope === s.draft.scope)) { s.notice = '同一手势与范围已有规则，请修改原规则或更换范围'; break; }
        s.rules.push({ id: Math.max(0, ...s.rules.map(r => r.id)) + 1, ...s.draft, name: s.draft.gesture, enabled: true });
        s.draft.dirty = false; s.modal = null; s.notice = '规则已保存到原型内存；不会自动启用全局手势'; break;
      case 'CLIP_DIRECTION': s.clipboard.direction = data.value === 'in' ? 'in' : 'out'; s.clipboard.enabled = false; break;
      case 'CLIP_MODE': s.clipboard.mode = data.value === 'auto' ? 'auto' : 'manual'; s.clipboard.enabled = false; break;
      case 'CLIP_ENABLE': {
        const grant = s.clipboard.direction === 'out' ? 'clipboardOut' : 'clipboardIn';
        if (s.peer.status !== 'connected' || !s.peer.grants[grant] || (grant === 'clipboardOut' && !s.peer.remoteText)) { s.modal = 'dataPermission'; break; }
        s.clipboard.enabled = !s.clipboard.enabled; break;
      }
      case 'SEND_TEXT':
        if (s.peer.status !== 'connected' || !s.peer.grants.clipboardOut || !s.peer.remoteText) { s.modal = 'dataPermission'; break; }
        s.clipboard.sent = true; s.notice = '演示：已发送示例文字，未读取系统剪贴板'; break;
      case 'INCOMING':
        if (s.peer.status !== 'connected' || !s.peer.grants.files) { s.modal = 'dataPermission'; break; }
        s.transfer = { phase: 'awaiting', progress: 0, accepted: false }; s.tab = 'files'; break;
      case 'ACCEPT_FILE':
        if (s.transfer.phase !== 'awaiting' || !s.peer.grants.files || s.peer.status !== 'connected') break;
        s.transfer.accepted = true; s.transfer.phase = 'transferring'; s.transfer.progress = 28; break;
      case 'REJECT_FILE': if (s.transfer.phase === 'awaiting') s.transfer.phase = 'cancelled'; break;
      case 'FILE_PAUSE': if (['transferring','verifying'].includes(s.transfer.phase)) s.transfer.phase = 'paused'; break;
      case 'FILE_RESUME': if (s.transfer.phase === 'paused' && s.transfer.accepted && s.peer.grants.files && s.peer.status === 'connected') s.transfer.phase = 'transferring'; break;
      case 'FILE_PROGRESS': if (s.transfer.phase === 'transferring') { s.transfer.progress = 100; s.transfer.phase = 'verifying'; } break;
      case 'FILE_COMMIT': if (s.transfer.phase === 'verifying' && s.transfer.accepted && s.peer.trusted && s.peer.grants.files && s.peer.status === 'connected') s.transfer.phase = 'done'; break;
      case 'FILE_CANCEL': if (!['empty', 'done'].includes(s.transfer.phase)) s.transfer.phase = 'cancelled'; break;
      case 'CONFIG_RESTORE': s.modal = 'restoreConfig'; break;
      case 'CONFIG_RESTORE_ACK':
        if (!data.confirmed) break;
        s.configWritable = true; s.gesture.enabled = false; s.gesture.consent = false; s.nativeConsent = false;
        s.peer.grants = { input: false, clipboardOut: false, clipboardIn: false, files: false };
        s.clipboard.enabled = false; stop(s, '恢复设置后正在停止旧输入', false); s.modal = null;
        s.notice = '演示：先备份再恢复设置，身份保留，授权关闭'; break;
    }
    return s;
  }
  return { initial, reduce, scenario, pages };
});
