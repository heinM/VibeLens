# Building and shipping a VibeLens release

How to turn the repo into installers other people can run: what it costs, what to sign, how to automate it, and what will bite you.

**Short version**
1. Work lands on `develop`, is tried on `test`, and is promoted to `release` (§0).
2. On `release`: bump the version in **one place** (§3) and run the manual test plan (kept outside this repository).
3. Build the installers into a **draft** GitHub release: on a Mac with `VibeLens/scripts/release-local.sh` (macOS, optionally Linux), or on runners with `gh workflow run release.yml --ref vX.Y.Z` (macOS, Windows, Linux). Pushing a tag does not start a build.
4. Open a pull request from `release` into `main`. This is the only thing that starts CI. Merge it when CI is green: `main` is what is in production.
5. Runner builds publish release candidate tags as prereleases automatically once all platforms finish; stable tags, and anything built locally, stay drafts until you publish them (§9).

Builds are **unsigned until you add the signing secrets** (§5, §6). They work, but people get scary warnings.

---

## 0. Branches and what runs when

| Branch | What it is | How code gets there | CI |
|---|---|---|---|
| `develop` | Day-to-day work: features and fixes. Dependabot opens its pull requests here. | Feature branches and pull requests | None automatically |
| `test` | What is being tried by hand: a build from here is what testers run. | Fast-forward from `develop` when a set of changes is ready to try | None automatically |
| `release` | The release candidate. The version is bumped and the tag `vX.Y.Z` or `vX.Y.Z-N` is cut here. | Fast-forward from `test` | None automatically |
| `main` | The deployed production version. Nothing is committed to it directly. | Pull request from `release` | **`ci.yml` runs on the pull request** |

- CI runs only on a pull request into `main`, to keep runner minutes down. Everywhere else, run the checks on your machine before promoting (README, "Development": `cargo fmt`, `cargo clippy`, `cargo test`, `npm run typecheck`, `npm test`), or start CI by hand for one branch: `gh workflow run ci.yml --ref develop`.
- Promote with fast-forwards so the three working branches never diverge: `git push origin develop:test`, then `git push origin test:release`.
- A fix found on `test` or `release` is made on `develop` and promoted again. An urgent production fix branches from `main`, goes back through a pull request into `main` (CI runs), and is then merged into `develop`.
- The release workflow is started by hand on a tag (§9); it never runs on a push.
- Repository rulesets enforce this: `main` only changes through a pull request merged with a merge commit after CI passes; none of the four branches can be deleted or force-pushed; `v*` tags cannot be moved or deleted. Do not turn on "Automatically delete head branches": the head of a release pull request is `release`.

---

## 1. What you ship, per platform

| Platform | Artifacts (Tauri v2) | Signing that matters | Without signing, users see |
|---|---|---|---|
| macOS (Apple Silicon + Intel) | `VibeLens_X.Y.Z_universal.dmg` (and the `.app` inside) | Developer ID Application + notarisation | "Apple could not verify 'VibeLens' is free of malware". On macOS 15+ the right-click-Open bypass is gone; users must go to System Settings → Privacy & Security → **Open Anyway**. |
| Windows 10/11 x64 | `VibeLens_X.Y.Z_x64-setup.exe` (NSIS), `VibeLens_X.Y.Z_x64_en-US.msi` | Authenticode (OV or EV) | SmartScreen "Windows protected your PC" → More info → Run anyway. **Windows 11 Smart App Control blocks unsigned apps outright**, and users can only turn SAC off entirely. |
| Linux x64 | Stable: `.AppImage`, `.deb`, `.rpm`; prerelease: `.AppImage`, `.deb` | none expected; publish SHA-256 checksums | nothing (but secrets are memory-only on Linux, see README) |

Artifacts land under `VibeLens/target/<triple>/release/bundle/…` (the workspace `target/`, not `src-tauri/target/`).

---

## 2. What it costs (production-honest)

Prices checked in late 2026. Verify before you buy; they change and some vary by country.

| Item | Needed for | Cost | Notes for you specifically |
|---|---|---|---|
| **Apple Developer Program** (individual) | macOS signing + notarisation | **US$99/year** (billed in local currency in the ZA store) | Individual enrolment works from South Africa with your Apple ID and ID document. An organisation account needs a D-U-N-S number, and is only worth it if you sell under a company name (the certificate shows that name). |
| **Windows code-signing certificate** | Authenticode | see below | **Azure Artifact Signing (formerly Trusted Signing), the cheap US$9.99/month option, is not available to you.** Individuals must be in the US or Canada. Organisations must be in the US, Canada, EU, UK, Australia, NZ, Japan, South Korea, Singapore, Switzerland, Norway or Israel. South Africa isn't on the list. |
| ↳ OV cert + cloud signing (e.g. SSL.com OV + eSigner) | | about **US$129/year** cert plus eSigner from about **US$15/month** (annual billing, 20 signings/month) | Since June 2023 the private key must live in a hardware token or cloud HSM, so no `.pfx` on disk. Cloud signing is what makes CI possible. Each release signs about 4 files (the app exe inside each installer, plus each installer), so 20/month is plenty. |
| ↳ OV cert + Azure Key Vault Premium (HSM) + `relic` | | cert (about US$130–450/year depending on CA) plus about US$5/month per HSM key | Unlimited signings. More moving parts. Works from any country your CA validates. |
| ↳ EV cert | | about US$350+/year | **No longer buys instant SmartScreen reputation** (Microsoft changed this in 2024). Not worth the premium for you. |
| GitHub Actions minutes | CI builds | public repo: free. Private repo: macOS minutes cost roughly 10× Linux minutes. | A full release (3 jobs, cold cache, DataFusion in the tree) is about 40–90 runner-minutes, of which macOS is about 25–40. Check current rates. |
| Hosting downloads | | GitHub Releases: free | **A private repo's releases are not downloadable by people without repo access.** See §7. |
| Tauri updater keys | auto-update | free | Losing the private key means existing installs can never auto-update again. |

**Realistic year-one budget:**
- macOS only: US$99.
- macOS + properly signed Windows: about US$99 + US$129 + US$180 = **about US$410/year**.
- Testers only: US$99, with Windows shipped unsigned with instructions. This is fine for a handful of technical people, and not fine for a bank's SOE laptops (§6.4).

---

## 3. Versioning

The version lives in four files; keep them identical:

| File | Field |
|---|---|
| `VibeLens/src-tauri/tauri.conf.json` | `"version"`. **This is what the installers and the updater use.** |
| `VibeLens/package.json` | `"version"` |
| `VibeLens/src-tauri/Cargo.toml` | `version` |
| `VibeLens/Cargo.toml` | `[workspace.package] version` (used by `vibelens-core`) |

```bash
# from VibeLens/ — bump all four (perl is portable across macOS and Linux; replaces the first match only)
V=0.2.0
perl -0pi -e "s/\"version\": \"[^\"]+\"/\"version\": \"$V\"/" src-tauri/tauri.conf.json package.json
perl -0pi -e "s/^version = \"[^\"]+\"/version = \"$V\"/m" src-tauri/Cargo.toml Cargo.toml
cargo update -w                       # refresh the workspace versions in Cargo.lock
grep -n "\"version\"\|^version" src-tauri/tauri.conf.json package.json src-tauri/Cargo.toml Cargo.toml
git commit -am "Release v$V" && git tag "v$V" && git push && git push --tags   # on the release branch (§0); builds are started separately (§9)
```

- Use SemVer.
- Pre-releases (`0.2.0-beta.1`) are fine on macOS and for NSIS, but **the MSI bundler only accepts a numeric pre-release identifier** (`0.2.0-1`). RPM's version field rejects SemVer prerelease hyphens, so prerelease Linux builds ship `.AppImage` and `.deb` only. For betas, either use a numeric Windows pre-release, ship NSIS only (`--bundles nsis`), or set `bundle.windows.wix.version`.
- Never reuse a version number; updaters and Windows installers both rely on it increasing.

Keep `identifier` (`com.heinvandermerwe.vibelens`) stable for future updates; it keys the app's config directory, keychain service, and Windows install registration. This release uses a fresh identity and does not import profiles or saved secrets from earlier app versions.

---

## 4. Build locally (no signing)

Do this once on your Mac before you automate anything; it's the fastest way to find bundling problems.

```bash
cd VibeLens
npm ci
./scripts/fetch-rds-ca.sh                 # REQUIRED: bundle.resources = resources/*; the build fails if it's empty
rustup target add aarch64-apple-darwin x86_64-apple-darwin
npm run tauri build -- --target universal-apple-darwin
open target/universal-apple-darwin/release/bundle/dmg/
```

- First release build: 15–30 min on an M-series Mac. DataFusion and Arrow dominate, and a universal build compiles everything twice.
- Note the DMG size and put it in the release notes. The Iceberg engine adds tens of MB. A build without Iceberg isn't wired up as a feature flag in `src-tauri` yet; add one if size becomes a complaint.
- Run it from `/Applications`, not from the DMG.
- Unsigned local builds run on *your* Mac because you built them (no quarantine attribute). That says nothing about how they behave when downloaded; test that via §9.

**Windows locally** (on a Windows machine):
- Visual Studio Build Tools (C++), Rust, Node 24, then `npm ci`, `scripts\fetch-rds-ca.ps1`, `npm run tauri build`.
- Cross-compiling Windows installers from macOS is possible but experimental, and signing then needs a custom `signCommand`. Use CI instead.

> **Honest status:** VibeLens has so far only been *run* on macOS. Windows is compiled in CI but has never been launched. Make the first Windows release a **beta**, and run the smoke and platform sections of the manual test plan on a real Windows 11 machine first. The Credential Manager and WebView2 paths are the risk.

---

## 5. macOS: signing and notarisation

### 5.1 One-time setup
1. **Enrol** in the Apple Developer Program (individual): developer.apple.com/programs. Allow 24–48 h for approval.
2. **Create a "Developer ID Application" certificate.** Only the account holder can, which is you. The easiest route is Xcode → Settings → Accounts → Manage Certificates → + → Developer ID Application. Or create a CSR in Keychain Access and upload it at developer.apple.com → Certificates.
3. **Find the identity:**
   ```bash
   security find-identity -v -p codesigning
   # → "Developer ID Application: Hein van der Merwe (TEAMID1234)"
   ```
4. **Create an App Store Connect API key for notarisation.** This is preferred over Apple ID + password: it's revocable and has no 2FA prompts. Go to App Store Connect → Users and Access → **Integrations** → App Store Connect API → **+**, with access **Developer**.
   - Note the **Issuer ID** and **Key ID**.
   - Download the `.p8`. You can only download it **once**; store it in your password manager.
5. **Back up the certificate:** in Keychain Access, export the certificate *with its private key* as a `.p12` with a strong password, and store it in your password manager. You need it for CI and if your Mac dies.

### 5.2 Signed + notarised local build
```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: Hein van der Merwe (TEAMID1234)"
export APPLE_API_ISSUER="<issuer-uuid>"
export APPLE_API_KEY="<KEYID>"
export APPLE_API_KEY_PATH="$HOME/.appstoreconnect/AuthKey_<KEYID>.p8"
npm run tauri build -- --target universal-apple-darwin
```

Tauri signs with the hardened runtime (on by default, `bundle.macOS.hardenedRuntime`), submits to Apple's notary service, waits, and staples the ticket. Notarisation usually takes 1–10 minutes, occasionally much longer.

**Alternative to the API key:**
- `APPLE_ID` + `APPLE_PASSWORD` + `APPLE_TEAM_ID`, where the password is an **app-specific password** from account.apple.com, not your Apple ID password.

**Entitlements:** none needed today. VibeLens isn't sandboxed (it's not a Mac App Store app), the keychain works for Developer ID apps without entitlements, and outbound network needs no entitlement outside the sandbox. If you ever add the Mac App Store as a channel, that's a different certificate ("Apple Distribution"), the App Sandbox, and a lot of rework: don't.

### 5.3 Verify before you share
```bash
APP="target/universal-apple-darwin/release/bundle/macos/VibeLens.app"
DMG=$(ls target/universal-apple-darwin/release/bundle/dmg/*.dmg)
codesign --verify --deep --strict --verbose=2 "$APP"           # valid on disk, satisfies its Designated Requirement
codesign -dv --verbose=4 "$APP" 2>&1 | grep -E "Authority|TeamIdentifier|Runtime"   # Developer ID + flags=0x10000(runtime)
spctl -a -vvv -t exec "$APP"                                    # accepted, source=Notarized Developer ID
xcrun stapler validate "$APP"
spctl -a -vvv -t open --context context:primary-signature "$DMG"
```

If the **DMG** isn't accepted (the app inside is, but the container isn't notarised), notarise and staple it too:
```bash
xcrun notarytool submit "$DMG" --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER" --wait
xcrun stapler staple "$DMG"
```

### 5.4 Things that will bite
- **Keychain prompts after the first signed release.** macOS ties keychain item access to the app's code signature. Testers moving from an unsigned or ad-hoc build to the Developer ID build get "VibeLens wants to use your confidential information stored in…" once, because all secrets share one keychain item. **Always Allow** fixes it. Say so in the release notes. After that, stay on the same Team ID forever.
- **Minimum macOS is 11.0** (`bundle.macOS.minimumSystemVersion`), but the real floor is **Safari 16.4's WebKit**. Big Sur and Monterey users must have updated Safari. Older engines show the in-app "needs a newer system WebKit" screen rather than a blank window.
- **Universal vs per-arch.** Universal is one download that works everywhere at about 2× the binary size. Fine for now.
- **macOS 26 icons.** Without a Liquid Glass (Icon Composer) icon, Tahoe draws the classic icon inside a system squircle. Acceptable, not beautiful.

### 5.5 Ad-hoc signing (free stopgap)
`"signingIdentity": "-"` in `bundle.macOS` gives an ad-hoc signature. Apple Silicon needs *some* signature to run downloaded code, but users still have to use **Open Anyway** in Privacy & Security. It's OK for two or three technical friends, and not for anything you'd call a release.

---

## 6. Windows: signing

### 6.1 Pick a route (given you're in South Africa)
| Route | Use when | Setup |
|---|---|---|
| **A. Unsigned** | a few technical testers | nothing; send them the SmartScreen instructions (§6.4) |
| **B. OV cert + SSL.com eSigner** (recommended first paid route) | sharing beyond friends | buy an OV (or individual-validated) code-signing cert from SSL.com, enable eSigner, use their `CodeSignTool` as Tauri's `signCommand` |
| **C. OV cert in Azure Key Vault Premium + relic** | you already live in Azure; want unlimited signings | CA that supports Key Vault (DigiCert, GlobalSign); `relic` as `signCommand`. Tauri documents this path. |
| D. Azure Artifact Signing | only if you incorporate in an eligible country | `artifact-signing-cli` as `signCommand` |

Identity validation for OV takes days (documents, a phone call). Start it a couple of weeks before you need it.

### 6.2 How Tauri calls your signer
Tauri runs `bundle.windows.signCommand` for every binary it produces. `%1` is the file path.

```jsonc
// src-tauri/tauri.conf.json → bundle.windows (example shapes; pick one)
"signCommand": "relic sign --file %1 --key azure --config relic.conf"
// or a small wrapper script that calls SSL.com CodeSignTool with credentials from env vars:
"signCommand": "pwsh -File ../scripts/sign-windows.ps1 %1"
```

Put credentials in environment variables or CI secrets, never in the repo. Always timestamp (RFC 3161), so signatures outlive the certificate.

### 6.3 Verify
```powershell
Get-AuthenticodeSignature .\VibeLens_0.2.0_x64-setup.exe | Format-List   # Status: Valid
signtool verify /pa /v .\VibeLens_0.2.0_x64-setup.exe
```

### 6.4 What testers will see anyway
**SmartScreen reputation** builds per certificate over downloads and time. A brand-new certificate on a brand-new app can *still* warn for the first releases. Reusing the same certificate carries reputation forward. Microsoft's file-submission portal can speed it up, but there's no guarantee.

**Corporate laptops** (your bank's) usually run AppLocker/WDAC or Defender for Endpoint policies: an unsigned, or unknown-signed, app won't run regardless of SmartScreen. Don't plan a corporate pilot without a signed build and an allow-list request to the desktop team. Your licence's commercial-use clause also kicks in there.

**WebView2:**
- It's preinstalled on Windows 10/11. The installer's default mode (download bootstrapper) handles stragglers but needs internet at install time.
- For locked-down networks, set `bundle.windows.webviewInstallMode` to `offlineInstaller`. That adds about 130 MB to the installer.

---

## 7. Where to host downloads

| Option | Good | Watch out |
|---|---|---|
| **GitHub Releases on a public repo** | free, the updater can point at it, checksums next to binaries | the code is source-available; is the repo public? |
| **GitHub Releases on a private repo** | keeps code private | **testers can't download without repo access**, and the updater can't fetch anonymously |
| **Separate public "vibelens-releases" repo** (binaries only) | private source, public downloads, updater-friendly | CI needs a token with write access to the second repo (tauri-action: `owner`/`repo` inputs + a fine-grained PAT) |
| S3 + CloudFront | full control, custom domain | small cost, you manage it |

Publish `SHA256SUMS.txt` with every release (`shasum -a 256 *` on the artifacts) and link the licence.

---

## 8. Auto-update (not wired yet; do this before the first wide release)

Without it, every tester must re-download by hand. Steps:

1. `cd VibeLens && npm run tauri add updater`. This adds `tauri-plugin-updater` and `@tauri-apps/plugin-updater`. The plugin needs Rust 1.90+.
2. **Generate keys:**
   - `npm run tauri signer generate -- -w ~/.tauri/vibelens.key`
   - Store the **private key and its password** in your password manager **and** as CI secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. These are env vars only; `.env` files aren't read.
3. `tauri.conf.json`:
   ```jsonc
   "bundle": { "createUpdaterArtifacts": true, … },
   "plugins": { "updater": {
       "pubkey": "<contents of ~/.tauri/vibelens.key.pub>",
       "endpoints": ["https://github.com/<owner>/<releases-repo>/releases/latest/download/latest.json"]
   } }
   ```
4. Add `"updater:default"` to `src-tauri/capabilities/default.json`. Register the plugin in `lib.rs` (`#[cfg(desktop)] app.handle().plugin(tauri_plugin_updater::Builder::new().build())?;`).
5. **UI:**
   - Check for updates on launch (at most daily).
   - Show "Update available: vX.Y.Z" with the release notes.
   - **Install only on click.** Never restart under the user while tabs are dirty; the close guard (`lifecycle.rs`) already knows the count.
6. `tauri-action` uploads `latest.json` plus `.sig` files automatically once `createUpdaterArtifacts` is on (`uploadUpdaterJson` defaults to true).

Update artifacts:
- macOS: `VibeLens.app.tar.gz` (+ `.sig`).
- Windows: the NSIS `-setup.exe` (+ `.sig`).
- Linux: the `.AppImage` (+ `.sig`). `.deb` and `.rpm` users update through their package manager or by hand.

The updater signature is **separate from** Apple/Microsoft code signing. You need both.

---

## 9. CI: `.github/workflows/release.yml`

Already in the repo. It is started by hand on a tag (`gh workflow run release.yml --ref vX.Y.Z`, or Actions → Release → Run workflow and pick the tag); pushing a tag does not start it. It:
- builds macOS universal, Windows x64 and Linux x64 in parallel;
- fetches the RDS CA bundle;
- signs and notarises **only if the secrets exist**;
- creates a **draft** release with all installers attached.

**Secrets** (repo → Settings → Secrets and variables → Actions):

| Secret | Value |
|---|---|
| `APPLE_CERTIFICATE` | `base64 -i DeveloperID.p12 \| pbcopy` |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12` password |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: Hein van der Merwe (TEAMID1234)` |
| `APPLE_API_ISSUER` | App Store Connect issuer ID |
| `APPLE_API_KEY` | key ID |
| `APPLE_API_KEY_P8` | full text of `AuthKey_<KEYID>.p8` |
| `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | updater keys, once §8 is done |
| Windows signer secrets | depends on route B/C (§6.1) |

Treat these secrets as production credentials:
- Anyone who can push a workflow change to the repo can exfiltrate them, so protect the `main` branch and tags.
- Consider a GitHub **environment** named `release` with you as required reviewer, and scope the secrets to it.

**After the workflow finishes:**
1. Download each artifact **in a browser** onto a clean machine or user account. That's how the quarantine attribute and Mark-of-the-Web get set, which is the only realistic Gatekeeper/SmartScreen test.
2. Run the smoke section of the manual test plan on macOS and Windows, plus its install and first-connection cases.
3. Write the release notes:
   - what's new;
   - known issues (P1 failures from the test plan);
   - the keychain-prompt note if relevant;
   - the minimum OS (macOS 11 with Safari 16.4+; Windows 10/11 with WebView2);
   - the licence summary.
4. Upload `SHA256SUMS.txt` and publish the draft.

---

## 10. Licence and legal checklist (every release)

- **Ship your licence inside the app bundle.**
  - Set `bundle.licenseFile` to `../../LICENCE.md`. The NSIS/MSI installers then show it.
  - Link it from the About dialog.
  - Put the commercial-use summary in the release notes: free for personal use; commercial use needs a paid licence or your written approval; MSSQL/Oracle restriction.
- **Third-party notices.**
  - VibeLens bundles hundreds of MIT/Apache-2.0/BSD crates and npm packages. Apache-2.0 components with a `NOTICE` file (Arrow, DataFusion, iceberg-rust, AWS SDK) require you to reproduce those notices when you distribute binaries.
  - Generate them with `cargo about generate` (Rust) and `npx license-checker --production` (npm). Ship the result as `THIRD_PARTY_NOTICES.md` in `bundle.resources`, and link it from About.
  - This is a real obligation the moment you hand a binary to anyone.
- **Monaco, fonts, icons:** Monaco is MIT. Inter and JetBrains Mono are OFL; include their licence text if you bundle the font files. Lucide is ISC.
- **Data and privacy statement for testers** (two sentences in the release notes): VibeLens sends no telemetry. In Assist/Agent modes it sends prompts and permitted schema/results to the AI provider *the user configures*, nothing otherwise.
- **Export control:** standard TLS/SSH crypto in a mass-market app. Nothing to file for normal distribution, but don't ship to sanctioned destinations.
- **Trademarks:** "PostgreSQL", "Valkey", "Redis", "Apache Iceberg", "AWS" are used descriptively. Don't use their logos in your icon or marketing.

---

## 11. Release checklist (copy into the release issue)

```text
☐ Version bumped in 4 files, CHANGELOG updated, tag pushed
☐ CI green on the release commit (ci.yml)
☐ Manual test plan: P0 all pass, P1 failures listed in notes, results sheet attached
☐ release.yml draft created; artifacts: dmg, setup.exe, msi, AppImage, deb, rpm (+ .sig, latest.json once updater is on)
☐ macOS: spctl accepted (app + dmg), stapler validate OK, tested from a browser download
☐ Windows: Get-AuthenticodeSignature Valid (or unsigned → instructions in notes), installed + uninstalled cleanly
☐ Keychain/Credential Manager: existing profiles still unlock after upgrade
☐ Licence + THIRD_PARTY_NOTICES included; release notes have licence summary + privacy line
☐ SHA256SUMS.txt uploaded
☐ Published; download links tested logged-out
```

**Rollback:**
- Un-publish (convert back to draft) and fix forward with X.Y.Z+1. Don't delete tags that people may have pulled.
- With the updater on, a bad `latest.json` can be replaced by re-pointing it at the previous good release. Installs that already updated stay updated, because the updater by default only installs versions higher than the running one.

---

Sources: [Tauri macOS signing](https://v2.tauri.app/distribute/sign/macos/) · [Tauri Windows signing](https://v2.tauri.app/distribute/sign/windows/) · [Tauri updater](https://v2.tauri.app/plugin/updater/) · [tauri-action](https://github.com/tauri-apps/tauri-action) · [Azure Artifact Signing quickstart (eligibility)](https://learn.microsoft.com/azure/artifact-signing/quickstart) · [DevClass on Artifact Signing pricing](https://devclass.com/2026/01/14/code-signing-windows-apps-may-be-easier-and-more-secure-with-new-azure-artifact-service/) · [Windows code-signing CI costs 2026](https://codenote.net/en/posts/windows-code-signing-ci-cost-per-signing-volume-2026/)
