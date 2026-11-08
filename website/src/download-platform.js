// A local convenience hint, never a compatibility or CPU-architecture claim.
export function detectPlatform(nav) {
  const ua = nav.userAgent || '';
  const hint = nav.userAgentData?.platform || '';
  const legacy = nav.platform || '';
  if (nav.userAgentData?.mobile || /Android|iPhone|iPad|iPod|CrOS/i.test(`${hint} ${ua}`) ||
      (/Mac/i.test(`${hint} ${legacy}`) && nav.maxTouchPoints > 1)) return null;
  const source = hint || legacy || ua;
  if (/Windows|Win32|Win64/i.test(source)) return 'windows';
  if (/macOS|MacIntel|MacPPC|Macintosh|Mac OS/i.test(source)) return 'macos';
  if (/Linux/i.test(source)) return 'linux';
  return null;
}

export function setupPlatformDownloads(doc, win, nav) {
  const platform = detectPlatform(nav);
  const status = doc.querySelector('[data-system-status]');
  const messages = {
    macos: '检测到 macOS，已标出对应下载。当前仅提供 Apple 芯片包，请确认你的机型。',
    windows: '检测到 Windows，已标出对应下载。当前提供 x64 包，请确认系统架构。',
    linux: '检测到 Linux：当前仅有诊断构建，尚无原生键鼠支持。',
  };
  if (status && platform) status.textContent = messages[platform];
  const target = platform && doc.querySelector(`[data-platform="${platform}"]`);
  if (!target) return;
  target.classList.add('is-system-match');
  const badge = target.querySelector('[data-platform-match]');
  if (badge) badge.hidden = false;

  const scroll = (initial = false) => {
    const reduced = win.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
    target.scrollIntoView({ behavior: initial || reduced ? 'instant' : 'smooth', block: 'start' });
  };
  // Wait for initial images to settle; never jump away from another section.
  const initialScroll = () => { if (win.location.hash === '#preview') scroll(true); };
  if (doc.readyState === 'complete') initialScroll();
  else win.addEventListener('load', initialScroll, { once: true });
  win.addEventListener('hashchange', () => {
    if (win.location.hash === '#preview') scroll();
  });
  doc.addEventListener('click', (event) => {
    const link = event.target.closest?.('a[href="#preview"]');
    if (!link || event.defaultPrevented || event.button !== 0 ||
        event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    if (win.location.hash !== '#preview') win.history.pushState(null, '', '#preview');
    scroll();
    if (event.detail === 0) target.focus({ preventScroll: true });
  });
}
