import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  compareStableVersions,
  prepareCommunityRelease,
} from "./prepare-community-release.mjs";

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), "headroom-release-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));

  const files = {
    macDmgPath: join(root, "input.dmg"),
    macUpdaterPath: join(root, "input.app.tar.gz"),
    macSignaturePath: join(root, "input.app.tar.gz.sig"),
    windowsInstallerPath: join(root, "input-setup.exe"),
    windowsUpdaterPath: join(root, "input-setup.nsis.zip"),
    windowsSignaturePath: join(root, "input-setup.nsis.zip.sig"),
    notesPath: join(root, "notes.md"),
    outputDir: join(root, "staging"),
  };
  writeFileSync(files.macDmgPath, "dmg");
  writeFileSync(files.macUpdaterPath, "mac-updater");
  writeFileSync(files.macSignaturePath, "mac-signature");
  writeFileSync(files.windowsInstallerPath, "windows-installer");
  writeFileSync(files.windowsUpdaterPath, "windows-updater");
  writeFileSync(files.windowsSignaturePath, "windows-signature");
  writeFileSync(files.notesPath, "Fixed updater retry.\n");
  return files;
}

test("compares stable release versions without lexical or integer overflow errors", () => {
  assert.equal(compareStableVersions("1.10.0", "1.9.99"), 1);
  assert.equal(compareStableVersions("1.2.3", "1.2.3"), 0);
  assert.equal(compareStableVersions("1.2.2", "1.2.3"), -1);
  assert.equal(
    compareStableVersions("999999999999999999999.0.0", "2.0.0"),
    1,
  );
  assert.throws(
    () => compareStableVersions("1.2.3-rc.1", "1.2.2"),
    /Stable release versions required/,
  );
});

test("stages macOS and Windows assets with one updater manifest", (t) => {
  const result = prepareCommunityRelease({
    version: "1.2.3",
    tag: "v1.2.3",
      repository: "OneBigMoon/Headroom-desktop",
    pubDate: "2026-08-30T00:00:00.000Z",
    ...fixture(t),
  });

  assert.deepEqual(result.assetNames, [
    "Headroom.Local.Community_1.2.3_universal.dmg",
    "Headroom.Local.Community_universal.app.tar.gz",
    "Headroom.Local.Community_universal.app.tar.gz.sig",
    "Headroom.Local.Community_1.2.3_x64-setup.exe",
    "Headroom.Local.Community_x64-setup.nsis.zip",
    "Headroom.Local.Community_x64-setup.nsis.zip.sig",
    "latest.json",
  ]);
  assert.equal(result.manifest.version, "1.2.3");
  assert.equal(result.manifest.notes, "Fixed updater retry.");
  assert.deepEqual(Object.keys(result.manifest.platforms), [
    "darwin-aarch64",
    "darwin-x86_64",
    "darwin-aarch64-app",
    "darwin-x86_64-app",
    "windows-x86_64",
  ]);
  assert.equal(
    result.manifest.platforms["darwin-aarch64"].url,
    "https://github.com/OneBigMoon/Headroom-desktop/releases/download/v1.2.3/Headroom.Local.Community_universal.app.tar.gz",
  );
  assert.equal(
    result.manifest.platforms["darwin-aarch64"].signature,
    "mac-signature",
  );
  assert.equal(
    result.manifest.platforms["windows-x86_64"].url,
    "https://github.com/OneBigMoon/Headroom-desktop/releases/download/v1.2.3/Headroom.Local.Community_x64-setup.nsis.zip",
  );
  assert.equal(
    result.manifest.platforms["windows-x86_64"].signature,
    "windows-signature",
  );
  assert.equal(
    JSON.parse(readFileSync(join(result.assetsDir, "latest.json"), "utf8")).version,
    "1.2.3",
  );
});

test("rejects prerelease versions and mismatched tags", (t) => {
  const files = fixture(t);
  assert.throws(
    () =>
      prepareCommunityRelease({
        version: "1.2.3-rc.1",
        tag: "v1.2.3-rc.1",
      repository: "OneBigMoon/Headroom-desktop",
        ...files,
      }),
    /Stable release version required/,
  );
  assert.throws(
    () =>
      prepareCommunityRelease({
        version: "1.2.3",
        tag: "v1.2.4",
      repository: "OneBigMoon/Headroom-desktop",
        ...files,
      }),
    /does not match version/,
  );
});

test("rejects an empty Windows updater archive", (t) => {
  const files = fixture(t);
  writeFileSync(files.windowsUpdaterPath, "");

  assert.throws(
    () =>
      prepareCommunityRelease({
        version: "1.2.3",
        tag: "v1.2.3",
      repository: "OneBigMoon/Headroom-desktop",
        ...files,
      }),
    /Windows updater archive/,
  );
});

test("rejects an empty Windows installer using file metadata", (t) => {
  const files = fixture(t);
  writeFileSync(files.windowsInstallerPath, "");

  assert.throws(
    () =>
      prepareCommunityRelease({
        version: "1.2.3",
        tag: "v1.2.3",
      repository: "OneBigMoon/Headroom-desktop",
        ...files,
      }),
    /Windows NSIS installer is empty/,
  );
});

test("release workflow rejects versions that are not newer than GitHub latest", () => {
  const workflow = readFileSync(
    new URL("../.github/workflows/release-community-macos.yml", import.meta.url),
    "utf8",
  );

  assert.match(
    workflow,
    /gh api "repos\/\$\{GITHUB_REPOSITORY\}\/releases\/latest" --jq '\.tag_name'/,
  );
  assert.match(workflow, /compareStableVersions\(candidate, latest\) <= 0/);
  assert.match(workflow, /GH_TOKEN: \$\{\{ github\.token \}\}/);
});

test("release workflow resumes drafts without replacing existing assets", () => {
  const workflow = readFileSync(
    new URL("../.github/workflows/release-community-macos.yml", import.meta.url),
    "utf8",
  );
  const publishStep = workflow.match(
    /- name: Create draft, verify uploaded digests, and publish([\s\S]*?)- name: Verify immutable public release downloads/,
  );

  assert.ok(publishStep);
  assert.match(publishStep[1], /gh api --paginate --slurp "\$\{releases_api\}"/);
  assert.match(
    publishStep[1],
    /gh release create "\$\{tag\}"[\s\S]*?for _ in \{1\.\.15\}[\s\S]*?Created draft \$\{tag\} did not become uniquely visible within 30 seconds/,
  );
  assert.match(
    publishStep[1],
    /release_api="repos\/\$\{GITHUB_REPOSITORY\}\/releases\/\$\{release_id\}"/,
  );
  assert.match(publishStep[1], /Unexpected asset .* refusing no-replace recovery/);
  assert.match(publishStep[1], /Existing asset .* has digest/);
  assert.match(publishStep[1], /--data-binary "@\$\{asset\}"/);
  assert.match(publishStep[1], /uploads\.github\.com/);
  assert.match(
    publishStep[1],
    /gh api --method PATCH "\$\{release_api\}" -F draft=false/,
  );
  assert.doesNotMatch(
    publishStep[1],
    /gh api --method PATCH "\$\{release_api\}" -F draft=true/,
  );
  assert.match(publishStep[1], /immutable-releases/);
  assert.match(publishStep[1], /X-GitHub-Api-Version: 2026-03-10/);
  assert.match(publishStep[1], /for _ in \{1\.\.30\}/);
  assert.match(
    publishStep[1],
    /Release did not become immutable within 60 seconds/,
  );
  assert.doesNotMatch(publishStep[1], /releases\/tags\/\$\{tag\}/);
  assert.doesNotMatch(publishStep[1], /--clobber/);
  assert.doesNotMatch(
    publishStep[1],
    /gh release create "\$\{tag\}" "\$\{assets_dir\}"\/\*/,
  );
});

test("release workflow builds both platforms and publishes one seven-asset release", () => {
  const workflow = readFileSync(
    new URL("../.github/workflows/release-community-macos.yml", import.meta.url),
    "utf8",
  );

  assert.match(workflow, /build_macos:[\s\S]*?runs-on: macos-14/);
  assert.match(workflow, /build_windows:[\s\S]*?runs-on: windows-latest/);
  assert.match(workflow, /--bundles nsis/);
  assert.match(workflow, /--target x86_64-pc-windows-msvc/);
  assert.match(
    workflow,
    /target\/x86_64-pc-windows-msvc\/release\/bundle\/nsis/,
  );
  assert.match(workflow, /createUpdaterArtifacts[^\n]*v1Compatible/);
  assert.match(workflow, /aggregate:[\s\S]*?needs: \[validate, build_macos, build_windows\]/);
  assert.match(workflow, /publish:[\s\S]*?needs: aggregate/);
  assert.match(workflow, /"windows-x86_64"/);
  assert.match(workflow, /Headroom\.Local\.Community_x64-setup\.nsis\.zip/);
  assert.match(workflow, /\.assets \| length' <<<"\$\{release_json\}"\)" == "7"/);
  assert.equal((workflow.match(/contents: write/g) ?? []).length, 1);
});

test("Windows NSIS bundle uses the branded installer assets", () => {
  const tauriConfig = JSON.parse(
    readFileSync(
      new URL("../src-tauri/tauri.conf.json", import.meta.url),
      "utf8",
    ),
  );
  const nsis = tauriConfig.bundle.windows.nsis;

  assert.equal(nsis.headerImage, "installer/header.bmp");
  assert.equal(nsis.sidebarImage, "installer/sidebar.bmp");
  assert.equal(nsis.installerIcon, "icons/icon.ico");
  assert.equal(nsis.uninstallerIcon, "icons/icon.ico");
});

test("documentation limits the Windows preview promise to Windows 11 x64", () => {
  const readme = readFileSync(new URL("../README.md", import.meta.url), "utf8");
  const releaseDocs = readFileSync(
    new URL("../docs/macos-release.md", import.meta.url),
    "utf8",
  );

  assert.match(readme, /Windows 11 x64 preview/);
  assert.doesNotMatch(readme, /Windows 10 or newer/);
  assert.match(readme, /SmartScreen/);
  assert.match(releaseDocs, /Windows 11 x64/);
  assert.match(releaseDocs, /Windows 10/);
  assert.match(releaseDocs, /SmartScreen/);
  assert.match(releaseDocs, /Authenticode/);
});
