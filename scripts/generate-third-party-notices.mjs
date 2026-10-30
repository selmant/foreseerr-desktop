#!/usr/bin/env node
// Produce a deterministic, redistributable inventory of what the bundled
// Foreseerr runtime ships: the Rust crates linked into its binary (from
// `cargo tree`) and the npm packages of Foreseerr's production dependency
// tree, which the bundled frontend is built from. This intentionally reports
// metadata rather than attempting to concatenate arbitrary upstream license
// text.
//
// usage: generate-third-party-notices.mjs <foreseerr-checkout> <cargo-tree-output> <output>
//
// <cargo-tree-output> is
//   cargo tree --locked -e normal --target <triple> --prefix none --format '{p}|{l}|{r}'
// run in the checkout's backend-rs/.
import { existsSync } from 'node:fs';
import { readFile, realpath, writeFile } from 'node:fs/promises';
import { basename, dirname, join } from 'node:path';

const [checkout, cargoTreeFile, outputFile] = process.argv.slice(2);
if (!checkout || !cargoTreeFile || !outputFile) {
  throw new Error(
    'usage: generate-third-party-notices.mjs <foreseerr-checkout> <cargo-tree-output> <output>',
  );
}

const manifest = JSON.parse(await readFile(join(checkout, 'package.json'), 'utf8'));

function byNameAndVersion(a, b) {
  return `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`);
}

function unique(packages) {
  return packages
    .sort(byNameAndVersion)
    .filter(
      (pkg, index, sorted) =>
        index === 0 ||
        `${pkg.name}@${pkg.version}` !== `${sorted[index - 1].name}@${sorted[index - 1].version}`,
    );
}

function line(pkg) {
  return `${pkg.name}@${pkg.version} — ${pkg.license}${pkg.repository ? ` — ${pkg.repository}` : ''}`;
}

// `name vX.Y.Z [(proc-macro)|(/path)]|license|repository[ (*)]`; the root
// crate is Foreseerr itself.
const crates = [];
for (const entry of (await readFile(cargoTreeFile, 'utf8')).split('\n')) {
  const [spec, license = '', repository = ''] = entry.replace(/ \(\*\)$/, '').split('|');
  const match = /^(\S+) v(\S+)/.exec(spec ?? '');
  if (!match || match[1] === 'foreseerr') continue;
  crates.push({
    name: match[1],
    version: match[2],
    license: license || 'UNSPECIFIED',
    repository,
  });
}
if (crates.length === 0) {
  throw new Error(`no crates in ${cargoTreeFile}`);
}

// Node's resolution: the nearest node_modules/<name> from the dependent
// package upwards to the checkout.
function resolvePackage(name, fromDirectory) {
  let directory = fromDirectory;
  for (;;) {
    const candidate = join(directory, 'node_modules', name, 'package.json');
    if (existsSync(candidate)) return candidate;
    const parent = dirname(directory);
    if (directory === checkout || parent === directory) return null;
    directory = parent;
  }
}

const npmPackages = [];
const seen = new Set();
const pending = Object.keys(manifest.dependencies ?? {}).map((name) => [name, checkout]);
while (pending.length > 0) {
  const [name, from] = pending.pop();
  const packageFile = resolvePackage(name, from);
  if (!packageFile) continue; // an optional dependency for another platform
  const real = await realpath(packageFile);
  if (seen.has(real)) continue;
  seen.add(real);
  let pkg;
  try {
    pkg = JSON.parse(await readFile(real, 'utf8'));
  } catch {
    continue;
  }
  if (pkg.name && pkg.version) {
    npmPackages.push({
      name: pkg.name,
      version: pkg.version,
      license:
        (typeof pkg.license === 'string' ? pkg.license : pkg.license?.type) ?? 'UNSPECIFIED',
      repository: typeof pkg.repository === 'string' ? pkg.repository : (pkg.repository?.url ?? ''),
    });
  }
  for (const dependency of Object.keys({
    ...pkg.dependencies,
    ...pkg.optionalDependencies,
  })) {
    pending.push([dependency, dirname(real)]);
  }
}
if (npmPackages.length === 0) {
  throw new Error(`no npm packages under ${checkout}/node_modules (run bun install)`);
}

const crateNotices = unique(crates);
const npmNotices = unique(npmPackages);
const lines = [
  'Foreseer Desktop third-party notices',
  '',
  `Foreseerr ${manifest.version} — ${manifest.license ?? 'MIT'} — https://github.com/selmant/foreseerr`,
  '  (bundled runtime in foreseerr/)',
  '',
  'Rust crates linked into the Foreseerr runtime:',
  ...crateNotices.map(line),
  '',
  "npm packages of Foreseerr's production dependency tree (bundled web UI):",
  ...npmNotices.map(line),
  '',
];
await writeFile(outputFile, lines.join('\n'));
console.log(
  `wrote ${crateNotices.length} crate and ${npmNotices.length} npm notices to ${basename(outputFile)}`,
);
