import { readFile, stat } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';

const root = fileURLToPath(new URL('./dist/', import.meta.url));
const manifest = JSON.parse(await readFile(join(root, 'asset-manifest.json'), 'utf8'));
assert.equal(manifest.createdBy, 'linko-static-site-builder-v1');
const files = Object.keys(manifest.files);
assert(files.length > 3 && files.length < 100);
const secret = /-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----|CLOUDFLARE_API_TOKEN\s*[=:]|(?:\/Users\/|\/private\/var\/|node_modules\/|target\/local-validation\/)/;
for (const name of files) {
  assert(!name.startsWith('.') && !name.includes('..') && !name.includes('\\'));
  const data = await readFile(join(root, name));
  assert.equal(createHash('sha256').update(data).digest('hex'), manifest.files[name].sha256, name);
  assert(data.length <= 25 * 1024 * 1024);
  if (/\.(html|css|js|json|svg|xml|txt)$/.test(name) || name.startsWith('_')) assert(!secret.test(data.toString('utf8')), `Private build input found: ${name}`);
  if (!name.endsWith('.html')) continue;
  const html = data.toString('utf8');
  assert(/<html[^>]+lang="zh-CN"/.test(html), `Chinese page language missing: ${name}`);
  assert(!/__SITE_ORIGIN__/.test(html), 'Unresolved deployment token');
  const ids = new Set([...html.matchAll(/\bid="([^"]+)"/g)].map((m) => m[1]));
  for (const match of html.matchAll(/\b(?:href|src)="([^"]+)"/g)) {
    const target = match[1];
    if (target.startsWith('#')) { assert(ids.has(target.slice(1)), `${name}: missing anchor ${target}`); continue; }
    if (/^https:\/\//.test(target)) continue;
    assert(!/^[a-z]+:/i.test(target) && !target.startsWith('//'), `Unexpected URL: ${target}`);
    if (/^\/download\/0\.3\.17-dev\/(macos|windows)$/.test(target)) continue;
    const clean = target.split(/[?#]/)[0];
    let path = clean.startsWith('/') ? resolve(root, '.' + clean) : resolve(root, name, '..', clean);
    if (clean.endsWith('/')) path = join(path, 'index.html');
    assert(path.startsWith(root));
    assert((await stat(path)).isFile(), `Missing local asset: ${target}`);
  }
  for (const match of html.matchAll(/<a\b[^>]*target="_blank"[^>]*>/g)) assert(/rel="[^"]*noopener/.test(match[0]), 'Unsafe external target');
}
const homepage = await readFile(join(root, 'index.html'), 'utf8');
assert.equal([...homepage.matchAll(/<h1\b/g)].length, 1);
assert(homepage.includes('0.3.17') && /开发|预览/.test(homepage));
assert(/GitHub/.test(homepage));
console.log(`PASS: ${files.length} public hashes, asset size, Chinese pages, local links/anchors, external targets and private-data checks`);
