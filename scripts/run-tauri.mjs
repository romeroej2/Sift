import { spawn } from "node:child_process";
import os from "node:os";
import path from "node:path";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const projectRoot = path.resolve(__dirname, "..");
const tauriCliEntrypoint = path.join(
  projectRoot,
  "node_modules",
  "@tauri-apps",
  "cli",
  "tauri.js"
);

function unique(values) {
  return [...new Set(values.filter(Boolean))];
}

function cargoExecutableName() {
  return process.platform === "win32" ? "cargo.exe" : "cargo";
}

function rustcExecutableName() {
  return process.platform === "win32" ? "rustc.exe" : "rustc";
}

function hasRustToolchain(binDir) {
  return (
    existsSync(path.join(binDir, cargoExecutableName())) &&
    existsSync(path.join(binDir, rustcExecutableName()))
  );
}

function commonRustBinDirs() {
  const home = os.homedir();
  const values = [
    process.env.CARGO_HOME ? path.join(process.env.CARGO_HOME, "bin") : null,
    path.join(home, ".cargo", "bin"),
    "/opt/homebrew/opt/rustup/bin",
    "/usr/local/opt/rustup/bin"
  ];

  if (process.platform === "win32") {
    values.push(path.join(process.env.USERPROFILE ?? home, ".cargo", "bin"));
  }

  return unique(values);
}

function buildEnv() {
  const pathEntries = (process.env.PATH ?? "").split(path.delimiter);
  const rustBinDirs = commonRustBinDirs().filter(hasRustToolchain);
  const nextPathEntries = unique([...rustBinDirs, ...pathEntries]);
  const cargoBin = rustBinDirs[0];

  return {
    ...process.env,
    PATH: nextPathEntries.join(path.delimiter),
    ...(cargoBin
      ? {
          CARGO: path.join(cargoBin, cargoExecutableName()),
          RUSTC: path.join(cargoBin, rustcExecutableName())
        }
      : {})
  };
}

function assertTauriCliInstalled() {
  if (!existsSync(tauriCliEntrypoint)) {
    console.error("SIFT could not find the local Tauri CLI. Run `npm install` first.");
    process.exit(1);
  }
}

function assertCargoAvailable(env) {
  const cargoInPath = (env.PATH ?? "")
    .split(path.delimiter)
    .some((entry) => existsSync(path.join(entry, cargoExecutableName())));

  if (!cargoInPath) {
    console.error(
      [
        "SIFT could not find `cargo` on the current PATH.",
        "Install Rust with rustup, or expose your Rust bin directory before running Tauri.",
        "Expected one of: ~/.cargo/bin or /opt/homebrew/opt/rustup/bin."
      ].join("\n")
    );
    process.exit(1);
  }
}

assertTauriCliInstalled();

const env = buildEnv();
assertCargoAvailable(env);

const child = spawn(process.execPath, [tauriCliEntrypoint, ...process.argv.slice(2)], {
  cwd: projectRoot,
  env,
  stdio: "inherit"
});

child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }

  process.exit(code ?? 0);
});

child.on("error", (error) => {
  console.error(`Failed to launch the Tauri CLI: ${error.message}`);
  process.exit(1);
});
