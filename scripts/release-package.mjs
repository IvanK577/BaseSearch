import { createHash } from "node:crypto";
import {
  chmodSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  statSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";

const PRIVATE_EXTENSIONS = new Set([
  ".csv",
  ".db",
  ".duckdb",
  ".feather",
  ".jsonl",
  ".ods",
  ".parquet",
  ".sqlite",
  ".sqlite3",
  ".tsv",
  ".xls",
  ".xlsb",
  ".xlsm",
  ".xlsx",
]);

const SOURCE_EXTENSIONS = new Set([
  ".c",
  ".cc",
  ".cpp",
  ".h",
  ".hpp",
  ".jsx",
  ".m",
  ".map",
  ".mm",
  ".rs",
  ".swift",
  ".toml",
  ".ts",
  ".tsx",
]);

function fail(message) {
  throw new Error(message);
}

function parseArgs(values) {
  const result = {};
  for (let index = 0; index < values.length; index += 2) {
    const name = values[index];
    const value = values[index + 1];
    if (!name?.startsWith("--") || value === undefined) {
      fail(`Invalid argument sequence near ${name ?? "<end>"}`);
    }
    result[name.slice(2)] = value;
  }
  return result;
}

function required(args, name) {
  const value = args[name];
  if (!value) fail(`Missing --${name}`);
  return value;
}

function booleanArg(args, name, fallback = false) {
  const value = args[name];
  if (value === undefined) return fallback;
  if (value === "true") return true;
  if (value === "false") return false;
  fail(`--${name} must be true or false`);
}

function signingDeclaration(args, platform) {
  const signing = args.signing ?? "unsigned";
  if (!new Set(["signed", "unsigned"]).has(signing)) {
    fail("--signing must be signed or unsigned");
  }
  const notarized = booleanArg(args, "notarized");
  if (platform !== "macos" && notarized) {
    fail("--notarized true is valid only for macOS packages");
  }
  if (signing !== "signed" && notarized) {
    fail("A notarized macOS package must also be signed");
  }
  return {
    windows_authenticode: platform === "windows" ? signing : "not-applicable",
    macos_codesign: platform === "macos" ? signing : "not-applicable",
    macos_notarization:
      platform === "macos" ? (notarized ? "stapled" : "not-notarized") : "not-applicable",
  };
}

function relativeUnix(root, item) {
  return path.relative(root, item).split(path.sep).join("/");
}

function walk(root) {
  const entries = [];
  function visit(current) {
    for (const name of readdirSync(current).sort((a, b) => a.localeCompare(b, "en"))) {
      const full = path.join(current, name);
      const info = lstatSync(full);
      if (info.isSymbolicLink()) fail(`Package must not contain symlinks: ${full}`);
      entries.push({ full, relative: relativeUnix(root, full), info });
      if (info.isDirectory()) visit(full);
    }
  }
  visit(root);
  return entries;
}

function expectedFiles(platform, signing) {
  if (platform === "windows") {
    return [
      "BaseSearch.exe",
      "LICENSE",
      "Open Base Search.cmd",
      "README.txt",
      "base-search-cli.exe",
      "release-manifest.json",
    ];
  }
  if (platform === "macos") {
    const files = [
      "BaseSearch.app/Contents/Info.plist",
      "BaseSearch.app/Contents/MacOS/BaseSearch",
      "LICENSE",
      "README.txt",
      "base-search-cli",
      "release-manifest.json",
    ];
    if (signing.macos_codesign === "signed") {
      files.push("BaseSearch.app/Contents/_CodeSignature/CodeResources");
    }
    if (signing.macos_notarization === "stapled") {
      // `xcrun stapler staple` stores the notarization ticket as an extra
      // top-level CodeResources file inside Contents.
      files.push("BaseSearch.app/Contents/CodeResources");
    }
    return files;
  }
  if (platform === "linux") {
    return [
      "BaseSearch",
      "LICENSE",
      "Open Base Search.sh",
      "README.txt",
      "base-search-cli",
      "release-manifest.json",
    ];
  }
  fail(`Unsupported platform: ${platform}`);
}

function assertSafePackage(root, platform, manifestRequired, signing) {
  if (!existsSync(root) || !statSync(root).isDirectory()) {
    fail(`Package root is missing: ${root}`);
  }
  const dataDir = path.join(root, "data");
  if (!existsSync(dataDir) || !statSync(dataDir).isDirectory()) {
    fail("Package must contain an empty data directory");
  }
  if (readdirSync(dataDir).length !== 0) fail("Package data directory is not empty");

  const entries = walk(root);
  const files = entries
    .filter((entry) => entry.info.isFile())
    .map((entry) => entry.relative)
    .sort();
  const expected = expectedFiles(platform, signing).filter(
    (name) => manifestRequired || name !== "release-manifest.json",
  );
  if (files.join("\n") !== [...expected].sort().join("\n")) {
    fail(
      `Unexpected package layout.\nExpected:\n${[...expected].sort().join("\n")}\nActual:\n${files.join("\n")}`,
    );
  }

  for (const entry of entries) {
    const parts = entry.relative.toLowerCase().split("/");
    if (parts.some((part) => [".git", "exports", "fixtures", "src", "tests", "uploads", "web-ui"].includes(part))) {
      fail(`Private or development directory found in package: ${entry.relative}`);
    }
    const lower = entry.relative.toLowerCase();
    const extension = path.extname(lower);
    if (
      PRIVATE_EXTENSIONS.has(extension) ||
      SOURCE_EXTENSIONS.has(extension) ||
      lower.endsWith("-wal") ||
      lower.endsWith("-shm") ||
      lower.endsWith(".db-wal") ||
      lower.endsWith(".db-shm")
    ) {
      fail(`Private data file found in package: ${entry.relative}`);
    }
  }

  // NTFS does not expose portable POSIX execute bits through Node. Target
  // runners still enforce them when building and verifying Unix packages.
  if (platform !== "windows" && process.platform !== "win32") {
    const executables =
      platform === "macos"
        ? ["BaseSearch.app/Contents/MacOS/BaseSearch", "base-search-cli"]
        : ["BaseSearch", "base-search-cli", "Open Base Search.sh"];
    for (const name of executables) {
      if ((statSync(path.join(root, name)).mode & 0o111) === 0) {
        fail(`Executable bit is missing: ${name}`);
      }
    }
  }
}

function sha256(file) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}

function normalizeTimes(root, epoch) {
  const time = new Date(Math.max(epoch, 315532800) * 1000);
  const entries = walk(root).sort((a, b) => b.relative.length - a.relative.length);
  for (const entry of entries) utimesSync(entry.full, time, time);
  utimesSync(root, time, time);
}

function renderReadme(args) {
  const platform = required(args, "platform");
  const signing = signingDeclaration(args, platform);
  const replacements = {
    "{{ARCH}}": required(args, "arch"),
    "{{GIT_SHA}}": required(args, "git-sha"),
    "{{PLATFORM}}": platform,
    "{{SOURCE_DATE_EPOCH}}": required(args, "epoch"),
    "{{VERSION}}": required(args, "version"),
  };
  if (platform === "windows") {
    replacements["{{START_INSTRUCTIONS}}"] =
      "Run Open Base Search.cmd or BaseSearch.exe.";
    replacements["{{CLI_INSTRUCTIONS}}"] =
      "Run base-search-cli.exe from PowerShell or Command Prompt.";
    replacements["{{PLATFORM_NOTE}}"] =
      signing.windows_authenticode === "signed"
        ? "This portable Windows package is Authenticode signed and does not require installation."
        : "This portable local Windows build is unsigned and does not require installation.";
  } else if (platform === "macos") {
    replacements["{{START_INSTRUCTIONS}}"] = signing.macos_codesign === "signed"
      ? "Open BaseSearch.app."
      : "Open BaseSearch.app. If macOS blocks an unsigned local build, use the context menu and choose Open.";
    replacements["{{CLI_INSTRUCTIONS}}"] =
      "Run ./base-search-cli from Terminal.";
    replacements["{{PLATFORM_NOTE}}"] =
      signing.macos_notarization === "stapled"
        ? "This .app is Developer ID signed, notarized, and stapled."
        : signing.macos_codesign === "signed"
          ? "This local .app is Developer ID signed but has not been notarized or stapled."
          : "This local .app build is unsigned. Public distribution requires Developer ID signing, hardened runtime, notarization, and stapling.";
  } else if (platform === "linux") {
    replacements["{{START_INSTRUCTIONS}}"] =
      "Run ./Open Base Search.sh or ./BaseSearch.";
    replacements["{{CLI_INSTRUCTIONS}}"] =
      "Run ./base-search-cli from a terminal.";
    replacements["{{PLATFORM_NOTE}}"] =
      "The launcher needs a graphical desktop session and a default browser.";
  } else {
    fail(`Unsupported platform: ${platform}`);
  }

  let text = readFileSync(required(args, "template"), "utf8");
  for (const [needle, value] of Object.entries(replacements)) {
    text = text.replaceAll(needle, value);
  }
  if (/\{\{[A-Z_]+\}\}/.test(text)) fail("README template contains unresolved placeholders");
  writeFileSync(required(args, "out"), text.replaceAll("\r\n", "\n"), "utf8");
}

function writeManifest(args) {
  const root = path.resolve(required(args, "root"));
  const platform = required(args, "platform");
  const signing = signingDeclaration(args, platform);
  const epoch = Number(required(args, "epoch"));
  if (!Number.isSafeInteger(epoch) || epoch < 0) fail("Source date epoch must be a positive integer");
  assertSafePackage(root, platform, false, signing);
  const hashes = {};
  for (const entry of walk(root).filter((item) => item.info.isFile())) {
    hashes[entry.relative] = sha256(entry.full);
  }
  const manifest = {
    schema: 1,
    product: "Base Search",
    version: required(args, "version"),
    platform,
    arch: required(args, "arch"),
    git_sha: required(args, "git-sha"),
    source_date_epoch: epoch,
    features: ["browser", "duckdb-olap"],
    launcher_default: "browser",
    legacy_desktop_fallback: true,
    data_policy: {
      default: "per-user-unversioned",
      existing_portable_database: "reuse-in-place",
      sibling_portable_database: "reuse-after-explicit-confirmation",
      automatic_database_move: false,
    },
    signing,
    files_sha256: hashes,
  };
  writeFileSync(
    path.join(root, "release-manifest.json"),
    `${JSON.stringify(manifest, null, 2)}\n`,
    "utf8",
  );
  normalizeTimes(root, epoch);
  assertSafePackage(root, platform, true, signing);
}

function verifyManifest(args) {
  const root = path.resolve(required(args, "root"));
  const platform = required(args, "platform");
  if (!existsSync(root) || !statSync(root).isDirectory()) {
    fail(`Package root is missing: ${root}`);
  }
  const manifestPath = path.join(root, "release-manifest.json");
  if (!existsSync(manifestPath) || !statSync(manifestPath).isFile()) {
    fail("Package release-manifest.json is missing");
  }
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  if (manifest.platform !== platform) fail("Manifest platform does not match package");
  if (!Array.isArray(manifest.features) || !manifest.features.includes("browser") || !manifest.features.includes("duckdb-olap")) {
    fail("Manifest does not declare browser + duckdb-olap");
  }
  if (manifest.launcher_default !== "browser" || manifest.legacy_desktop_fallback !== true) {
    fail("Manifest launcher contract is invalid");
  }
  if (
    manifest.data_policy?.default !== "per-user-unversioned" ||
    manifest.data_policy?.existing_portable_database !== "reuse-in-place" ||
    manifest.data_policy?.sibling_portable_database !== "reuse-after-explicit-confirmation" ||
    manifest.data_policy?.automatic_database_move !== false
  ) {
    fail("Manifest database location and migration policy is invalid");
  }
  const signing = manifest.signing;
  if (
    !signing ||
    !["signed", "unsigned", "not-applicable"].includes(signing.windows_authenticode) ||
    !["signed", "unsigned", "not-applicable"].includes(signing.macos_codesign) ||
    !["stapled", "not-notarized", "not-applicable"].includes(signing.macos_notarization)
  ) {
    fail("Manifest signing declaration is invalid");
  }
  assertSafePackage(root, platform, true, signing);
  if (booleanArg(args, "require-signed")) {
    if (platform === "windows" && signing.windows_authenticode !== "signed") {
      fail("Windows Authenticode signing is required for stable publishing");
    }
    if (platform === "macos" && signing.macos_codesign !== "signed") {
      fail("macOS codesigning is required for stable publishing");
    }
    if (platform === "macos" && signing.macos_notarization !== "stapled") {
      fail("The macOS app must be notarized and stapled for stable publishing");
    }
  }
  const actual = {};
  for (const entry of walk(root).filter(
    (item) => item.info.isFile() && item.relative !== "release-manifest.json",
  )) {
    actual[entry.relative] = sha256(entry.full);
  }
  if (JSON.stringify(actual) !== JSON.stringify(manifest.files_sha256)) {
    fail("Package file hashes do not match release-manifest.json");
  }
  process.stdout.write(`Verified ${platform} package: ${root}\n`);
}

const [command, ...values] = process.argv.slice(2);
const args = parseArgs(values);
mkdirSync(path.dirname(path.resolve(args.out ?? ".")), { recursive: true });

if (command === "render-readme") renderReadme(args);
else if (command === "write-manifest") writeManifest(args);
else if (command === "verify") verifyManifest(args);
else fail("Usage: release-package.mjs render-readme|write-manifest|verify [--key value ...]");
