import { copyFile, mkdir, readdir, readFile } from "node:fs/promises";
import path from "node:path";

const bundles = {
  "windows-x64": ["nsis", "-setup.exe", "exe"],
  "macos-arm64": ["dmg", ".dmg", "dmg"],
  "macos-x64": ["dmg", ".dmg", "dmg"],
  "linux-x64": ["deb", ".deb", "deb"],
};
const [platform, profile = "release"] = process.argv.slice(2);
if (!Object.hasOwn(bundles, platform)) throw new Error("Unknown package platform");
if (!["debug", "release"].includes(profile)) throw new Error("Unknown Cargo profile");
const [bundle, suffix, extension] = bundles[platform];
const source = path.join("target", profile, "bundle", bundle);
const files = (await readdir(source)).filter((file) => file.endsWith(suffix));
if (files.length !== 1) throw new Error(`Expected one ${suffix} bundle in ${source}, found ${files.length}`);
const { version } = JSON.parse(await readFile("package.json", "utf8"));
const output = path.join("release_packages", `BaseSearch-${version}-${platform}.${extension}`);
await mkdir(path.dirname(output), { recursive: true });
await copyFile(path.join(source, files[0]), output);
console.log(output.replaceAll("\\", "/"));
