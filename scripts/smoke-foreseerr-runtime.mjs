#!/usr/bin/env node
// Start a staged Foreseerr runtime the way src/supervisor.rs does and check
// the desktop child contract end to end: the readiness record names the
// pinned version and revision, the status API and the bundled frontend
// answer, and a shutdown on the control channel exits 0 after releasing the
// instance lock. Uses a throwaway data directory.
//
// usage: scripts/smoke-foreseerr-runtime.mjs <runtime-dir> [KEY=VALUE ...]
//
// KEY=VALUE pairs go to the child's environment only, e.g. the AppImage's
// LD_LIBRARY_PATH when smoke-testing the runtime inside an extracted
// AppImage.
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

const READY_PREFIX = 'FORESEERR_DESKTOP_READY ';
const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const [runtimeArgument, ...childEnvironment] = process.argv.slice(2);
if (!runtimeArgument) {
  console.error('usage: smoke-foreseerr-runtime.mjs <runtime-dir> [KEY=VALUE ...]');
  process.exit(2);
}
const runtime = resolve(runtimeArgument);
const pin = (file) => readFileSync(join(root, file), 'utf8').trim();
const version = pin('foreseerr.version');
const revision = pin('foreseerr.rev');
const executable = join(runtime, process.platform === 'win32' ? 'foreseerr.exe' : 'foreseerr');
const data = mkdtempSync(join(tmpdir(), 'foreseerr-smoke-'));
const config = join(data, 'standalone');
const lock = join(config, 'state', 'instance.lock');

const env = { ...process.env };
for (const key of [
  'FORESEERR_NODE_URL',
  'FORESEERR_PLUGIN',
  'FORESEERR_BASE_PATH',
  'FORESEERR_PUBLIC_PATH',
  'RUST_LOG',
  'DB_TYPE',
]) {
  delete env[key];
}
Object.assign(env, {
  FORESEERR_RUNTIME: 'desktop',
  FORESEERR_STANDALONE: '1',
  FORESEERR_APP_ROOT: runtime,
  NODE_ENV: 'production',
  CONFIG_DIRECTORY: config,
  CACHE_DIRECTORY: join(data, 'cache'),
  LOG_DIRECTORY: join(config, 'logs'),
  HOST: '127.0.0.1',
  PORT: '0',
  FORESEERR_COMMIT: revision,
  FORESEER_CACHE_LIMIT_BYTES: String(64 * 1024 * 1024),
});
for (const pair of childEnvironment) {
  const separator = pair.indexOf('=');
  if (separator <= 0) throw new Error(`not KEY=VALUE: ${pair}`);
  env[pair.slice(0, separator)] = pair.slice(separator + 1);
}

const child = spawn(executable, [], { cwd: runtime, env, stdio: ['pipe', 'pipe', 'pipe'] });
const childLog = [];
createInterface({ input: child.stderr }).on('line', (line) => {
  childLog.push(line);
  if (childLog.length > 200) childLog.shift();
});
const exited = new Promise((done) => {
  child.on('error', (error) => done({ code: null, signal: String(error) }));
  child.on('exit', (code, signal) => done({ code, signal }));
});

function within(promise, ms, what) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, fail) => {
      timer = setTimeout(() => fail(new Error(`${what} within ${ms / 1000} s`)), ms);
    }),
  ]).finally(() => clearTimeout(timer));
}

function check(condition, message) {
  if (!condition) throw new Error(message);
}

try {
  const ready = await within(
    new Promise((done, fail) => {
      createInterface({ input: child.stdout }).on('line', (line) => {
        if (line.startsWith(READY_PREFIX)) done(JSON.parse(line.slice(READY_PREFIX.length)));
      });
      exited.then(({ code, signal }) =>
        fail(new Error(`exited before readiness (${code ?? signal})`)),
      );
    }),
    90_000,
    'no readiness record',
  );
  console.log('ready:', JSON.stringify(ready));
  check(ready.protocolVersion === 1, 'protocolVersion is not 1');
  check(ready.foreseerrVersion === version, `version ${ready.foreseerrVersion}, pinned ${version}`);
  check(ready.commit === revision, `commit ${ready.commit}, pinned ${revision}`);
  check(ready.pid === child.pid, `pid ${ready.pid}, spawned ${child.pid}`);
  check(/^http:\/\/127\.0\.0\.1:[1-9]\d*$/.test(ready.origin), `origin ${ready.origin}`);
  check(readFileSync(lock, 'utf8').trim() === String(child.pid), 'instance.lock does not hold the pid');

  const status = await (await fetch(`${ready.origin}/api/v1/status`)).json();
  check(status.runtime === 'desktop' && status.managed === true, `status ${JSON.stringify(status)}`);
  check(status.version === version, `status version ${status.version}`);
  const index = await fetch(`${ready.origin}/`);
  check(index.ok && (await index.text()).includes('<html'), `frontend answered ${index.status}`);

  child.stdin.write('{"type":"shutdown","deadlineMs":2000}\n');
  const { code, signal } = await within(exited, 4_000, 'no exit after the shutdown message');
  check(code === 0, `exit ${code ?? signal}`);
  check(!existsSync(lock), 'instance.lock left behind');
  rmSync(data, { recursive: true, force: true });
  console.log(`smoke-foreseerr-runtime: Foreseerr ${version} ok`);
} catch (error) {
  child.kill('SIGKILL');
  console.error(childLog.join('\n'));
  console.error(`smoke-foreseerr-runtime: ${error.message} (data in ${data})`);
  process.exit(1);
}
