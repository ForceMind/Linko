import { test } from 'node:test';
import assert from 'node:assert/strict';
import { detectPlatform, setupPlatformDownloads } from './src/download-platform.js';

test('desktop hints and Safari/Firefox fallback identify OS without assuming architecture', () => {
  for (const [nav, expected] of [
    [{ userAgentData: { platform: 'Windows', mobile: false } }, 'windows'],
    [{ platform: 'Win32', userAgent: 'Windows NT 10.0' }, 'windows'],
    [{ userAgentData: { platform: 'macOS' } }, 'macos'],
    [{ platform: 'MacIntel', maxTouchPoints: 0 }, 'macos'],
    [{ userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15)' }, 'macos'],
    [{ platform: 'Linux x86_64' }, 'linux'],
    [{ userAgentData: { platform: 'Windows' }, platform: 'MacIntel' }, 'windows'],
  ]) assert.equal(detectPlatform(nav), expected);
});
test('mobile, iPad desktop mode, ChromeOS and unknown never recommend a desktop package', () => {
  for (const nav of [{}, { userAgent: 'unknown' }, { userAgent: 'CrOS x86_64' },
    { userAgent: 'Linux; Android 16', platform: 'Linux arm' },
    { userAgent: 'iPhone Mac OS X', platform: 'iPhone' },
    { platform: 'MacIntel', maxTouchPoints: 5 },
    { userAgentData: { platform: 'Android', mobile: true } },
  ]) assert.equal(detectPlatform(nav), null);
});
function fixture(hash = '', reduced = false, ready = 'complete') {
  const events = {}, classes = new Set(), scrolls = [], focus = [];
  const badge = { hidden: true }, status = { textContent: '' };
  const target = { classList: { add: (x) => classes.add(x) },
    querySelector: () => badge, scrollIntoView: (x) => scrolls.push(x), focus: (x) => focus.push(x) };
  const doc = { readyState: ready, querySelector: (selector) => selector === '[data-system-status]' ? status : target,
    addEventListener: (key, fn) => { events[key] = fn; } };
  const win = { location: { hash }, matchMedia: () => ({ matches: reduced }),
    addEventListener: (key, fn) => { events[key] = fn; },
    history: { pushState: (_, __, next) => { win.location.hash = next; } } };
  return { doc, win, events, classes, scrolls, focus, badge, status };
}
test('highlights matching card on homepage without forcing a scroll', () => {
  const f = fixture(); setupPlatformDownloads(f.doc, f.win, { platform: 'MacIntel' });
  assert(f.classes.has('is-system-match')); assert.equal(f.badge.hidden, false);
  assert.match(f.status.textContent, /请确认你的机型/); assert.equal(f.scrolls.length, 0);
});
test('direct preview URL waits for load and ignores navigation away before load', () => {
  const f = fixture('#preview', false, 'loading');
  setupPlatformDownloads(f.doc, f.win, { platform: 'Win32' });
  assert.equal(f.scrolls.length, 0); f.events.load();
  assert.equal(f.scrolls[0].behavior, 'instant');
  f.win.location.hash = '#product'; f.events.load(); assert.equal(f.scrolls.length, 1);
});
test('repeated download clicks and hash navigation scroll; reduced motion and keyboard focus respected', () => {
  const f = fixture('#preview', true); setupPlatformDownloads(f.doc, f.win, { platform: 'Win32' });
  const event = { target: { closest: () => ({}) }, button: 0, detail: 0, preventDefault() { this.prevented = true; } };
  f.events.click(event); assert.equal(event.prevented, true);
  assert.equal(f.scrolls.at(-1).behavior, 'instant'); assert.equal(f.focus.length, 1);
  f.win.location.hash = '#product'; f.events.hashchange(); assert.equal(f.scrolls.length, 2);
  f.win.location.hash = '#preview'; f.events.hashchange(); assert.equal(f.scrolls.length, 3);
  f.events.click({ ...event, ctrlKey: true }); assert.equal(f.scrolls.length, 3);
});
test('unknown platform preserves native navigation; Linux labels diagnostic-only', () => {
  const f = fixture('#preview'); setupPlatformDownloads(f.doc, f.win, {});
  assert.equal(f.classes.size, 0); assert.equal(f.scrolls.length, 0);
  const linux = fixture(); setupPlatformDownloads(linux.doc, linux.win, { platform: 'Linux x86_64' });
  assert.match(linux.status.textContent, /尚无原生键鼠/);
});
