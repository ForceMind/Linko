import { mkdir, readFile, writeFile, readdir, lstat, rename } from 'node:fs/promises';
import { resolve, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash, randomUUID } from 'node:crypto';
import { execFileSync } from 'node:child_process';

const root = fileURLToPath(new URL('.', import.meta.url));
const source = join(root, 'src');
const output = join(root, 'dist');
const marker = 'linko-static-site-builder-v1';
const sourceCommit = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
const topFiles = new Set(['index.html', '404.html', 'styles.css', 'main.js', 'release.json', 'robots.txt', 'sitemap.xml', '_headers', '_redirects', '_worker.js', '_routes.json']);
const assetExtensions = /\.(png|svg|webp|jpe?g|woff2)$/i;
const paths = [];

async function inventory(directory) {
  for (const name of await readdir(directory)) {
    const path = join(directory, name);
    const info = await lstat(path);
    const rel = relative(source, path).replaceAll('\\', '/');
    if (info.isSymbolicLink()) throw new Error(`Symlink is not publishable: ${rel}`);
    if (info.isDirectory()) {
      if (rel !== 'assets' && !rel.startsWith('assets/')) throw new Error(`Unexpected public directory: ${rel}`);
      await inventory(path);
    } else if (info.isFile()) {
      if (!topFiles.has(rel) && !(rel.startsWith('assets/') && assetExtensions.test(rel))) throw new Error(`Unexpected public file: ${rel}`);
      if (info.size > 25 * 1024 * 1024) throw new Error(`Pages asset too large: ${rel}`);
      paths.push({ path, rel });
    } else throw new Error(`Unsupported public entry: ${rel}`);
  }
}

await inventory(source);
if (!paths.some(({ rel }) => rel === 'index.html')) throw new Error('Homepage is missing');
let origin = process.env.SITE_ORIGIN || '';
if (origin) {
  const parsed = new URL(origin);
  if (parsed.protocol !== 'https:' || parsed.pathname !== '/' || parsed.search || parsed.hash) throw new Error('SITE_ORIGIN must be an HTTPS origin');
  origin = parsed.origin;
}
const staging = join(root, `.build-history/staging-${randomUUID()}`);
await mkdir(staging, { recursive: true });
const manifest = { createdBy: marker, siteVersion: '0.1.4', sourceCommit, appVersion: '0.3.17-dev', files: {} };
for (const { path, rel } of paths.sort((a, b) => a.rel.localeCompare(b.rel))) {
  let bytes = await readFile(path);
  if (/\.(html|xml|txt)$/.test(rel)) {
    const text = bytes.toString('utf8');
    if (text.includes('__SITE_ORIGIN__') && !origin) throw new Error('Canonical URL requires SITE_ORIGIN');
    bytes = Buffer.from(text.replaceAll('__SITE_ORIGIN__', origin));
  }
  if (rel === '_worker.js') bytes = Buffer.from(bytes.toString('utf8').replace('__RELEASE_METADATA__', await readFile(join(source, 'release.json'), 'utf8')));
  await mkdir(resolve(staging, rel, '..'), { recursive: true });
  await writeFile(join(staging, rel), bytes, { flag: 'wx' });
  manifest.files[rel] = { sha256: createHash('sha256').update(bytes).digest('hex'), bytes: bytes.length };
}
await writeFile(join(staging, 'asset-manifest.json'), JSON.stringify(manifest, null, 2) + '\n', { flag: 'wx' });
let exists = false;
try { await lstat(output); exists = true; } catch (error) { if (error.code !== 'ENOENT') throw error; }
if (exists) {
  let old;
  try { old = JSON.parse(await readFile(join(output, 'asset-manifest.json'), 'utf8')); }
  catch { throw new Error('Refusing to replace existing output without a builder manifest'); }
  if (old.createdBy !== marker) throw new Error('Refusing to replace output not created by this builder');
  await rename(output, join(root, `.build-history/previous-${randomUUID()}`));
}
await rename(staging, output);
console.log(`Built ${paths.length} public files for ${sourceCommit.slice(0, 7)} in website/dist`);
