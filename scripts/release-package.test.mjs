import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const releaseTool = path.join(repoRoot, "scripts", "release-package.mjs");

test("source launchers build locked React assets before compiling Rust", () => {
  for (const file of ["start.sh", "run.sh"]) {
    const source = readFileSync(path.join(repoRoot, file), "utf8");
    const install = source.indexOf("npm --prefix web-ui ci");
    const frontend = source.indexOf("npm --prefix web-ui run build");
    const rust = source.indexOf("cargo build --release");
    assert.ok(install >= 0, `${file} must install locked frontend dependencies`);
    assert.ok(frontend > install, `${file} must build React after npm ci`);
    assert.ok(rust > frontend, `${file} must compile Rust after React`);
  }
});

test("local packages identify builds made from a dirty source tree", () => {
  for (const file of [
    "package-release.ps1",
    "package-linux.sh",
    "package-macos.sh",
  ]) {
    const source = readFileSync(path.join(repoRoot, "scripts", file), "utf8");
    assert.match(source, /git status --porcelain --untracked-files=normal/);
    assert.match(source, /dirty/);
  }
});

test("manifest hashes final signed bytes and records Windows Authenticode", (context) => {
  const root = packageFixture(context, "windows");
  const binary = path.join(root, "BaseSearch.exe");
  writeFileSync(binary, "signed executable bytes");

  const write = invokeRelease(
    "write-manifest",
    manifestArgs(root, "windows", "signed", false),
  );
  assert.equal(write.status, 0, write.stderr);
  const manifest = JSON.parse(readFileSync(path.join(root, "release-manifest.json"), "utf8"));
  assert.deepEqual(manifest.signing, {
    windows_authenticode: "signed",
    macos_codesign: "not-applicable",
    macos_notarization: "not-applicable",
  });
  assert.deepEqual(manifest.data_policy, {
    default: "per-user-unversioned",
    existing_portable_database: "reuse-in-place",
    sibling_portable_database: "reuse-after-explicit-confirmation",
    automatic_database_move: false,
  });
  assert.equal(
    manifest.files_sha256["BaseSearch.exe"],
    createHash("sha256").update("signed executable bytes").digest("hex"),
  );

  const verify = invokeRelease("verify", [
    "--root",
    root,
    "--platform",
    "windows",
    "--require-signed",
    "true",
  ]);
  assert.equal(verify.status, 0, verify.stderr);

  const manifestPath = path.join(root, "release-manifest.json");
  const manifestText = readFileSync(manifestPath, "utf8");
  const unsafePolicy = JSON.parse(manifestText);
  unsafePolicy.data_policy.automatic_database_move = true;
  writeFileSync(manifestPath, `${JSON.stringify(unsafePolicy, null, 2)}\n`);
  const unsafe = invokeRelease("verify", ["--root", root, "--platform", "windows"]);
  assert.notEqual(unsafe.status, 0, "automatic database moves must be rejected");
  assert.match(unsafe.stderr, /database location and migration policy/i);
  writeFileSync(manifestPath, manifestText);

  writeFileSync(binary, "changed after manifest");
  const changed = invokeRelease("verify", ["--root", root, "--platform", "windows"]);
  assert.notEqual(changed.status, 0, "post-manifest binary changes must be rejected");
});

test("stable verification rejects unsigned Windows and unstapled macOS packages", (context) => {
  const windows = packageFixture(context, "windows");
  assert.equal(
    invokeRelease(
      "write-manifest",
      manifestArgs(windows, "windows", "unsigned", false),
    ).status,
    0,
  );
  const unsigned = invokeRelease("verify", [
    "--root",
    windows,
    "--platform",
    "windows",
    "--require-signed",
    "true",
  ]);
  assert.notEqual(unsigned.status, 0);
  assert.match(unsigned.stderr, /Authenticode signing is required/i);

  const macos = packageFixture(context, "macos");
  const codeResources = path.join(
    macos,
    "BaseSearch.app",
    "Contents",
    "_CodeSignature",
    "CodeResources",
  );
  mkdirSync(path.dirname(codeResources), { recursive: true });
  writeFileSync(codeResources, "codesign resource seal");
  const macWrite = invokeRelease(
    "write-manifest",
    manifestArgs(macos, "macos", "signed", false),
  );
  assert.equal(macWrite.status, 0, macWrite.stderr);
  const unstapled = invokeRelease("verify", [
    "--root",
    macos,
    "--platform",
    "macos",
    "--require-signed",
    "true",
  ]);
  assert.notEqual(unstapled.status, 0);
  assert.match(unstapled.stderr, /notarized and stapled/i);
});

test("notarized macOS packages preserve the stapled ticket through archiving", () => {
  const packager = readFileSync(path.join(repoRoot, "scripts", "package-macos.sh"), "utf8");
  const workflow = readFileSync(
    path.join(repoRoot, ".github", "workflows", "ci.yml"),
    "utf8",
  );
  assert.match(
    packager,
    /ditto -c -k --keepParent "\$package_dir" "\$archive_path"/,
    "the final notarized archive must preserve macOS metadata",
  );
  assert.match(
    workflow,
    /ditto -x -k "\$archive" target\/archive-smoke\/macos/,
    "CI must restore stapled metadata before validating the package",
  );
});

test("package verification rejects databases and source files", (context) => {
  for (const leaked of ["customer.db", "src/main.rs"]) {
    const root = packageFixture(context, "linux");
    const write = invokeRelease(
      "write-manifest",
      manifestArgs(root, "linux", "unsigned", false),
    );
    assert.equal(write.status, 0, write.stderr);
    const leakPath = path.join(root, ...leaked.split("/"));
    mkdirSync(path.dirname(leakPath), { recursive: true });
    writeFileSync(leakPath, "must not ship");
    const result = invokeRelease("verify", ["--root", root, "--platform", "linux"]);
    assert.notEqual(result.status, 0, `${leaked} must be rejected`);
  }
});

test("portable packages do not require an untrackable empty data directory", (context) => {
  const root = packageFixture(context, "windows");
  rmSync(path.join(root, "data"), { recursive: true });

  const write = invokeRelease(
    "write-manifest",
    manifestArgs(root, "windows", "unsigned", false),
  );

  assert.equal(write.status, 0, write.stderr);
  const verify = invokeRelease("verify", [
    "--root",
    root,
    "--platform",
    "windows",
  ]);
  assert.equal(verify.status, 0, verify.stderr);
});

function packageFixture(context, platform) {
  const temp = mkdtempSync(path.join(tmpdir(), `base-search-${platform}-`));
  context.after(() => rmSync(temp, { recursive: true, force: true }));
  const root = path.join(temp, `BaseSearch-2.0.0-${platform}-x86_64`);
  mkdirSync(path.join(root, "data"), { recursive: true });
  const files = {
    windows: [
      "BaseSearch.exe",
      "LICENSE",
      "Open Base Search.cmd",
      "README.txt",
      "base-search-cli.exe",
    ],
    macos: [
      "BaseSearch.app/Contents/Info.plist",
      "BaseSearch.app/Contents/MacOS/BaseSearch",
      "LICENSE",
      "README.txt",
      "base-search-cli",
    ],
    linux: ["BaseSearch", "LICENSE", "Open Base Search.sh", "README.txt", "base-search-cli"],
  }[platform];
  for (const relative of files) {
    const file = path.join(root, ...relative.split("/"));
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, relative);
    if (platform !== "windows" && !relative.endsWith(".plist") && relative !== "LICENSE" && relative !== "README.txt") {
      chmodSync(file, 0o755);
    }
  }
  return root;
}

function manifestArgs(root, platform, signing, notarized) {
  return [
    "--root",
    root,
    "--platform",
    platform,
    "--arch",
    "x86_64",
    "--version",
    "2.0.0",
    "--git-sha",
    "395b4e4",
    "--epoch",
    "1700000000",
    "--signing",
    signing,
    "--notarized",
    String(notarized),
  ];
}

function invokeRelease(command, args) {
  return spawnSync(process.execPath, [releaseTool, command, ...args], {
    cwd: repoRoot,
    encoding: "utf8",
  });
}
