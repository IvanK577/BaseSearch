import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, writeFile, realpath, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { setTimeout as pause } from "node:timers/promises";

const input = process.argv[2];
if (!input) throw new Error("Pass the installed Base Search executable path");
const app = path.resolve(input);
const tempRoot = await realpath(os.tmpdir());
const scratch = await mkdtemp(path.join(tempRoot, "base-search-package-"));

function proc() {
  const child = spawn(app, [], {
    cwd: scratch,
    stdio: "inherit",
    windowsHide: true,
    env: { ...process.env, BASE_SEARCH_SMOKE_DIR: scratch },
  });
  const finished = new Promise((resolve) => {
    child.once("error", (error) => resolve({ error }));
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
  return { child, finished };
}

async function readEventually(file, process) {
  const deadline = Date.now() + 65_000;
  while (Date.now() < deadline) {
    try {
      return await readFile(path.join(scratch, file), "utf8");
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    const exit = await Promise.race([process.finished, pause(175, null)]);
    if (exit) throw new Error(`The installed app stopped early: ${JSON.stringify(exit)}`);
  }
  throw new Error(`The installed app did not produce ${file}`);
}

let primary;
let secondary;
try {
  await writeFile(path.join(scratch, "smoke.csv"), [
    "Date,Company,Product code,Description,Value,Currency,Weight,Weight unit",
    "2024-01-09,Atlas,001122,Coffee shipment,100,USD,10,kg",
    "2024-03-02,Boreal,990000,Tea shipment,210,EUR,21,kg",
    "",
  ].join("\n"));
  const startup = performance.now();
  primary = proc();
  assert.equal((await readEventually("window-ready.txt", primary)).trim(), "1.0.0");
  console.log("Desktop ready in " + (performance.now() - startup).toFixed(0) + " ms");
  const result = JSON.parse(await readEventually("package-smoke.json", primary));
  assert.equal(result.error, null, result.error ?? undefined);
  assert.equal((await readEventually("window-ready.txt", primary)).trim(), "1.0.0");
  const output = await readFile(path.join(scratch, "coffee.csv"), "utf8");
  assert.match(output, /001122/);
  assert.match(output, /Coffee shipment/);
  assert.doesNotMatch(output, /Tea shipment/);

  secondary = proc();
  const exit = await Promise.race([secondary.finished, pause(10_000, null)]);
  assert.ok(exit, "A second launch should forward to the existing desktop window");
  assert.equal(exit.code, 0, JSON.stringify(exit));
  assert.equal(primary.child.exitCode, null, "The first window should remain open");
  console.log("Base Search native smoke: preview, import, filtered search, record, summary, CSV export, single instance");
} finally {
  for (const process of [secondary, primary]) {
    if (process && process.child.exitCode === null && process.child.signalCode === null) {
      process.child.kill();
      await Promise.race([process.finished, pause(4_000)]);
    }
  }
  const resolved = await realpath(scratch);
  if (path.dirname(resolved) !== tempRoot) throw new Error("Unexpected package-scratch path");
  await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}
