import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { handleDownload } from './dist/_worker.js';
const release = JSON.parse(await readFile(new URL('./src/release.json', import.meta.url)));
const path = `/download/${release.version}/macos`;
const req = (suffix = '', init) => new Request(`https://example.pages.dev${path}${suffix}`, init);
const noNetwork = () => { throw new Error('Unexpected network'); };
const origin = () => new Response('PK-test', { headers: {
  'Content-Type': 'application/octet-stream', 'Content-Length': String(release.downloads[0].bytes),
} });

test('unrelated requests use static assets', async () => {
  const res = await handleDownload(new Request('https://example.pages.dev/'), {
    ASSETS: { fetch: () => new Response('static') },
  }, noNetwork);
  assert.equal(await res.text(), 'static');
});
test('unknown platforms, versions, query, POST and Range do not contact upstream', async () => {
  for (const request of [req('?url=https://private.invalid'),
    new Request('https://example.pages.dev/download/1.0.0-dev/macos'),
    new Request('https://example.pages.dev/download/0.3.17-dev/linux')]) {
    assert.equal((await handleDownload(request, {}, noNetwork)).status, 404);
  }
  assert.equal((await handleDownload(req('', { method: 'POST' }), {}, noNetwork)).status, 405);
  assert.equal((await handleDownload(req('', { headers: { Range: 'bytes=0-1' } }), {}, noNetwork)).status, 416);
});
test('only fixed release upstream; visitor credentials are not forwarded', async () => {
  let calls = 0;
  const res = await handleDownload(req('', { headers: { Cookie: 'private', Authorization: 'private' } }), {}, async (url, init) => {
    calls++;
    assert.equal(url, release.downloads[0].url);
    assert.equal(init.redirect, 'manual');
    assert(!new Headers(init.headers).has('Cookie'));
    assert(!new Headers(init.headers).has('Authorization'));
    return origin();
  });
  assert.equal(calls, 1);
  assert.equal(res.status, 200);
  assert.equal(await res.text(), 'PK-test');
  assert.equal(res.headers.get('X-Linko-SHA256'), release.downloads[0].sha256);
  assert.match(res.headers.get('Content-Disposition'), /attachment; filename="Linko-/);
  assert.equal(res.headers.get('X-Content-Type-Options'), 'nosniff');
});
test('HEAD returns metadata without a body', async () => {
  const res = await handleDownload(req('', { method: 'HEAD' }), {}, async (_, init) => {
    assert.equal(init.method, 'HEAD'); return origin();
  });
  assert.equal(res.status, 200);
  assert.equal(await res.text(), '');
});
test('official release asset redirect succeeds', async () => {
  let calls = 0;
  const res = await handleDownload(req(), {}, async () => ++calls === 1
    ? new Response(null, { status: 302, headers: { Location: 'https://release-assets.githubusercontent.com/asset?signature=public' } })
    : origin());
  assert.equal(res.status, 200);
  assert.equal(calls, 2);
});
test('private, insecure and credentialed redirects are rejected', async () => {
  for (const location of ['http://release-assets.githubusercontent.com/x', 'https://127.0.0.1/x',
    'https://github.com/another/repo', 'https://user@release-assets.githubusercontent.com/x']) {
    let calls = 0;
    const res = await handleDownload(req(), {}, async () => {
      calls++; return new Response(null, { status: 302, headers: { Location: location } });
    });
    assert.equal(res.status, 502); assert.equal(calls, 1);
  }
});
test('redirect loops are bounded', async () => {
  let calls = 0;
  const res = await handleDownload(req(), {}, async () => {
    calls++; return new Response(null, { status: 302, headers: { Location: 'https://release-assets.githubusercontent.com/loop' } });
  });
  assert.equal(res.status, 502); assert.equal(calls, 4);
});
test('wrong length, HTML, unavailable upstream and network timeout fail closed', async () => {
  for (const upstream of [new Response('HTML', { headers: { 'Content-Type': 'text/html' } }),
    new Response(null, { status: 404 }),
    new Response('x', { headers: { 'Content-Type': 'application/zip', 'Content-Length': '1' } })]) {
    assert.equal((await handleDownload(req(), {}, async () => upstream)).status, 502);
  }
  const res = await handleDownload(req(), {}, async () => { throw new Error('timeout'); });
  assert.equal(res.status, 502); assert.equal(res.headers.get('Cache-Control'), 'no-store');
});
