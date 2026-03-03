#!/usr/bin/env bun
//! Downloads conpty.dll + OpenConsole.exe from the Windows Terminal NuGet package
//! and copies them directly to the Cargo output directory.
//!
//! Usage: bun scripts/fetch-conpty.ts <arch> <out-dir>
//!   arch:    x64 | x86 | arm64
//!   out-dir: target/{profile}/ — the directory where the final binary lives
//!
//! No-ops if the marker file in <out-dir> already records this version + arch.

import { join } from "node:path";
import { mkdirSync, existsSync, readFileSync, writeFileSync, copyFileSync } from "node:fs";
import { tmpdir } from "node:os";

const CONPTY_VERSION = "1.23.260121001";
const RELEASE_TAG = "v1.23.20211.0";
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

// Write to a temp file so unzip can work on it.
const tmpPath = join(tmpdir(), `conpty-${CONPTY_VERSION}.nupkg`);
writeFileSync(tmpPath, nupkgBytes);

// The nupkg is a zip. Extract the two files we need via `unzip -p`.
mkdirSync(outDir, { recursive: true });

const dllZipPath = `runtimes/win-${arch}/native/conpty.dll`;
const exeZipPath = `build/native/runtimes/${arch}/OpenConsole.exe`;

function extractEntry(zipFile: string, entryPath: string, destPath: string): void {
    const proc = Bun.spawnSync(["unzip", "-p", zipFile, entryPath], {
        stdout: "pipe",
        stderr: "pipe",
    });
    if (proc.exitCode !== 0) {
        const msg = proc.stderr ? new TextDecoder().decode(proc.stderr) : "(no stderr)";
        console.error(`[fetch-conpty] unzip failed for ${entryPath}: ${msg}`);
        process.exit(1);
    }
    writeFileSync(destPath, proc.stdout!);
}

const dllDest = join(outDir, "conpty.dll");
const exeDest = join(outDir, "OpenConsole.exe");

extractEntry(tmpPath, dllZipPath, dllDest);
extractEntry(tmpPath, exeZipPath, exeDest);

// Also copy into deps/ if it exists (used by `cargo test`).
const depsDir = join(outDir, "deps");
if (existsSync(depsDir)) {
    copyFileSync(dllDest, join(depsDir, "conpty.dll"));
    copyFileSync(exeDest, join(depsDir, "OpenConsole.exe"));
}

writeFileSync(markerPath, expectedMarker + "\n");
console.log(`[fetch-conpty] conpty.dll + OpenConsole.exe → ${outDir}`);
