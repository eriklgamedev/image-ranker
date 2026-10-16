import assert from 'node:assert/strict';
import path from 'node:path';

// FIX 1: Compare complete absolute Windows path spellings in one namespace.
// This does not resolve file identities, symlinks, case, or filename aliases.
export function windowsLocationKey(value) {
  assert.equal(typeof value,'string','Expected a Windows path string');
  assert.ok(/^[a-z]:[\\/]/i.test(value)||/^\\\\[^\\]+\\[^\\]+/.test(value),'Expected a fully qualified Windows location');
  return path.win32.toNamespacedPath(path.win32.resolve(value));
}

export function assertWindowsLocation(actual,expected,message='Expected the exact Windows location') {
  assert.equal(windowsLocationKey(actual),windowsLocationKey(expected),message);
}
