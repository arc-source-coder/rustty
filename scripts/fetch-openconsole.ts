#!/usr/bin/env bun
//! Downloads OpenConsole.exe from the Windows Terminal NuGet package
//! and copies it directly to the Cargo output directory.
//!
//! Usage: bun scripts/fetch-openconsole.ts <arch> <out-dir>
//!   arch:    x64 | x86 | arm64
//!   out-dir: target/{profile}/ — the directory where the final binary lives
//!
//! No-ops if the marker file in <out-dir> already records this version + arch.

import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { mkdirSync, existsSync, readFileSync, writeFileSync, copyFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";

const CONPTY_VERSION = "1.25.260303002-preview";
const RELEASE_TAG = "v1.25.622.0";
const NUPKG_URL = `https://github.com/microsoft/terminal/releases/download/${RELEASE_TAG}/Microsoft.Windows.Console.ConPTY.${CONPTY_VERSION}.nupkg`;

const MARKER_FILENAME = ".openconsole-version";

const [arch, outDir] = process.argv.slice(2);

if (!arch || !outDir) {
    console.error("Usage: fetch-openconsole.ts <arch:x64|x86|arm64> <out-dir>");
    process.exit(1);
}

if (!["x64", "x86", "arm64"].includes(arch)) {
    console.error(`Unknown arch: ${arch}. Expected x64, x86, or arm64.`);
    process.exit(1);
}

const markerPath = join(outDir, MARKER_FILENAME);
const expectedMarker = `${arch}:${CONPTY_VERSION}`;

if (existsSync(markerPath)) {
    try {
        const recorded = readFileSync(markerPath, "utf8").trim();
        if (recorded === expectedMarker) {
            const exeOk = existsSync(join(outDir, "OpenConsole.exe"));
            if (exeOk) {
                console.log(`[fetch-openconsole] Already at ${CONPTY_VERSION} (${arch}), skipping.`);
                process.exit(0);
            }
        }
    } catch {
        // Marker unreadable — fall through to download.
    }
}

console.log(`[fetch-openconsole] Downloading OpenConsole ${CONPTY_VERSION} (${arch})...`);

const resp = await fetch(NUPKG_URL);
if (!resp.ok) {
    console.error(`Download failed: HTTP ${resp.status} from ${NUPKG_URL}`);
    process.exit(1);
}

const nupkgBytes = Buffer.from(await resp.arrayBuffer());

const tmpZipPath = join(tmpdir(), `openconsole-${CONPTY_VERSION}.zip`);
writeFileSync(tmpZipPath, nupkgBytes);

mkdirSync(outDir, { recursive: true });

const extractDir = join(tmpdir(), `openconsole-extract-${CONPTY_VERSION}`);
mkdirSync(extractDir, { recursive: true });

function run(cmd: string, args: string[]): void {
    const result = spawnSync(cmd, args, { stdio: "inherit" });
    if (result.error) {
        console.error(`[fetch-openconsole] Failed to run ${cmd}: ${result.error.message}`);
        process.exit(1);
    }
    if (result.status !== 0) {
        console.error(`[fetch-openconsole] ${cmd} exited with code ${result.status}`);
        process.exit(1);
    }
}

if (process.platform === "win32") {
    const psCmd = `Expand-Archive -Path '${tmpZipPath.replace(/'/g, "''")}' -DestinationPath '${extractDir.replace(/'/g, "''")}' -Force`;
    run("powershell", ["-NoProfile", "-Command", psCmd]);
} else {
    run("unzip", ["-o", tmpZipPath, "-d", extractDir]);
}

const exeSrc = join(extractDir, "build", "native", "runtimes", arch, "OpenConsole.exe");
const exeDest = join(outDir, "OpenConsole.exe");
copyFileSync(exeSrc, exeDest);

rmSync(extractDir, { recursive: true, force: true });
rmSync(tmpZipPath, { force: true });

const depsDir = join(outDir, "deps");
if (existsSync(depsDir)) {
    copyFileSync(exeDest, join(depsDir, "OpenConsole.exe"));
}

writeFileSync(markerPath, expectedMarker + "\n");
console.log(`[fetch-openconsole] OpenConsole.exe → ${outDir}`);
