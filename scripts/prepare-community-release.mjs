#!/usr/bin/env node

import {
  copyFileSync,
  mkdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const STABLE_VERSION = /^\d+\.\d+\.\d+$/;
const REPOSITORY = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;

export function compareStableVersions(left, right) {
  if (!STABLE_VERSION.test(left) || !STABLE_VERSION.test(right)) {
    throw new Error(`Stable release versions required, got: ${left}, ${right}`);
  }

  const leftParts = left.split(".").map(BigInt);
  const rightParts = right.split(".").map(BigInt);
  for (let index = 0; index < leftParts.length; index += 1) {
    if (leftParts[index] < rightParts[index]) return -1;
    if (leftParts[index] > rightParts[index]) return 1;
  }
  return 0;
}

function requireNonEmptyFile(path, label) {
  const metadata = statSync(path);
  if (!metadata.isFile()) {
    throw new Error(`${label} is not a file: ${path}`);
  }
  if (metadata.size === 0) {
    throw new Error(`${label} is empty: ${path}`);
  }
}

function requireTextFile(path, label) {
  requireNonEmptyFile(path, label);
  return readFileSync(path, "utf8");
}

export function prepareCommunityRelease({
  version,
  tag,
  repository,
  macDmgPath,
  macUpdaterPath,
  macSignaturePath,
  windowsInstallerPath,
  windowsUpdaterPath,
  windowsSignaturePath,
  notesPath,
  outputDir,
  pubDate = new Date().toISOString(),
}) {
  if (!STABLE_VERSION.test(version)) {
    throw new Error(`Stable release version required, got: ${version}`);
  }
  if (tag !== `v${version}`) {
    throw new Error(`Tag ${tag} does not match version ${version}`);
  }
  if (!REPOSITORY.test(repository)) {
    throw new Error(`Invalid GitHub repository: ${repository}`);
  }
  if (Number.isNaN(Date.parse(pubDate))) {
    throw new Error(`Invalid publication date: ${pubDate}`);
  }

  requireNonEmptyFile(macDmgPath, "macOS DMG");
  requireNonEmptyFile(macUpdaterPath, "macOS updater archive");
  const macSignature = requireTextFile(
    macSignaturePath,
    "macOS updater signature",
  ).trim();
  requireNonEmptyFile(windowsInstallerPath, "Windows NSIS installer");
  requireNonEmptyFile(windowsUpdaterPath, "Windows updater archive");
  const windowsSignature = requireTextFile(
    windowsSignaturePath,
    "Windows updater signature",
  ).trim();
  const notes = requireTextFile(notesPath, "Release notes").trim();
  if (!macSignature) {
    throw new Error(`macOS updater signature is empty: ${macSignaturePath}`);
  }
  if (!windowsSignature) {
    throw new Error(`Windows updater signature is empty: ${windowsSignaturePath}`);
  }
  if (!notes) {
    throw new Error(`Release notes are empty: ${notesPath}`);
  }

  const stagingDir = resolve(outputDir);
  const assetsDir = join(stagingDir, "assets");
  mkdirSync(assetsDir, { recursive: true });

  const macDmgName = `codexbox_${version}_universal.dmg`;
  const macUpdaterName = "codexbox_universal.app.tar.gz";
  const macSignatureName = `${macUpdaterName}.sig`;
  const windowsInstallerName = `codexbox_${version}_x64-setup.exe`;
  const windowsUpdaterName = "codexbox_x64-setup.nsis.zip";
  const windowsSignatureName = `${windowsUpdaterName}.sig`;

  copyFileSync(macDmgPath, join(assetsDir, macDmgName));
  copyFileSync(macUpdaterPath, join(assetsDir, macUpdaterName));
  copyFileSync(macSignaturePath, join(assetsDir, macSignatureName));
  copyFileSync(windowsInstallerPath, join(assetsDir, windowsInstallerName));
  copyFileSync(windowsUpdaterPath, join(assetsDir, windowsUpdaterName));
  copyFileSync(windowsSignaturePath, join(assetsDir, windowsSignatureName));

  const downloadRoot = `https://github.com/${repository}/releases/download/${tag}`;
  const macPlatform = {
    signature: macSignature,
    url: `${downloadRoot}/${macUpdaterName}`,
  };
  const windowsPlatform = {
    signature: windowsSignature,
    url: `${downloadRoot}/${windowsUpdaterName}`,
  };
  const manifest = {
    version,
    notes,
    pub_date: new Date(pubDate).toISOString(),
    platforms: {
      "darwin-aarch64": macPlatform,
      "darwin-x86_64": macPlatform,
      "darwin-aarch64-app": macPlatform,
      "darwin-x86_64-app": macPlatform,
      "windows-x86_64": windowsPlatform,
    },
  };

  writeFileSync(join(assetsDir, "latest.json"), `${JSON.stringify(manifest)}\n`);
  writeFileSync(join(stagingDir, "release-body.md"), `${notes}\n`);

  return {
    assetsDir,
    assetNames: [
      macDmgName,
      macUpdaterName,
      macSignatureName,
      windowsInstallerName,
      windowsUpdaterName,
      windowsSignatureName,
      "latest.json",
    ],
    manifest,
  };
}

function main(argv) {
  if (argv.length !== 11) {
    throw new Error(
      "Usage: prepare-community-release.mjs <version> <tag> <owner/repo> <mac.dmg> <mac-updater.tar.gz> <mac-signature> <windows-installer.exe> <windows-updater.nsis.zip> <windows-signature> <notes.md> <output-dir>",
    );
  }

  const [
    version,
    tag,
    repository,
    macDmgPath,
    macUpdaterPath,
    macSignaturePath,
    windowsInstallerPath,
    windowsUpdaterPath,
    windowsSignaturePath,
    notesPath,
    outputDir,
  ] = argv;
  const result = prepareCommunityRelease({
    version,
    tag,
    repository,
    macDmgPath,
    macUpdaterPath,
    macSignaturePath,
    windowsInstallerPath,
    windowsUpdaterPath,
    windowsSignaturePath,
    notesPath,
    outputDir,
  });
  process.stdout.write(`${result.assetNames.join("\n")}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  }
}
