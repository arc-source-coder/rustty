#!/usr/bin/env bun
//! Downloads conpty.dll + OpenConsole.exe from the Windows Terminal NuGet package
//! and copies them directly to the Cargo output directory.
//!
//! Usage: bun scripts/fetch-conpty.ts <arch> <out-dir>
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

const MARKER_FILENAME = ".conpty-version";

const [arch, outDir] = process.argv.slice(2);

if (!arch || !outDir) {
    console.error("Usage: fetch-conpty.ts <arch:x64|x86|arm64> <out-dir>");
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
            // Already up to date — confirm both files exist before skipping.
            const dllOk = existsSync(join(outDir, "conpty.dll"));
            const exeOk = existsSync(join(outDir, "OpenConsole.exe"));
            if (dllOk && exeOk) {
                console.log(`[fetch-conpty] Already at ${CONPTY_VERSION} (${arch}), skipping.`);
                process.exit(0);
            }
        }
    } catch {
        // Marker unreadable — fall through to download.
    }
}

console.log(`[fetch-conpty] Downloading ConPTY ${CONPTY_VERSION} (${arch})...`);

const resp = await fetch(NUPKG_URL);
if (!resp.ok) {
    console.error(`Download failed: HTTP ${resp.status} from ${NUPKG_URL}`);
    process.exit(1);
}

const nupkgBytes = Buffer.from(await resp.arrayBuffer());

// Write to a temp file. Rename to .zip because PowerShell's Expand-Archive
// rejects non-.zip extensions, even though .nupkg is a standard zip archive.
const tmpZipPath = join(tmpdir(), `conpty-${CONPTY_VERSION}.zip`);
writeFileSync(tmpZipPath, nupkgBytes);

mkdirSync(outDir, { recursive: true });

// Extract entire archive to temp dir, then copy the two files we need.
const extractDir = join(tmpdir(), `conpty-extract-${CONPTY_VERSION}`);
mkdirSync(extractDir, { recursive: true });

function run(cmd: string, args: string[]): void {
    const result = spawnSync(cmd, args, { stdio: "inherit" });
    if (result.error) {
        console.error(`[fetch-conpty] Failed to run ${cmd}: ${result.error.message}`);
        process.exit(1);
    }
    if (result.status !== 0) {
        console.error(`[fetch-conpty] ${cmd} exited with code ${result.status}`);
        process.exit(1);
    }
}

// .nupkg is a zip archive. Extract with platform-specific tooling.
if (process.platform === "win32") {
    const psCmd = `Expand-Archive -Path '${tmpZipPath.replace(/'/g, "''")}' -DestinationPath '${extractDir.replace(/'/g, "''")}' -Force`;
    run("powershell", ["-NoProfile", "-Command", psCmd]);
} else {
    run("unzip", ["-o", tmpZipPath, "-d", extractDir]);
}

const dllSrc = join(extractDir, "runtimes", `win-${arch}`, "native", "conpty.dll");
const exeSrc = join(extractDir, "build", "native", "runtimes", arch, "OpenConsole.exe");

const dllDest = join(outDir, "conpty.dll");
const exeDest = join(outDir, "OpenConsole.exe");

copyFileSync(dllSrc, dllDest);
copyFileSync(exeSrc, exeDest);

// Clean up temp dirs.
rmSync(extractDir, { recursive: true, force: true });
rmSync(tmpZipPath, { force: true });

// Also copy into deps/ if it exists (used by `cargo test`).
const depsDir = join(outDir, "deps");
if (existsSync(depsDir)) {
    copyFileSync(dllDest, join(depsDir, "conpty.dll"));
    copyFileSync(exeDest, join(depsDir, "OpenConsole.exe"));
}

writeFileSync(markerPath, expectedMarker + "\n");
console.log(`[fetch-conpty] conpty.dll + OpenConsole.exe → ${outDir}`);
