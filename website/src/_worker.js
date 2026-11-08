// Build replaces this token using the same checked-in release.json as the UI.
const release = __RELEASE_METADATA__;
const security = {
  'Content-Security-Policy': "default-src 'none'; frame-ancestors 'none'",
  'X-Content-Type-Options': 'nosniff',
  'X-Frame-Options': 'DENY',
  'Referrer-Policy': 'no-referrer',
};
function failure(status, text) {
  return new Response(text, { status, headers: { ...security,
    'Content-Type': 'text/plain; charset=utf-8', 'Cache-Control': 'no-store' } });
}

export async function handleDownload(request, env, fetchUpstream = fetch) {
  const url = new URL(request.url);
  if (!url.pathname.startsWith('/download/')) return env.ASSETS.fetch(request);
  if (request.method !== 'GET' && request.method !== 'HEAD') {
    const response = failure(405, '仅支持 GET 或 HEAD。');
    response.headers.set('Allow', 'GET, HEAD');
    return response;
  }
  const download = release.downloads.find((item) =>
    url.pathname === `/download/${release.version}/${item.platform}`);
  if (!download || url.search) return failure(404, '下载地址不存在，请从官网选择下载。');
  if (request.headers.has('Range')) return failure(416, '备用入口暂不支持断点续传，请重新下载或使用 GitHub 原链。');
  let upstreamUrl = new URL(download.url);
  const filename = upstreamUrl.pathname.split('/').pop();
  if (upstreamUrl.origin !== 'https://github.com' ||
      upstreamUrl.pathname !== `/ForceMind/Linko/releases/download/v${release.version}/${filename}` ||
      !/^Linko-\d+\.\d+\.\d+-dev-(darwin-arm64|windows-amd64)\.zip$/.test(filename) ||
      !Number.isSafeInteger(download.bytes) || download.bytes <= 0 || download.bytes > 128 * 1024 * 1024) {
    return failure(503, '下载配置不可用，请使用 GitHub 原链。');
  }
  const signal = AbortSignal.any([request.signal, AbortSignal.timeout(300000)]);
  try {
    for (let redirects = 0; redirects <= 3; redirects++) {
      // Deliberately forward no visitor cookies, authorization or URL parameters.
      const upstream = await fetchUpstream(upstreamUrl.href, {
        method: request.method, redirect: 'manual', signal,
        headers: { 'Accept': 'application/octet-stream', 'User-Agent': 'Linko-Release-Download' },
      });
      if ([301, 302, 303, 307, 308].includes(upstream.status)) {
        const location = upstream.headers.get('Location');
        await upstream.body?.cancel();
        if (!location || redirects === 3) return failure(502, '下载源重定向异常，请使用 GitHub 原链。');
        const next = new URL(location, upstreamUrl);
        if (next.protocol !== 'https:' || next.port || next.username || next.password ||
            !['release-assets.githubusercontent.com', 'objects.githubusercontent.com'].includes(next.hostname)) {
          return failure(502, '下载源地址异常，请使用 GitHub 原链。');
        }
        upstreamUrl = next;
        continue;
      }
      if (upstream.status !== 200 ||
          upstream.headers.get('Content-Length') !== String(download.bytes) ||
          !/^(application\/(octet-stream|zip|x-zip-compressed))(;|$)/i.test(upstream.headers.get('Content-Type') || '') ||
          upstream.headers.has('Content-Encoding')) {
        await upstream.body?.cancel();
        return failure(502, '下载源暂不可用，请使用 GitHub 原链。');
      }
      // Stream directly; never load the native package into Worker memory.
      return new Response(request.method === 'HEAD' ? null : upstream.body, { headers: {
        ...security, 'Content-Type': 'application/zip',
        'Content-Length': String(download.bytes),
        'Content-Disposition': `attachment; filename="${filename}"`,
        'Cache-Control': 'no-store', 'X-Linko-SHA256': download.sha256,
      } });
    }
  } catch {
    return failure(502, '下载超时或中断，请重试或使用 GitHub 原链。');
  }
  return failure(502, '下载源暂不可用，请使用 GitHub 原链。');
}

export default { fetch(request, env) { return handleDownload(request, env); } };
