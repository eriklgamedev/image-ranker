import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import { validateLicenseSources } from "./license-source-validation.mjs";

// Collect the installed packages' own notices. SPDX declarations do not replace
// copyright/license text; missing originals remain an explicit release failure.
const check = process.argv.includes("--check");
const root = process.cwd();
const cargoHome =
  process.env.CARGO_HOME ||
  (fs.existsSync(".cargo-home") ? path.resolve(".cargo-home") : undefined);
const targets = [
  "x86_64-pc-windows-msvc",
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
];
const graphs = targets.map((target) => {
  const metadata = spawnSync(
    "cargo",
    [
      "metadata",
      "--manifest-path",
      "src-tauri/Cargo.toml",
      "--format-version",
      "1",
      "--locked",
      "--offline",
      "--filter-platform",
      target,
    ],
    {
      encoding: "utf8",
      windowsHide: true,
      maxBuffer: 64 * 1024 * 1024,
      env: { ...process.env, ...(cargoHome ? { CARGO_HOME: cargoHome } : {}) },
    },
  );
  if (metadata.status !== 0)
    throw new Error(
      metadata.stderr ||
        "Cargo dependency metadata is unavailable; fetch locked dependencies first.",
    );
  return JSON.parse(metadata.stdout);
});
const cargo = graphs[0],
  lock = JSON.parse(fs.readFileSync("package-lock.json", "utf8"));
const packages = [
  ...new Map(
    graphs.flatMap((graph) => graph.packages).map((pkg) => [pkg.id, pkg]),
  ).values(),
];
const kept = new Set(),
  targetPackages = {};
for (const [index, graph] of graphs.entries()) {
  const nodes = new Map(graph.resolve.nodes.map((node) => [node.id, node])),
    visited = new Set();
  function visit(id) {
    if (visited.has(id)) return;
    visited.add(id);
    kept.add(id);
    for (const dependency of nodes.get(id)?.deps || [])
      if (dependency.dep_kinds.some((kind) => kind.kind !== "dev"))
        visit(dependency.pkg);
  }
  visit(graph.resolve.root);
  targetPackages[targets[index]] = graph.packages
    .filter((pkg) => visited.has(pkg.id) && pkg.id !== graph.resolve.root)
    .map((pkg) => `cargo:${pkg.name}@${pkg.version}`)
    .sort();
}
const inputs = [
  ...Object.entries(lock.packages)
    .filter(([directory, pkg]) => directory && !pkg.dev)
    .map(([directory, pkg]) => ({
      ecosystem: "npm",
      directory: path.resolve(directory),
      name: JSON.parse(
        fs.readFileSync(path.join(directory, "package.json"), "utf8"),
      ).name,
      version: pkg.version,
      spdx: pkg.license || "NOASSERTION",
    })),
  ...packages
    .filter((pkg) => kept.has(pkg.id) && pkg.id !== cargo.resolve.root)
    .map((pkg) => ({
      ecosystem: "cargo",
      directory: path.dirname(pkg.manifest_path),
      name: pkg.name,
      version: pkg.version,
      spdx: pkg.license || "NOASSERTION",
      licenseFile: pkg.license_file,
      repository: pkg.repository,
    })),
].sort((a, b) =>
  `${a.ecosystem}:${a.name}@${a.version}`.localeCompare(
    `${b.ecosystem}:${b.name}@${b.version}`,
    "en",
  ),
);
const sha = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const sourceManifestFile = "assets/license-sources.manifest.json";
const sourceManifest = fs.existsSync(sourceManifestFile)
  ? JSON.parse(fs.readFileSync(sourceManifestFile, "utf8"))
  : { schemaVersion: 1, packages: [] };
if (
  sourceManifest.schemaVersion !== 1 ||
  !Array.isArray(sourceManifest.packages)
)
  throw Error("Invalid pinned license source manifest");
const sourceEntries = new Map(
  sourceManifest.packages.map((pkg) => [pkg.id, pkg]),
);
const inputIds = new Set(
  inputs.map((pkg) => `${pkg.ecosystem}:${pkg.name}@${pkg.version}`),
);
if (
  inputIds.size !== inputs.length ||
  sourceEntries.size !== sourceManifest.packages.length
)
  throw Error("Duplicate notice package identity");
for (const id of sourceEntries.keys())
  if (!inputIds.has(id))
    throw Error(
      `Stale license source for a package outside the supported dependency graphs: ${id}`,
    );
const issues = [],
  reviewRequired = [],
  provenance = [],
  notices = [];
for (const pkg of inputs) {
  const id = `${pkg.ecosystem}:${pkg.name}@${pkg.version}`;
  const candidates = new Set();
  function add(file) {
    if (!fs.existsSync(file) || !fs.statSync(file).isFile()) return;
    const relative = path.relative(pkg.directory, file);
    if (relative.startsWith("..") || path.isAbsolute(relative))
      throw new Error(`Notice path escapes installed package ${id}`);
    candidates.add(path.resolve(file));
  }
  for (const entry of fs.readdirSync(pkg.directory, { withFileTypes: true })) {
    if (
      !/^(licen[sc]es?|copying|copyright|notices?)(?:$|[._-])/i.test(entry.name)
    )
      continue;
    const file = path.join(pkg.directory, entry.name);
    if (entry.isFile()) add(file);
    else if (entry.isDirectory())
      for (const nested of fs.readdirSync(file, { withFileTypes: true }))
        if (nested.isFile()) add(path.join(file, nested.name));
  }
  if (pkg.licenseFile) add(path.resolve(pkg.directory, pkg.licenseFile));
  const sources = [];
  const texts = [...candidates]
    .sort()
    .map((file) => {
      const bytes = fs.readFileSync(file);
      if (bytes.length > 2 * 1024 * 1024)
        throw new Error(`Oversized notice in ${id}`);
      const text = new TextDecoder("utf-8", { fatal: true })
        .decode(bytes)
        .replace(/\r\n/g, "\n")
        .trim();
      const name = path.relative(pkg.directory, file).replaceAll("\\", "/");
      sources.push({ path: name, sha256: sha(bytes) });
      return `--- ${name} ---\n${text}`;
    })
    .filter(Boolean);
  const upstream = sourceEntries.get(id);
  if (upstream) {
    for (const { source, text } of validateLicenseSources(
      root,
      pkg,
      upstream,
    )) {
      sources.push(source);
      const label =
        source.kind === "linked-license-terms"
          ? `Canonical ${source.license} terms explicitly linked by the upstream declaration (reference text, not package-specific copyright attribution)`
          : source.kind === "upstream-source-header"
            ? "Original source license header"
            : "Original upstream notice";
      texts.push(`--- ${label}: ${source.url} ---\n${text}`);
    }
    if (upstream.reviewRequired)
      reviewRequired.push({ id, reason: upstream.reviewRequired });
  }
  if (!texts.length)
    issues.push({
      id,
      reason: "The installed package has no bundled license/notice text.",
    });
  notices.push({
    id,
    component: pkg.name,
    version: pkg.version,
    spdx: pkg.spdx,
    text:
      texts.join("\n\n") ||
      `License text is missing from this installed package. Declared license: ${pkg.spdx}. Review is required before distribution.`,
  });
  provenance.push({
    id,
    license: pkg.spdx,
    ...(upstream
      ? {
          upstream: {
            repository: upstream.repository,
            revision: upstream.revision,
            packagePath: upstream.packagePath,
          },
        }
      : {}),
    sources,
  });
}
const files = {
  "assets/third-party-notices.json": notices,
  "assets/third-party-notices.provenance.json": {
    schemaVersion: 1,
    scope:
      "Production npm packages and non-development Cargo graphs for the declared Windows/macOS targets, including build dependencies. This is a conservative notice inventory, not an application license.",
    targets,
    targetPackages,
    sourceManifest: fs.existsSync(sourceManifestFile)
      ? sha(fs.readFileSync(sourceManifestFile))
      : null,
    lockfiles: {
      npm: sha(fs.readFileSync("package-lock.json")),
      cargo: sha(fs.readFileSync("src-tauri/Cargo.lock")),
    },
    issues,
    reviewRequired,
    packages: provenance,
  },
};
for (const [file, value] of Object.entries(files)) {
  const expected = JSON.stringify(value, null, 2) + "\n";
  if (check) {
    if (!fs.existsSync(file) || fs.readFileSync(file, "utf8") !== expected)
      throw new Error(
        `Stale notice inventory: run npm run notices:generate (${file})`,
      );
  } else {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, expected);
  }
}
console.log(
  `${check ? "Verified" : "Collected"} ${notices.length} package notices; ${issues.length} missing originals; ${reviewRequired.length} upstream licensing reviews retained.`,
);
if (issues.length) {
  console.error(
    issues.map((issue) => `${issue.id}: ${issue.reason}`).join("\n"),
  );
  process.exitCode = 1;
}
