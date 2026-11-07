import assert from 'node:assert/strict';
import test from 'node:test';
import { cargoCommand, cargoPlan as planCargo, readAvailableMemory, GIB } from './cargo-with-memory.mjs';

const cargoPlan = (args, memory, threshold, options = {}) =>
  // undefined invokes the production default and reads CARGO_BUILD_JOBS from CI.
  planCargo(args, memory, threshold, { cpuCount: 16, envJobs: 'default', ...options });

test('fixture isolates host job limits while production defaults still honor them', () => {
  const previous = process.env.CARGO_BUILD_JOBS;
  try {
    process.env.CARGO_BUILD_JOBS = '1';
    assert.equal(cargoPlan(['build'], 4 * GIB).jobs, 2);
    assert.equal(planCargo(['build'], 4 * GIB, undefined, { cpuCount: 16 }).jobs, 1);
    assert.equal(cargoPlan(['build'], 4 * GIB, undefined, { envJobs: '1' }).jobs, 1);
  } finally {
    if (previous === undefined) delete process.env.CARGO_BUILD_JOBS;
    else process.env.CARGO_BUILD_JOBS = previous;
  }
});

test('global Cargo options and toolchains cannot bypass the preflight', () => {
  assert.equal(cargoCommand(['+stable', '--config', 'build.jobs=8', '-v', 'build']), 'build');
  assert.equal(cargoCommand(['--color=never', 'test', '--', 'run']), 'test');
  assert.equal(cargoCommand(['metadata', '--manifest-path', 'build']), 'metadata');
  assert.equal(cargoCommand(['--version']), undefined);
});

test('low memory overrides all supported Cargo job flags but preserves application arguments', () => {
  for (const jobs of [[], ['-j', '8'], ['-j8'], ['-j=8'], ['--jobs', '8'], ['--jobs=8'], ['-j', '-1']]) {
    const plan = cargoPlan(['+stable', 'run', '--release', ...jobs, '--', 'serve', '--jobs', '9'], 2 * GIB);
    assert.deepEqual(plan.args, ['+stable', 'run', '--release', '--jobs', '1', '--', 'serve', '--jobs', '9']);
    assert.equal(plan.single, true);
  }
  assert.deepEqual(cargoPlan(['clippy', '--all-targets', '--', '-D', 'warnings'], GIB).args,
    ['clippy', '--all-targets', '--jobs', '1', '--', '-D', 'warnings']);
});

test('tier boundaries select one, two or unrestricted jobs', () => {
  for (const [memory, limit] of [[GIB, 1], [3 * GIB - 1, 1], [3 * GIB, 2],
    [3.95 * GIB, 2], [6 * GIB, 2], [6 * GIB + 1, null], [32 * GIB, null]]) {
    const plan = cargoPlan(['build', '--release', '--locked'], memory);
    assert.equal(plan.memoryLimit, limit);
    assert.deepEqual(plan.args, limit === null ? ['build', '--release', '--locked']
      : ['build', '--release', '--locked', '--jobs', String(limit)]);
  }
});

test('medium memory caps every supported job spelling and preserves forwarded arguments', () => {
  for (const jobs of [[], ['-j', '8'], ['-j8'], ['-j=8'], ['--jobs', '8'], ['--jobs=8'],
    ['-j', '-1'], ['-j-1'], ['-j=-1'], ['--jobs=-1'], ['--jobs=default'], ['-jdefault']]) {
    const plan = cargoPlan(['run', ...jobs, '--', 'serve', '--jobs', '9'], 4 * GIB);
    assert.deepEqual(plan.args, ['run', '--jobs', '2', '--', 'serve', '--jobs', '9']);
    assert.equal(plan.single, false);
  }
});

test('smaller explicit, environment and CPU limits are respected with CLI precedence', () => {
  assert.equal(cargoPlan(['build', '-j1'], 4 * GIB).jobs, 1);
  assert.equal(cargoPlan(['build'], 4 * GIB, undefined, { envJobs: '1' }).jobs, 1);
  assert.equal(cargoPlan(['build', '-j2'], 4 * GIB, undefined, { envJobs: '1' }).jobs, 2);
  assert.equal(cargoPlan(['build'], 4 * GIB, undefined, { envJobs: '8' }).jobs, 2);
  assert.equal(cargoPlan(['build'], 4 * GIB, undefined, { envJobs: 'default' }).jobs, 2);
  assert.equal(cargoPlan(['build'], 4 * GIB, undefined, { cpuCount: 1 }).jobs, 1);
  assert.equal(cargoPlan(['build', '-j-1'], 4 * GIB, undefined, { cpuCount: 2 }).jobs, 1);
  assert.equal(cargoPlan(['build'], 4 * GIB, undefined, { envJobs: '-20' }).jobs, 1);
});

test('sufficient memory preserves original flags and profile options', () => {
  const args = ['test', '--target-dir', 'path with spaces', '--jobs=2', '--', '--test-threads=1'];
  assert.deepEqual(cargoPlan(args, 8 * GIB).args, args);
  assert.deepEqual(cargoPlan(['build', '--release', '--locked'], 16 * GIB).args, ['build', '--release', '--locked']);
  assert.equal(cargoPlan(['test'], 8 * GIB, 12).single, true);
  assert.equal(cargoPlan(['test'], 8 * GIB, 4).jobs, 2);
  assert.equal(cargoPlan(['test'], 8 * GIB + 1, 4).memoryLimit, null);
  assert.deepEqual(cargoPlan(['build', '--jobs=32'], 7 * GIB).args, ['build', '--jobs=32']);
});

test('unknown or invalid memory selects one job; invalid settings fail before launching Cargo', () => {
  for (const memory of [null, undefined, NaN, Infinity, 0, -1]) {
    assert.deepEqual(cargoPlan(['build'], memory).args, ['build', '--jobs', '1']);
  }
  for (const threshold of [0, -1, NaN, Infinity]) assert.throws(() => cargoPlan(['build'], GIB, threshold));
  assert.throws(() => cargoPlan(['build', '--jobs', '--release'], GIB));
  for (const jobs of ['0', 'bogus', '', '1.5', 'Infinity']) {
    assert.throws(() => cargoPlan(['build', `--jobs=${jobs}`], 4 * GIB));
    assert.throws(() => cargoPlan(['build'], 4 * GIB, undefined, { envJobs: jobs }));
  }
});

test('Windows uses the smaller physical, virtual and process memory budgets', () => {
  const available = readAvailableMemory({ system: 'win32', free: () => 16 * GIB, available: () => 12 * GIB,
    execute: (program, args, options) => {
      assert.equal(program, 'powershell.exe');
      assert.equal(options.windowsHide, true);
      assert.equal(options.timeout, 10000);
      return JSON.stringify({ physical: 10 * GIB, virtual: 4 * GIB });
    } });
  assert.equal(available, 4 * GIB);
});

test('Unix considers process limits and failed memory detection is conservative', () => {
  assert.equal(readAvailableMemory({ system: 'linux', free: () => 16 * GIB, available: () => 2 * GIB }), 2 * GIB);
  for (const execute of [() => { throw new Error('unavailable'); }, () => '{}', () => 'invalid']) {
    assert.equal(readAvailableMemory({ system: 'win32', free: () => 16 * GIB, available: () => 16 * GIB, execute }), null);
  }
});
