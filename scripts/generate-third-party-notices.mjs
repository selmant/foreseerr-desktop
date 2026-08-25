#!/usr/bin/env node
// Produce a deterministic, redistributable inventory from the *deployed*
// production tree. This intentionally reports metadata rather than attempting
// to concatenate arbitrary upstream license text.
import { lstat, readdir, readFile, realpath, writeFile } from 'node:fs/promises';
import { basename, join } from 'node:path';

const [bundleDirectory, nodeVersion, outputFile] = process.argv.slice(2);
if (!bundleDirectory || !nodeVersion || !outputFile) {
  throw new Error('usage: generate-third-party-notices.mjs <bundle> <node-version> <output>');
}

const packageFiles = new Set();
async function collect(directory) {
  let entries;
  try {
    entries = await readdir(directory, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    if (entry.name === '.bin' || entry.name === '.cache') continue;
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === 'node_modules' || directory.includes('node_modules')) {
        await collect(path);
      }
    } else if (entry.name === 'package.json' && directory.includes('node_modules')) {
      try {
        packageFiles.add(await realpath(path));
      } catch {
        // A dangling optional-dependency symlink is not a shipped package.
      }
    }
  }
}

await collect(join(bundleDirectory, 'node_modules'));
const packages = [];
for (const packageFile of packageFiles) {
  try {
    const pkg = JSON.parse(await readFile(packageFile, 'utf8'));
    if (!pkg.name || !pkg.version) continue;
    const repository = typeof pkg.repository === 'string'
      ? pkg.repository
      : pkg.repository?.url ?? '';
    packages.push({
      name: pkg.name,
      version: pkg.version,
      license: pkg.license ?? 'UNSPECIFIED',
      repository,
    });
  } catch {
    // Ignore malformed optional package metadata; runtime execution does not
    // depend on a notice generator parsing it.
  }
}
packages.sort((a, b) => `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`));
const unique = packages.filter((pkg, index) =>
  index === 0 || `${pkg.name}@${pkg.version}` !== `${packages[index - 1].name}@${packages[index - 1].version}`,
);

const lines = [
  'Foreseer Desktop third-party notices',
  '',
  `Node.js ${nodeVersion} — MIT (full text: node/LICENSE)`,
  'Foreseerr — MIT (full text: foreseerr/LICENSE when supplied by its package)',
  '',
  'Production npm dependencies:',
  ...unique.map((pkg) => `${pkg.name}@${pkg.version} — ${pkg.license}${pkg.repository ? ` — ${pkg.repository}` : ''}`),
  '',
];
await writeFile(outputFile, lines.join('\n'));
console.log(`wrote ${unique.length} dependency notices to ${basename(outputFile)}`);
