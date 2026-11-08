import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
const release = JSON.parse(await readFile(new URL('./src/release.json', import.meta.url)));
const origin = new URL(process.argv[2]);
assert(origin.protocol === 'https:' && origin.hostname.endsWith('.pages.dev') &&
  !origin.username && !origin.password && !origin.port && origin.pathname === '/' && !origin.search);
for (const download of release.downloads) {
  const url = new URL(`/download/${release.version}/${download.platform}`, origin);
  const head = await fetch(url, { method: 'HEAD', redirect: 'error', signal: AbortSignal.timeout(30000) });
  assert.equal(head.status, 200);
  assert.equal(head.headers.get('Content-Length'), String(download.bytes));
  assert.equal(head.headers.get('X-Linko-SHA256'), download.sha256);
  const response = await fetch(url, { redirect: 'error', signal: AbortSignal.timeout(300000) });
  assert.equal(response.status, 200);
  assert.equal(response.headers.get('Content-Type'), 'application/zip');
  const hash = createHash('sha256');
  let bytes = 0;
  for await (const chunk of response.body) {
    bytes += chunk.length;
    assert(bytes <= download.bytes, 'Download exceeds published length');
    hash.update(chunk);
  }
  assert.equal(bytes, download.bytes);
  assert.equal(hash.digest('hex'), download.sha256);
  console.log(`PASS ${download.platform}: ${bytes} bytes, HEAD metadata and full ZIP SHA-256 match GitHub release`);
}
for (const suffix of ['/download/0.3.17-dev/linux', '/download/0.3.17-dev/macos?url=https://example.com']) {
  assert.equal((await fetch(new URL(suffix, origin), { signal: AbortSignal.timeout(15000) })).status, 404);
}
console.log('PASS invalid download targets rejected online');
