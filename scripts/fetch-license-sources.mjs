import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { cargoLicenseMetadata } from "./cargo-license-metadata.mjs";

// Explicit maintenance command only. Builds/checks never download license data.
// Run after reviewing missing entries in third-party-notices.provenance.json.
const root = process.cwd();
const inventory = JSON.parse(
  fs.readFileSync("assets/third-party-notices.provenance.json", "utf8"),
);
const metadata = cargoLicenseMetadata();
const manifestFile = "assets/license-sources.manifest.json";
const existing = fs.existsSync(manifestFile)
  ? JSON.parse(fs.readFileSync(manifestFile, "utf8"))
  : { schemaVersion: 1, packages: [] };
const packages = new Map(existing.packages.map((pkg) => [pkg.id, pkg]));
const trees = new Map(),
  downloads = new Map();
const sha = (value) => crypto.createHash("sha256").update(value).digest("hex");
const noticeName = /^(licen[sc]es?|copying|copyright|notices?)(?:$|[._-])/i;
async function download(url) {
  if (!downloads.has(url))
    downloads.set(
      url,
      (async () => {
        const response = await fetch(url, {
          signal: AbortSignal.timeout(30000),
          headers: { "User-Agent": "Image-Ranker-notice-inventory" },
        });
        if (!response.ok) throw Error(`${response.status} ${url}`);
        const value = Buffer.from(await response.arrayBuffer());
        if (value.length > 2 * 1024 * 1024)
          throw Error(`Oversized notice ${url}`);
        return value;
      })(),
    );
  return downloads.get(url);
}
function save(repository, revision, repoPath, raw) {
  const local = `license-sources/${repository.replace("https://github.com/", "")}/${revision}/${repoPath}`;
  const target = path.resolve("assets", local);
  if (!target.startsWith(path.resolve("assets/license-sources") + path.sep))
    throw Error("Source path escapes source inventory");
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, raw);
  return local;
}
for (const { id } of inventory.issues) {
  const pkg = metadata.packages.find(
    (pkg) => `cargo:${pkg.name}@${pkg.version}` === id,
  );
  if (!pkg) throw Error(`No exact installed package for ${id}`);
  const vcs = JSON.parse(
    fs.readFileSync(
      path.join(path.dirname(pkg.manifest_path), ".cargo_vcs_info.json"),
      "utf8",
    ),
  );
  const repository = pkg.repository.replace(/\.git$/, "").replace(/\/$/, "");
  if (
    !/^https:\/\/github\.com\/[^/]+\/[^/]+$/.test(repository) ||
    !/^[0-9a-f]{40}$/.test(vcs.git.sha1)
  )
    throw Error(`Unsupported exact source ${id}`);
  const revision = vcs.git.sha1,
    packagePath = vcs.path_in_vcs || "",
    repo = repository.slice("https://github.com/".length),
    key = repo + "/" + revision;
  if (!trees.has(key)) {
    const tree = JSON.parse(
      (
        await download(
          `https://api.github.com/repos/${repo}/git/trees/${revision}?recursive=1`,
        )
      ).toString("utf8"),
    );
    if (tree.truncated) throw Error(`Truncated repository tree ${key}`);
    trees.set(key, tree.tree);
  }
  const levels = new Set([""]);
  let dir = packagePath;
  while (dir && dir !== ".") {
    levels.add(dir);
    dir = path.posix.dirname(dir);
  }
  const tree = trees.get(key);
  let matches = tree.filter(
    (entry) =>
      entry.type === "blob" &&
      entry.mode === "100644" &&
      noticeName.test(path.posix.basename(entry.path)) &&
      levels.has(
        path.posix.dirname(entry.path) === "."
          ? ""
          : path.posix.dirname(entry.path),
      ),
  );
  // Source headers are an explicit authoritative notice, not a fabricated LICENSE.
  if (id === "cargo:selectors@0.38.0")
    matches = [tree.find((entry) => entry.path === `${packagePath}/lib.rs`)];
  if (!matches.length || matches.some((entry) => !entry))
    throw Error(`No applicable notice found at pinned revision for ${id}`);
  const sources = [];
  for (const entry of matches.sort((a, b) =>
    a.path.localeCompare(b.path, "en"),
  )) {
    const url = `https://raw.githubusercontent.com/${repo}/${revision}/${entry.path}`;
    const raw = await download(url);
    const blob = crypto
      .createHash("sha1")
      .update(`blob ${raw.length}\0`)
      .update(raw)
      .digest("hex");
    if (blob !== entry.sha) throw Error(`Git blob integrity mismatch: ${url}`);
    const local = save(repository, revision, entry.path, raw);
    sources.push({
      path: local,
      repositoryPath: entry.path,
      url,
      revision,
      sha256: sha(raw),
      gitBlob: entry.sha,
      kind:
        id === "cargo:selectors@0.38.0"
          ? "upstream-source-header"
          : "upstream-notice",
    });
  }
  packages.set(id, {
    id,
    repository,
    revision,
    packagePath,
    license: pkg.license,
    sources,
  });
  console.log(
    `${id}: ${sources.map((source) => source.repositoryPath).join(", ")}`,
  );
}
fs.writeFileSync(
  manifestFile,
  JSON.stringify(
    {
      schemaVersion: 1,
      packages: [...packages.values()].sort((a, b) =>
        a.id.localeCompare(b.id, "en"),
      ),
    },
    null,
    2,
  ) + "\n",
);

// Supplemental canonical terms are explicitly referenced by pinned declarations.
// The linked pages are preserved byte-for-byte; bundling uses a fixed audited text extractor.
for (const pkg of packages.values()) {
  const names =
    pkg.repository === "https://github.com/madsmtm/objc2"
      ? ["MIT", "Zlib", "Apache-2.0"]
      : pkg.id === "cargo:selectors@0.38.0"
        ? ["MPL-2.0"]
        : [];
  if (!names.length) continue;
  pkg.sources = pkg.sources.filter(
    (source) => source.kind !== "linked-license-terms",
  );
  for (const license of names) {
    const url =
      license === "MIT"
        ? "https://opensource.org/license/MIT"
        : license === "Zlib"
          ? "https://zlib.net/zlib_license.html"
          : license === "Apache-2.0"
            ? "https://www.apache.org/licenses/LICENSE-2.0"
            : "https://mozilla.org/MPL/2.0/";
    const declaration = pkg.sources.find((source) =>
      fs.readFileSync(path.join("assets", source.path), "utf8").includes(url),
    );
    if (!declaration)
      throw Error(`Pinned declaration does not reference ${url}`);
    const raw = await download(url);
    const local = `license-sources/canonical/${license}/${license === "Apache-2.0" ? "terms.txt" : "source.html"}`;
    fs.mkdirSync(path.dirname(path.join("assets", local)), { recursive: true });
    fs.writeFileSync(path.join("assets", local), raw);
    pkg.sources.push({
      path: local,
      url,
      revision: pkg.revision,
      sha256: sha(raw),
      kind: "linked-license-terms",
      license,
      referencedBy: declaration.repositoryPath,
    });
  }
  pkg.reviewRequired =
    pkg.id === "cargo:selectors@0.38.0"
      ? "No standalone license file exists at the pinned package or ancestor paths. The original MPL-2.0 source notice and explicitly linked canonical terms are preserved. Review covered-source availability and other MPL distribution obligations before release."
      : "The pinned upstream declaration raises an Apple SDK derived-work licensing caveat. Canonical MIT text includes unfilled copyright placeholders and is supplied as referenced terms, not invented package attribution. Review upstream licensing and SDK distribution obligations before release.";
}
fs.writeFileSync(
  manifestFile,
  JSON.stringify(
    {
      schemaVersion: 1,
      packages: [...packages.values()].sort((a, b) =>
        a.id.localeCompare(b.id, "en"),
      ),
    },
    null,
    2,
  ) + "\n",
);
