import { availableParallelism, freemem } from 'node:os';

const GiB = 1024 ** 3;

export function chooseTestWorkers(memoryBytes: number, cpus: number, requested?: string): number {
  if (!Number.isSafeInteger(cpus) || cpus < 1) throw new Error('Invalid available CPU count');
  const memoryLimit = !Number.isFinite(memoryBytes) || memoryBytes < 3 * GiB
    ? 1 : memoryBytes <= 6 * GiB ? 2 : 4;
  let limit = memoryLimit;
  if (requested !== undefined) {
    if (!/^[1-9]\d*$/.test(requested) || !Number.isSafeInteger(Number(requested))) {
      throw new Error('BRIDGE_TEST_WORKERS must be a positive integer');
    }
    limit = Math.min(limit, Number(requested));
  }
  return Math.min(cpus, limit);
}

export function testWorkers(): number {
  let memory = Number.NaN;
  try {
    memory = Math.min(freemem(), process.availableMemory());
  } catch {
    // Unknown process limits: use one browser worker.
  }
  return chooseTestWorkers(memory, availableParallelism(), process.env.BRIDGE_TEST_WORKERS);
}
