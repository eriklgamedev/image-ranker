import fs from "node:fs";
import path from "node:path";
import assert from "node:assert/strict";
import { test } from "node:test";
import { validateLicenseSources } from "./license-source-validation.mjs";
import { cargoLicenseMetadata } from "./cargo-license-metadata.mjs";
const root = process.cwd();
const manifest = JSON.parse(
  fs.readFileSync("assets/license-sources.manifest.json", "utf8"),
);
const cargo = cargoLicenseMetadata();
function pkgFor(entry) {
  const pkg = cargo.packages.find(
    (pkg) => `cargo:${pkg.name}@${pkg.version}` === entry.id,
  );
  return {
    ecosystem: "cargo",
    directory: path.dirname(pkg.manifest_path),
    name: pkg.name,
    version: pkg.version,
    spdx: pkg.license,
    repository: pkg.repository,
  };
}
const sample = manifest.packages.find(
  (pkg) => pkg.id === "cargo:alloc-stdlib@0.3.0",
);
test("every supplemented notice is bound to installed crate revision, Git blob, URL and local SHA256", () => {
  assert.equal(manifest.packages.length, 19);
  for (const entry of manifest.packages)
    assert.ok(validateLicenseSources(root, pkgFor(entry), entry).length > 0);
});
test("changed local source hash is rejected", () => {
  const entry = structuredClone(sample);
  entry.sources[0].sha256 = "0".repeat(64);
  assert.throws(
    () => validateLicenseSources(root, pkgFor(sample), entry),
    /hash mismatch/,
  );
});
test("stale package revision and latest-branch substitutions are rejected", () => {
  const stale = structuredClone(sample);
  stale.revision = "0".repeat(40);
  assert.throws(
    () => validateLicenseSources(root, pkgFor(sample), stale),
    /Stale pinned/,
  );
  const branch = structuredClone(sample);
  branch.sources[0].url = branch.sources[0].url.replace(
    branch.revision,
    "main",
  );
  assert.throws(
    () => validateLicenseSources(root, pkgFor(sample), branch),
    /Unpinned source URL/,
  );
});
test("missing sources, unbound terms and inventory escapes are rejected", () => {
  const empty = structuredClone(sample);
  empty.sources = [];
  assert.throws(
    () => validateLicenseSources(root, pkgFor(sample), empty),
    /Missing pinned/,
  );
  const missing = structuredClone(sample);
  missing.sources[0].path = "license-sources/does-not-exist";
  assert.throws(
    () => validateLicenseSources(root, pkgFor(sample), missing),
    /ENOENT/,
  );
  const escape = structuredClone(sample);
  escape.sources[0].path = "../package.json";
  assert.throws(
    () => validateLicenseSources(root, pkgFor(sample), escape),
    /Invalid license source path/,
  );
  const entry = structuredClone(
    manifest.packages.find((pkg) => pkg.id === "cargo:objc2@0.6.4"),
  );
  entry.sources.find(
    (source) => source.kind === "linked-license-terms",
  ).referencedBy = "nonexistent.md";
  assert.throws(
    () => validateLicenseSources(root, pkgFor(entry), entry),
    /Unbound canonical/,
  );
});
test("complete canonical terms are readable and upstream caveats stay distinct", () => {
  const objc = manifest.packages.find((pkg) => pkg.id === "cargo:objc2@0.6.4");
  const texts = validateLicenseSources(root, pkgFor(objc), objc);
  assert.match(
    texts.find((item) => item.source.kind === "upstream-notice").text,
    /Apple SDKs/,
  );
  const mit = texts.find((item) => item.source.license === "MIT").text;
  assert.match(mit, /Copyright <YEAR> <COPYRIGHT HOLDER>/);
  assert.match(mit, /Permission is hereby granted/);
  assert.match(mit, /OTHER DEALINGS IN THE SOFTWARE/);
  assert.doesNotMatch(mit, /<script|<div|State of the Source/);
  assert.match(
    texts.find((item) => item.source.license === "Apache-2.0").text,
    /END OF TERMS AND CONDITIONS/,
  );
  assert.match(
    texts.find((item) => item.source.license === "Zlib").text,
    /This notice may not be removed/,
  );
  const selector = manifest.packages.find(
    (pkg) => pkg.id === "cargo:selectors@0.38.0",
  );
  const mpl = validateLicenseSources(root, pkgFor(selector), selector);
  assert.match(
    mpl.find((item) => item.source.kind === "upstream-source-header").text,
    /Mozilla Public/,
  );
  assert.match(
    mpl.find((item) => item.source.license === "MPL-2.0").text,
    /Exhibit B/,
  );
  assert.ok(selector.reviewRequired);
  assert.ok(objc.reviewRequired);
});
test("supported target union includes macOS-only crates, not just Windows metadata", () => {
  const inventory = JSON.parse(
    fs.readFileSync("assets/third-party-notices.provenance.json", "utf8"),
  );
  assert.deepEqual(inventory.targets, [
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
  ]);
  const windows = new Set(inventory.targetPackages["x86_64-pc-windows-msvc"]);
  const bundled = new Set(inventory.packages.map((pkg) => pkg.id));
  for (const target of ["aarch64-apple-darwin", "x86_64-apple-darwin"]) {
    const ids = inventory.targetPackages[target];
    assert.ok(ids.includes("cargo:objc2-app-kit@0.3.2"));
    assert.ok(ids.includes("cargo:swift-rs@1.0.8"));
    const exclusive = ids.filter((id) => !windows.has(id));
    assert.ok(exclusive.length >= 20);
    assert.ok(exclusive.every((id) => bundled.has(id)));
  }
  assert.equal(inventory.issues.length, 0);
  assert.ok(
    inventory.reviewRequired.some(
      (item) => item.id === "cargo:selectors@0.38.0",
    ),
  );
});
