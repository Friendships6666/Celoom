import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const names = ['dsh-body.json', 'raw-native-equivalent.json', 'raw-native-original-arguments-NOT-equivalent.json', 'dsh-wire-body.txt', 'raw-native-wire-body.txt', 'synthetic-input.json', 'scenario-bodies.json', 'comparison.json', 'captured-native-context.json', 'header-observations.json', 'source-manifest.json', 'nosafe-gpt-6.1-sol-default-body.json', 'nosafe-deepseek-flash-default-body.json'];
async function hashes() {
  return Promise.all(names.map(async name => createHash('sha256').update(await readFile(join(here, 'fixtures', name))).digest('hex')));
}
function run() {
  const child = spawnSync(process.execPath, [join(here, 'offline-responses.mjs')], { encoding: 'utf8', timeout: 30000 });
  assert.equal(child.status, 0, child.stderr || `signal=${child.signal}`);
  const result = JSON.parse(child.stdout);
  assert.equal(result.ok, true);
  assert.equal(result.realNetworkAttempts, 0);
  assert.equal(result.nativeEquivalent.rawBytesEqual, true);
  return result;
}
test('installed DSH/Responses adapter runs entirely offline and fixtures are deterministic', async () => {
  const first = run();
  const before = await hashes();
  const second = run();
  assert.deepEqual(await hashes(), before, 'all fixture bytes must reproduce across runs');
  assert.deepEqual(second, first);
  const report = JSON.parse(await readFile(join(here, 'fixtures/comparison.json'), 'utf8'));
  assert.equal(report.guardSelfTests.length, 2);
  assert.equal(report.originalArgumentsNativeContrast.structuralDiffs.length, 48);
  assert.equal(report.headerObservations.dsh.authorization, '<present; value never saved>');
  assert.ok(!Object.hasOwn(report.headerObservations.dsh, 'originator'));
});
