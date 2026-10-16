#!/usr/bin/env node

"use strict";

const { spawn } = require("node:child_process");

const packages = Object.freeze({
  "darwin-arm64": "@nexus-research-lab/nexus-cua-darwin-arm64",
  "win32-x64": "@nexus-research-lab/nexus-cua-win32-x64",
});

const target = `${process.platform}-${process.arch}`;
const packageName = packages[target];
if (packageName === undefined) {
  process.stderr.write(`nexus-cua does not support ${target}\n`);
  process.exit(1);
}

let executable;
try {
  executable = require.resolve(`${packageName}/binary`);
} catch {
  process.stderr.write(
    `nexus-cua platform payload ${packageName} is missing; reinstall the exact package version\n`,
  );
  process.exit(1);
}

const child = spawn(executable, process.argv.slice(2), {
  stdio: "inherit",
  windowsHide: true,
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => {
    if (!child.killed) {
      child.kill(signal);
    }
  });
}

child.on("error", (error) => {
  process.stderr.write(`nexus-cua failed to start the platform payload: ${error.message}\n`);
  process.exitCode = 1;
});

child.on("exit", (code, signal) => {
  if (signal !== null && process.platform !== "win32") {
    process.kill(process.pid, signal);
    return;
  }
  process.exitCode = code === null ? 1 : code;
});
