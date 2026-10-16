import assert from 'node:assert/strict';
import {test} from 'node:test';
import {assertWindowsLocation,windowsLocationKey} from './windows-paths.mjs';

// FIX 3: Namespace equivalence must not relax full-location assertions.
test('drive and UNC namespace prefixes preserve exact location equivalence',()=>{
  assertWindowsLocation(String.raw`\\?\C:\fixture\recovery-profile`,String.raw`C:\fixture\recovery-profile`);
  assertWindowsLocation(String.raw`C:\fixture\recovery-profile`,String.raw`\\?\C:\fixture\recovery-profile`);
  assertWindowsLocation(String.raw`\\?\UNC\server\share\recovery-profile`,String.raw`\\server\share\recovery-profile`);
  assertWindowsLocation(String.raw`C:\fixture\recovery-profile`,String.raw`C:\fixture\recovery-profile`);
});
test('same leaf name, alternate drive, prefix and suffix do not match another location',()=>{
  const expected=String.raw`C:\fixture\recovery-profile`;
  for(const actual of [
    String.raw`\\?\C:\other-fixture\recovery-profile`,
    String.raw`\\?\D:\fixture\recovery-profile`,
    String.raw`\\?\C:\fixture\recovery-profile-other`,
    String.raw`\\?\C:\fixture\recovery-profile\child`,
    String.raw`\\server\share\recovery-profile`,
  ]) assert.throws(()=>assertWindowsLocation(actual,expected),assert.AssertionError);
  assert.throws(()=>assertWindowsLocation(String.raw`\\?\UNC\server\other-share\recovery-profile`,String.raw`\\server\share\recovery-profile`),assert.AssertionError);
});
test('relative or drive-relative locations cannot acquire meaning from the test working directory',()=>{
  for(const value of ['recovery-profile',String.raw`C:recovery-profile`,String.raw`\fixture\recovery-profile`]) {
    assert.throws(()=>windowsLocationKey(value),assert.AssertionError);
  }
});
