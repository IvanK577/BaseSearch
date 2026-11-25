import { execFileSync } from "node:child_process";
import { chmod, copyFile, mkdir, mkdtemp, readFile, realpath, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";

const platforms = {
  "windows-x64": ["win32", "x64", "cmd", "zip"],
  "macos-arm64": ["darwin", "arm64", "command", "zip"],
  "macos-x64": ["darwin", "x64", "command", "zip"],
  "linux-x64": ["linux", "x64", "sh", "tar.gz"],
};
const [platform, profile = "release"] = process.argv.slice(2);
if (!Object.hasOwn(platforms, platform)) throw new Error("Unknown portable package platform");
if (!["debug", "release"].includes(profile)) throw new Error("Unknown Cargo profile");
const [system, arch, launcherExtension, archiveExtension] = platforms[platform];
if (process.platform !== system || process.arch !== arch) {
  throw new Error(`Build the ${platform} archive on its target system and architecture`);
}

const { version } = JSON.parse(await readFile("package.json", "utf8"));
const packageName = `BaseSearch-${version}-${platform}-portable`;
const build = path.join("target", profile);
const destination = path.join("release_packages", `${packageName}.${archiveExtension}`);
const tempRoot = await realpath(os.tmpdir());
const scratch = await mkdtemp(path.join(tempRoot, "base-search-portable-"));
const folder = path.join(scratch, packageName);
const archive = path.join(scratch, path.basename(destination));
const launcher = `Start Base Search.${launcherExtension}`;

function run(program, args) {
  execFileSync(program, args, { stdio: "inherit", windowsHide: true });
}

try {
  await mkdir(folder);
  await copyFile(path.join("scripts", "v1", "launchers", launcher), path.join(folder, launcher));
  await copyFile("README.md", path.join(folder, "README.md"));
  await copyFile("README.uk.md", path.join(folder, "README.uk.md"));
  await copyFile("LICENSE", path.join(folder, "LICENSE"));
  if (system !== "win32") await chmod(path.join(folder, launcher), 0o755);

  if (system === "darwin") {
    const application = "Base Search.app";
    const bundle = path.join(build, "bundle", "macos", application);
    run("/usr/bin/ditto", [bundle, path.join(folder, application)]);
    run("/usr/bin/ditto", ["-c", "-k", "--sequesterRsrc", "--keepParent", folder, archive]);
  } else {
    const binary = system === "win32" ? "base-search.exe" : "base-search";
    const program = path.join(folder, "app", binary);
    await mkdir(path.dirname(program));
    await copyFile(path.join(build, binary), program);
    if (system === "win32") {
      run("tar.exe", ["-a", "-c", "-f", archive, "-C", scratch, packageName]);
    } else {
      await chmod(program, 0o755);
      run("tar", ["-czf", archive, "-C", scratch, packageName]);
    }
  }
  await mkdir(path.dirname(destination), { recursive: true });
  await copyFile(archive, destination);
} finally {
  const resolved = await realpath(scratch);
  if (path.dirname(resolved) !== tempRoot) throw new Error("Unexpected portable staging path");
  await rm(resolved, { recursive: true, force: true });
}
console.log(destination.replaceAll("\\", "/"));
