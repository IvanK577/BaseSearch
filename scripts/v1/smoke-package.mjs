import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { access, mkdtemp, readFile, writeFile, realpath, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { setTimeout as pause } from "node:timers/promises";

const input = process.argv[2];
if (!input) throw new Error("Pass the Base Search launcher or executable path");
const app = path.resolve(input);
await access(app);
const isBatch = app.endsWith(".cmd");
const isMacLauncher = app.endsWith(".command");
const detached = isBatch || isMacLauncher;
const tempRoot = await realpath(os.tmpdir());
const scratch = await mkdtemp(path.join(tempRoot, "base-search-package-"));

function launch(again = false) {
  const env = { ...process.env, BASE_SEARCH_SMOKE_DIR: scratch };
  let program = app;
  let args = [];
  if (isBatch) {
    program = process.env.ComSpec || "cmd.exe";
    env.BASE_SEARCH_TEST_LAUNCHER = app;
    args = ["/d", "/s", "/v:off", "/c", '""%BASE_SEARCH_TEST_LAUNCHER%""'];
  }
  if (isMacLauncher) {
    args = ["--env", `BASE_SEARCH_SMOKE_DIR=${scratch}`];
    if (again) args.push("-n");
  }
  const child = spawn(program, args, {
    cwd: scratch,
    stdio: "inherit",
    windowsHide: true,
    windowsVerbatimArguments: isBatch,
    env,
  });
  const finished = new Promise((resolve) => {
    child.once("error", (error) => resolve({ error }));
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
  return { child, finished };
}

async function readEventually(file, launch, mayExit = detached) {
  const deadline = Date.now() + 65_000;
  while (Date.now() < deadline) {
    try {
      const contents = await readFile(path.join(scratch, file), "utf8");
      if (contents.length) return contents;
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    const exit = await Promise.race([launch.finished, pause(175, null)]);
    if (exit && (!mayExit || exit.code !== 0)) {
      throw new Error(`Base Search stopped early: ${JSON.stringify(exit)}`);
    }
    if (exit) await pause(175);
  }
  throw new Error(`Base Search did not produce ${file}`);
}

function processId(text) {
  const pid = Number(text);
  assert.ok(Number.isSafeInteger(pid) && pid > 0 && pid !== process.pid, "Invalid desktop process ID");
  return pid;
}

let primary;
let secondary;
let desktopPid;
try {
  await writeFile(path.join(scratch, "smoke.csv"), [
    "Date,Company,Product code,Description,Value,Currency,Weight,Weight unit",
    "2024-01-09,Atlas,001122,Coffee shipment,100,USD,10,kg",
    "2024-03-02,Boreal,990000,Tea shipment,210,EUR,21,kg",
    "",
  ].join("\n"));
  const startup = performance.now();
  primary = launch();
  desktopPid = processId(await readEventually("desktop-pid.txt", primary));
  assert.equal((await readEventually("window-ready.txt", primary)).trim(), "1.0.0");
  console.log("Desktop ready in " + (performance.now() - startup).toFixed(0) + " ms");
  const result = JSON.parse(await readEventually("package-smoke.json", primary));
  assert.equal(result.error, null, result.error ?? undefined);
  assert.equal((await readEventually("window-ready.txt", primary)).trim(), "1.0.0");
  const output = await readFile(path.join(scratch, "coffee.csv"), "utf8");
  assert.match(output, /001122/);
  assert.match(output, /Coffee shipment/);
  assert.doesNotMatch(output, /Tea shipment/);

  secondary = launch(true);
  const exit = await Promise.race([secondary.finished, pause(10_000, null)]);
  assert.ok(exit, "A second launch should forward to the existing desktop window");
  assert.equal(exit.code, 0, JSON.stringify(exit));
  assert.equal(processId(await readEventually("relaunch-pid.txt", secondary, true)), desktopPid);
  assert.doesNotThrow(() => process.kill(desktopPid, 0), "The first desktop process should remain open");
  console.log("Base Search native smoke: preview, import, filtered search, record, summary, CSV export, single instance");
} finally {
  for (const process of [secondary, primary]) {
    if (process && process.child.exitCode === null && process.child.signalCode === null) {
      process.child.kill();
      await Promise.race([process.finished, pause(4_000)]);
    }
  }
  if (detached) {
    const pids = new Set();
    if (desktopPid) pids.add(desktopPid);
    try {
      pids.add(processId(await readFile(path.join(scratch, "desktop-pid.txt"), "utf8")));
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    for (const pid of pids) {
      try {
        process.kill(pid);
      } catch (error) {
        if (error.code !== "ESRCH") throw error;
      }
    }
  }
  const resolved = await realpath(scratch);
  if (path.dirname(resolved) !== tempRoot) throw new Error("Unexpected package-scratch path");
  await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}
