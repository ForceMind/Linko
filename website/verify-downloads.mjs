import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
const release = JSON.parse(await readFile(new URL('./src/release.json', import.meta.url)));
const origin = new URL('https://gh-proxy.org');
assert(origin.protocol === 'https:' && origin.hostname.endsWith('.pages.dev') &&
  !origin.username && !origin.password && !origin.port && origin.pathname === '/' && !origin.search);
for (const download of release.downloads) {
  const url = new URL(`/${download.url}`, origin);
  const response = await fetch(url, { redirect: 'follow', signal: AbortSignal.timeout(300000) });
  assert.equal(response.status, 200);
  assert(response.url.startsWith('https://'));
  const hash = createHash('sha256');
  let bytes = 0;
  for await (const chunk of response.body) {
    bytes += chunk.length;
    assert(bytes <= download.bytes, 'Download exceeds published length');
    hash.update(chunk);
  }
  assert.equal(bytes, download.bytes);
  assert.equal(hash.digest('hex'), download.sha256);
  console.log(`PASS ${download.platform}: ${bytes} bytes, full third-party proxy ZIP SHA-256 matches GitHub release`);
}
