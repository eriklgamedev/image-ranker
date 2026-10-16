import test from 'node:test';
import assert from 'node:assert/strict';
import {desktopProfile} from './desktop-profile.mjs';

test('ordinary desktop acceptance retains its original deadline and selected workflows', () => {
  assert.deepEqual(desktopProfile(undefined), {name:'standard', specs:['./tests/desktop/workflow.e2e.mjs'], timeoutMs:180000});
  assert.deepEqual(desktopProfile(''), desktopProfile(undefined));
});

test('capacity qualification gives its own twenty-five-minute deadline room to preserve evidence', () => {
  const value = desktopProfile('decision-capacity', ['--spec', 'tests\\desktop\\decision-capacity.e2e.mjs']);
  assert.equal(value.timeoutMs, 30 * 60 * 1000);
  assert.deepEqual(value.specs, ['./tests/desktop/decision-capacity.e2e.mjs']);
  assert.deepEqual(desktopProfile('decision-capacity', ['--spec=./tests/desktop/decision-capacity.e2e.mjs']), value);
});

test('invalid profiles and spec substitutions fail before any desktop process starts', () => {
  for (const profile of ['3000000', 'other', 'decision-capacity ']) assert.throws(() => desktopProfile(profile));
  for (const args of [['--spec'], ['--spec='], ['--spec','tests/desktop/workflow.e2e.mjs'],
    ['--spec','tests/desktop/decision-capacity.e2e.mjs','--spec=tests/desktop/workflow.e2e.mjs'],
    ['--spec','../tests/desktop/decision-capacity.e2e.mjs']]) assert.throws(() => desktopProfile('decision-capacity', args));
});
