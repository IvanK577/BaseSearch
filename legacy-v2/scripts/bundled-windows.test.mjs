import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const bundleRoot = path.join(repoRoot, "dist", "BaseSearch");
const releaseTool = path.join(repoRoot, "scripts", "release-package.mjs");
const manifestPath = path.join(bundleRoot, "release-manifest.json");

const bundledFiles = [
  "dist/BaseSearch/BaseSearch.exe",
  "dist/BaseSearch/LICENSE",
  "dist/BaseSearch/Open Base Search.cmd",
  "dist/BaseSearch/README.txt",
  "dist/BaseSearch/base-search-cli.exe",
  "dist/BaseSearch/release-manifest.json",
].sort((left, right) => left.localeCompare(right, "en"));

const runtimeInputs = [
  "Cargo.lock",
  "Cargo.toml",
  "src",
  "web-ui/index.html",
  "web-ui/package-lock.json",
  "web-ui/package.json",
  "web-ui/public",
  "web-ui/src",
  "web-ui/tsconfig.json",
  "web-ui/vite.config.ts",
];

test("the repository Windows bundle has a complete verified package layout", () => {
  const verification = spawnSync(
    process.execPath,
    [releaseTool, "verify", "--root", bundleRoot, "--platform", "windows"],
    { cwd: repoRoot, encoding: "utf8" },
  );
  assert.equal(verification.status, 0, verification.stderr || verification.stdout);

  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  const cargoManifest = readFileSync(path.join(repoRoot, "Cargo.toml"), "utf8");
  const cargoVersion = cargoManifest.match(
    /^\[package\][\s\S]*?^version\s*=\s*"([^"]+)"/m,
  )?.[1];
  const frontendVersion = JSON.parse(
    readFileSync(path.join(repoRoot, "web-ui", "package.json"), "utf8"),
  ).version;

  assert.ok(cargoVersion, "Cargo.toml must declare the package version");
  assert.equal(manifest.version, cargoVersion);
  assert.equal(frontendVersion, cargoVersion);
  assert.equal(manifest.platform, "windows");
  assert.equal(manifest.arch, "x86_64");
  assert.deepEqual(manifest.features, ["browser"]);
});

const gitProbe = spawnSync("git", ["rev-parse", "--is-inside-work-tree"], {
  cwd: repoRoot,
  encoding: "utf8",
});
const isGitCheckout = gitProbe.status === 0 && gitProbe.stdout.trim() === "true";

test(
  "GitHub source archives include a bundle built from unchanged runtime sources",
  { skip: !isGitCheckout },
  () => {
    const tracked = spawnSync("git", ["ls-files", "--", "dist/BaseSearch"], {
      cwd: repoRoot,
      encoding: "utf8",
    });
    assert.equal(tracked.status, 0, tracked.stderr);
    assert.deepEqual(
      tracked.stdout
        .trim()
        .split(/\r?\n/)
        .filter(Boolean)
        .sort((left, right) => left.localeCompare(right, "en")),
      bundledFiles,
      "the GitHub-generated source ZIP would omit part of dist/BaseSearch",
    );

    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    assert.match(
      manifest.git_sha,
      /^[0-9a-f]{12}$/,
      "the bundled package must come from a clean source revision",
    );
    const sourceRevision = spawnSync(
      "git",
      ["rev-parse", "--verify", `${manifest.git_sha}^{commit}`],
      { cwd: repoRoot, encoding: "utf8" },
    );
    assert.equal(sourceRevision.status, 0, sourceRevision.stderr);

    const sourceDiff = spawnSync(
      "git",
      ["diff", "--quiet", manifest.git_sha, "--", ...runtimeInputs],
      { cwd: repoRoot, encoding: "utf8" },
    );
    assert.equal(
      sourceDiff.status,
      0,
      `dist/BaseSearch is stale: runtime inputs changed after ${manifest.git_sha}`,
    );
  },
);

test(
  "GitHub source archives preserve the package bytes recorded in the manifest",
  { skip: !isGitCheckout },
  () => {
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    for (const [relative, expectedHash] of Object.entries(manifest.files_sha256)) {
      const repositoryPath = `dist/BaseSearch/${relative}`;
      const blob = spawnSync("git", ["show", `:${repositoryPath}`], {
        cwd: repoRoot,
        encoding: null,
        maxBuffer: 64 * 1024 * 1024,
      });
      assert.equal(blob.status, 0, blob.stderr?.toString("utf8"));
      const actualHash = createHash("sha256").update(blob.stdout).digest("hex");
      assert.equal(
        actualHash,
        expectedHash,
        `${repositoryPath} would change inside GitHub's source ZIP`,
      );
    }
  },
);
