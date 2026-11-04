const test = require('node:test');
const assert = require('node:assert/strict');
const M = require('./model.js');
const apply = (s, type, data) => M.reduce(s, { type, data });
function trusted() {
  let s = apply(M.initial(), 'ADD_DEVICE');
  s = apply(s, 'SELECT_DEVICE'); s = apply(s, 'COMPARE', { value: true });
  return apply(s, 'PAIR_NEXT');
}
function connected() { let s = trusted(); s = apply(s, 'CONNECT'); return apply(s, 'CONNECTED', { generation: s.generation }); }

test('discovery does not grant trust; identity change invalidates comparison', () => {
  let s = apply(M.initial(), 'SELECT_DEVICE'); assert.equal(s.peer.trusted, false);
  assert.equal(apply(s, 'PAIR_NEXT').pairing.step, 1);
  s = apply(s, 'COMPARE', { value: true }); s = apply(s, 'CERT_CHANGED');
  assert.equal(s.pairing.compared, false); assert.equal(apply(s, 'PAIR_NEXT').peer.trusted, false);
});
test('trust defaults each grant off and connection never starts input', () => {
  const s = connected(); assert.equal(s.peer.trusted, true); assert.equal(s.peer.status, 'connected');
  assert.deepEqual(Object.values(s.peer.grants), [false, false, false, false]);
  assert.equal(s.input.phase, 'local'); assert.equal(s.clipboard.enabled, false);
});
test('inbound keyboard grant cannot stand in for remote permission', () => {
  let s = connected(); s = apply(s, 'GRANT', { key: 'input', value: true });
  s = apply(s, 'SYSTEM_READY'); s = apply(s, 'INPUT_CONSENT', { value: true });
  s = apply(s, 'START_CONTROL'); assert.equal(s.input.phase, 'local'); assert.equal(s.modal, 'remotePermission');
  s = apply(s, 'REMOTE_ACCEPT'); assert.equal(s.input.phase, 'local');
  s = apply(s, 'START_CONTROL'); assert.equal(s.input.phase, 'remote');
});
test('late connection result cannot resurrect revoked or disconnected session', () => {
  let s = apply(trusted(), 'CONNECT'); const generation = s.generation;
  s = apply(s, 'DISCONNECT'); s = apply(s, 'CONNECTED', { generation }); assert.equal(s.peer.status, 'disconnected');
  s = apply(trusted(), 'CONNECT'); const old = s.generation;
  s = apply(s, 'REVOKE'); s = apply(s, 'CONNECTED', { generation: old }); assert.equal(s.peer.trusted, false);
});
test('emergency and revoked permission wait for recovery acknowledgment', () => {
  let s = M.scenario('control'); s = apply(s, 'EMERGENCY'); assert.equal(s.input.phase, 'restoring');
  s = apply(s, 'RECOVERY_FAIL'); assert.equal(s.input.phase, 'failed');
  s = apply(s, 'RECOVERY_ACK'); assert.equal(s.input.phase, 'local');
  s = apply(M.scenario('control'), 'PERMISSION_REVOKED');
  assert.equal(s.nativeConsent, false); assert.equal(s.systemReady, false); assert.equal(s.input.phase, 'restoring');
});
test('local gesture enable is independent and saved rules are not consent', () => {
  let s = M.initial(); s = apply(s, 'RULE_EDIT'); s = apply(s, 'RULE_TEST'); s = apply(s, 'RULE_SAVE');
  assert.equal(s.rules.length, 3); assert.equal(s.gesture.enabled, false); assert.equal(s.gesture.consent, false);
  s = apply(s, 'GESTURE_ACTIVATE'); assert.equal(s.gesture.enabled, false);
  s = apply(s, 'SYSTEM_READY'); s = apply(s, 'GESTURE_CONSENT', { value: true });
  s = apply(s, 'GESTURE_ACTIVATE'); assert.equal(s.gesture.enabled, true); assert.equal(s.peer.trusted, false);
  s = apply(s, 'EMERGENCY'); assert.equal(s.input.phase, 'restoring'); assert.equal(s.gesture.enabled, false);
});
test('remote control pauses local gestures and closing setup grants nothing', () => {
  let s = connected(); s = apply(s, 'OPEN', { name: 'gestureSetup' }); s = apply(s, 'CLOSE');
  assert.equal(s.gesture.consent, false);
  s.systemReady = true; s.nativeConsent = true; s.peer.remoteInput = true;
  s.gesture.enabled = true; s.gesture.consent = true;
  s = apply(s, 'START_CONTROL'); assert.equal(s.gesture.paused, true);
});
test('rule conflict preserves saved rules and failed save never enables features', () => {
  let s = apply(M.initial(), 'RULE_EDIT'); s = apply(s, 'DRAFT', { key: 'gesture', value: '←' });
  s = apply(s, 'RULE_TEST'); s = apply(s, 'RULE_SAVE');
  assert.equal(s.rules.length, 2); assert.match(s.notice, /已有规则/); assert.equal(s.modal, 'rule');
});
test('unsaved edits require explicit discard; keep editing preserves draft', () => {
  let s = apply(M.initial(), 'RULE_EDIT'); s = apply(s, 'DRAFT', { key: 'scope', value: '浏览器' });
  s = apply(s, 'CLOSE'); assert.equal(s.modal, 'discardRule');
  s = apply(s, 'KEEP_EDITING'); assert.equal(s.draft.scope, '浏览器');
  s = apply(s, 'CLOSE'); s = apply(s, 'DISCARD_RULE'); assert.equal(s.modal, null); assert.equal(s.rules.length, 2);
});
test('dangerous mapping needs confirmation and still grants no execution', () => {
  let s = apply(M.initial(), 'RULE_EDIT'); s = apply(s, 'DRAFT', { key: 'action', value: '关闭窗口（需确认）' });
  s = apply(s, 'RULE_TEST'); s = apply(s, 'RULE_SAVE'); assert.equal(s.rules.length, 2);
  s = apply(s, 'DANGER_CONFIRM', { value: true }); s = apply(s, 'RULE_SAVE');
  assert.equal(s.rules.length, 3); assert.equal(s.gesture.enabled, false); assert.equal(s.nativeConsent, false);
});
test('clipboard direction requires its own grant and outbound remote consent', () => {
  let s = connected(); s.peer.grants.input = true;
  s = apply(s, 'CLIP_ENABLE'); assert.equal(s.clipboard.enabled, false);
  s = apply(s, 'GRANT', { key: 'clipboardOut', value: true });
  s = apply(s, 'CLIP_ENABLE'); assert.equal(s.clipboard.enabled, false);
  s = apply(s, 'REMOTE_DATA_ACCEPT'); s = apply(s, 'CLIP_ENABLE'); assert.equal(s.clipboard.enabled, true);
  s = apply(s, 'CLIP_DIRECTION', { value: 'in' }); assert.equal(s.clipboard.enabled, false);
  s = apply(s, 'CLIP_ENABLE'); assert.equal(s.clipboard.enabled, false);
});
test('file receipt requires per-device grant and one-time acceptance', () => {
  let s = connected(); s = apply(s, 'INCOMING'); assert.equal(s.transfer.phase, 'empty');
  s = apply(s, 'GRANT', { key: 'files', value: true }); s = apply(s, 'INCOMING');
  assert.equal(s.transfer.phase, 'awaiting'); assert.equal(s.transfer.accepted, false);
  s = apply(s, 'ACCEPT_FILE'); assert.equal(s.transfer.phase, 'transferring');
  s = apply(s, 'FILE_PAUSE'); assert.equal(s.transfer.phase, 'paused');
  s = apply(s, 'FILE_RESUME'); s = apply(s, 'FILE_PROGRESS'); assert.equal(s.transfer.phase, 'verifying');
  s = apply(s, 'FILE_COMMIT'); assert.equal(s.transfer.phase, 'done');
});
test('readonly settings do not accept new grants or saves, but allow revocation', () => {
  let s = connected(); s.configWritable = false;
  s = apply(s, 'GRANT', { key: 'input', value: true }); assert.equal(s.peer.grants.input, false);
  s = apply(s, 'RULE_EDIT'); s = apply(s, 'RULE_TEST'); s = apply(s, 'RULE_SAVE'); assert.equal(s.rules.length, 2);
  s = apply(s, 'CONFIG_RESTORE_ACK', { confirmed: true });
  assert.equal(s.peer.trusted, true); assert.equal(s.configWritable, true);
  assert.equal(s.nativeConsent, false); assert.deepEqual(Object.values(s.peer.grants), [false, false, false, false]);
});
test('reducer preserves caller state and all navigation pages remain reachable', () => {
  const s = M.initial(); const original = JSON.stringify(s);
  for (const page of M.pages) assert.equal(apply(s, 'NAV', { page }).page, page);
  apply(s, 'SYSTEM_READY'); assert.equal(JSON.stringify(s), original);
});

test('unconfirmed restoration blocks fresh keyboard and gesture activation', () => {
  let s = apply(M.scenario('control'), 'EMERGENCY');
  s = apply(s, 'START_CONTROL'); assert.equal(s.input.phase, 'restoring');
  s = apply(s, 'RECOVERY_FAIL'); s.gesture.consent = true;
  s = apply(s, 'GESTURE_ACTIVATE'); assert.equal(s.gesture.enabled, false);
  assert.equal(s.input.phase, 'failed');
  s = apply(s, 'RECOVERY_ACK'); s = apply(s, 'START_CONTROL'); assert.equal(s.input.phase, 'remote');
});
test('application chooser keeps draft and excluded applications reject rule save', () => {
  let s = apply(M.initial(), 'RULE_EDIT'); s = apply(s, 'DRAFT', { key: 'scope', value: '指定应用' });
  assert.equal(s.modal, 'appPicker'); s = apply(s, 'APP_CHOOSE', { value: 'Safari（示例）' });
  assert.equal(s.modal, 'rule'); assert.equal(s.draft.scope, 'Safari（示例）');
  s = apply(s, 'DRAFT', { key: 'scope', value: '绘图应用（示例）' });
  s = apply(s, 'RULE_TEST'); s = apply(s, 'RULE_SAVE'); assert.equal(s.rules.length, 2);
});

test('manual address and sketched draft survive later choices without saved-rule changes', () => {
  let s = apply(M.initial(), 'ADD_DEVICE'); s = apply(s, 'ADDRESS', { value: '[2001:db8::24]:24870' });
  s = apply(s, 'SELECT_DEVICE'); s = apply(s, 'PAIR_BACK'); assert.equal(s.pairing.address, '[2001:db8::24]:24870');
  s = apply(s, 'RULE_EDIT'); s = apply(s, 'SKETCH_EDIT'); s = apply(s, 'CLOSE');
  assert.equal(s.modal, 'discardRule'); assert.equal(s.rules.length, 2);
});

test('local gesture continue cannot bypass remote control or unresolved recovery', () => {
  let s = M.scenario('control'); s.gesture = { enabled: true, consent: true, paused: true, reason: '远程控制期间暂停' };
  s = apply(s, 'GESTURE_PAUSE'); assert.equal(s.gesture.paused, true);
  s = apply(s, 'DISCONNECT'); s = apply(s, 'GESTURE_PAUSE'); assert.equal(s.gesture.paused, true);
  s = apply(s, 'RECOVERY_ACK'); s = apply(s, 'GESTURE_PAUSE'); assert.equal(s.gesture.paused, true);
});
test('revoking file permission during verification never reports committed', () => {
  let s = apply(M.scenario('incoming'), 'ACCEPT_FILE'); s = apply(s, 'FILE_PROGRESS');
  s = apply(s, 'GRANT', { key: 'files', value: false }); s = apply(s, 'FILE_COMMIT');
  assert.notEqual(s.transfer.phase, 'done'); assert.equal(s.peer.grants.files, false);
  s = apply(M.scenario('incoming'), 'ACCEPT_FILE'); s = apply(s, 'FILE_PROGRESS'); s = apply(s, 'DISCONNECT'); s = apply(s, 'FILE_COMMIT');
  assert.notEqual(s.transfer.phase, 'done');
});
test('readonly guard also covers identity registration layout and exclusion updates', () => {
  let s = apply(M.initial(), 'SELECT_DEVICE'); s = apply(s, 'COMPARE', { value: true }); s.configWritable = false;
  s = apply(s, 'PAIR_NEXT'); assert.equal(s.peer.trusted, false);
  s = apply(s, 'POSITION', { value: 'left' }); assert.equal(s.position, 'right');
  s = apply(s, 'APP_PICKER', { purpose: 'exclude' }); s = apply(s, 'APP_CHOOSE', { value: 'Safari（示例）' });
  assert.deepEqual(s.exclusions, ['绘图应用（示例）']);
});

test('manual gesture pause survives temporary remote control and restoration', () => {
  let s = M.scenario('connected'); s.systemReady = true; s.nativeConsent = true; s.peer.remoteInput = true;
  s.gesture = { enabled: true, consent: true, paused: false, manualPaused: false, reason: '' };
  s = apply(s, 'GESTURE_PAUSE'); assert.equal(s.gesture.manualPaused, true);
  s = apply(s, 'START_CONTROL'); s = apply(s, 'DISCONNECT'); s = apply(s, 'RECOVERY_ACK');
  assert.equal(s.gesture.paused, true); assert.equal(s.gesture.manualPaused, true);
});
test('inbound control has distinct takeover and grant-revocation restoration', () => {
  let s = M.scenario('receiving'); s = apply(s, 'START_CONTROL'); assert.equal(s.input.phase, 'receiving');
  s = apply(s, 'PHYSICAL_TAKEOVER'); assert.equal(s.input.phase, 'restoring');
  s = apply(s, 'RECOVERY_ACK'); assert.equal(s.input.phase, 'local');
  s = apply(M.scenario('receiving'), 'GRANT', { key: 'input', value: false });
  assert.equal(s.input.phase, 'restoring'); assert.equal(s.peer.grants.input, false);
});
