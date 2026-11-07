import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';

const dist = new URL('./dist/', import.meta.url);
const manifestBytes = await readFile(new URL('asset-manifest.json', dist));
const manifest = JSON.parse(manifestBytes);
const origins = process.argv.slice(2);
assert(origins.length > 0, 'Supply production and/or deployment HTTPS origins');
const receipts = [];
for (const origin of origins) {
  const base = new URL(origin);
  assert(base.protocol === 'https:' && base.hostname.endsWith('.pages.dev') &&
    !base.username && !base.password && !base.port && base.pathname === '/' &&
    !base.search && !base.hash, 'Expected a Cloudflare Pages HTTPS origin');
  const assets = Object.entries(manifest.files).filter(([name]) => !name.startsWith('_'));
  assets.push(['asset-manifest.json', {
    sha256: createHash('sha256').update(manifestBytes).digest('hex'),
  }]);
  const files = [];
  for (const [name, info] of assets) {
    // Pages serves HTML at extensionless URLs and redirects *.html with 308.
    const path = name === 'index.html' ? '/' : `/${name.replace(/\.html$/, '')}`;
    const response = await fetch(new URL(path, base), {
      redirect: 'error', cache: 'no-store', signal: AbortSignal.timeout(15000),
    });
    assert.equal(response.status, 200, `${base.origin}${path}`);
    const data = Buffer.from(await response.arrayBuffer());
    const sha256 = createHash('sha256').update(data).digest('hex');
    assert.equal(sha256, info.sha256, `Online bytes differ: ${path}`);
    assert.equal(response.headers.get('x-content-type-options'), 'nosniff');
    const csp = response.headers.get('content-security-policy') || '';
    assert(csp.includes("default-src 'none'") && csp.includes("frame-ancestors 'none'"));
    const contentType = response.headers.get('content-type') || '';
    const expected = { html: 'text/html', css: 'text/css', js: 'javascript', json: 'application/json', png: 'image/png', svg: 'image/svg+xml', xml: 'xml', txt: 'text/plain' };
    assert(contentType.includes(expected[name.split('.').pop()]), `Wrong MIME: ${path}`);
    files.push({ path, sha256, bytes: data.length });
  }
  const missing = await fetch(new URL('/__linko_missing_page_check__', base), {
    redirect: 'error', signal: AbortSignal.timeout(15000),
  });
  assert.equal(missing.status, 404, 'Missing pages must return 404');
  assert.equal(createHash('sha256').update(Buffer.from(await missing.arrayBuffer())).digest('hex'),
    manifest.files['404.html'].sha256, 'Expected the Chinese 404 page');
  receipts.push({ origin: base.origin, files, missingPageStatus: missing.status });
  console.log(`PASS ${base.origin}: ${files.length} online hashes, MIME, CSP, nosniff and Chinese 404`);
}
await writeFile(new URL('./deployment.local.json', import.meta.url), JSON.stringify({
  verifiedAt: new Date().toISOString(), sourceCommit: manifest.sourceCommit,
  siteVersion: manifest.siteVersion, browserTested: false, receipts,
}, null, 2) + '\n');
