import assert from 'node:assert/strict';
import test from 'node:test';
import { chooseTestWorkers } from '../web/test-workers.ts';

const GiB = 1024 ** 3;
test('browser workers follow memory boundaries and CPU limits', () => {
  for (const [memory, expected] of [[NaN, 1], [Infinity, 1], [-1, 1], [0, 1],
    [3 * GiB - 1, 1], [3 * GiB, 2], [6 * GiB, 2], [6 * GiB + 1, 4]]) {
    assert.equal(chooseTestWorkers(memory, 16), expected);
    assert.equal(chooseTestWorkers(memory, 1), 1);
  }
  assert.equal(chooseTestWorkers(16 * GiB, 3), 3);
});
test('explicit limits can reduce workers but cannot bypass memory or CPU limits', () => {
  assert.equal(chooseTestWorkers(16 * GiB, 8, '2'), 2);
  assert.equal(chooseTestWorkers(4 * GiB, 8, '8'), 2);
  assert.equal(chooseTestWorkers(16 * GiB, 2, '8'), 2);
  assert.equal(chooseTestWorkers(NaN, 8, '4'), 1);
});
test('invalid overrides fail before launching browsers', () => {
  for (const value of ['', '0', '-1', '1.5', '50%', 'Infinity', '9007199254740992']) {
    assert.throws(() => chooseTestWorkers(8 * GiB, 8, value), /BRIDGE_TEST_WORKERS/);
  }
  assert.throws(() => chooseTestWorkers(8 * GiB, 0), /CPU/);
});
