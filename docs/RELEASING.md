# Cutting a release

## Tags

`v0.1.0` was cut before there was more than one client, and means macOS. Now that the
repository holds several, tags are platform-prefixed — `linux-v0.1.0` — so it's obvious
what a release contains and the platforms can move at their own pace. The macOS
`.github/workflows/release.yml` triggers on `v*` only, so a prefixed tag never fires it.

# macOS

## 1. Set the version

`macos/Resources/Info.plist` holds the version that a source build gets.
`package.sh` stamps its own — pass the version as its argument — but keep the plist in
step so `./install.sh` and the Settings window report the same number.

```sh
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString 0.1.0" macos/Resources/Info.plist
```

`CFBundleVersion` in the plist stays at `1`; `package.sh` replaces it with a UTC
timestamp so every release build is monotonically newer than the last.

## 2. Test

```sh
cd shared/PairDropKit && swift test        # 19 protocol tests
```

Then exercise the app itself against a real instance — at minimum: connect, see a peer,
send a file, receive a file, send text, and open Settings.

## 3. Build the artifacts

```sh
cd macos
./package.sh 0.1.0
```

Produces, in `macos/dist/`:

| | |
|---|---|
| `PairDrop-0.1.0-macOS-universal.dmg` | the download most people want |
| `PairDrop-0.1.0-macOS-universal.zip` | for scripted installs |
| `SHA256SUMS` | checksums for both |

Attach all three to the release.

### Signing

Unsigned releases work, but every downloader has to clear quarantine by hand. With a
Developer ID and a notarization profile the download just opens:

```sh
xcrun notarytool store-credentials pairdrop \
    --apple-id you@example.com --team-id TEAMID --password <app-specific-password>

SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
NOTARY_PROFILE=pairdrop ./package.sh 0.1.0
```

`package.sh` then signs with the hardened runtime, notarizes and staples the app, and
notarizes and staples the disk image. Ad-hoc signing and the hardened runtime are
mutually exclusive: dyld refuses an embedded framework whose Team ID doesn't match the
app's, and ad-hoc signatures have no Team ID — so `build.sh` only turns the hardened
runtime on for a real identity. A real identity also gets `--timestamp`; notarization
rejects a signature without a secure timestamp.

CI has no keychain to hold a notarization profile, so `package.sh` also accepts an App
Store Connect API key directly:

```sh
NOTARY_KEY=~/private_keys/AuthKey_ABC123.p8 \
NOTARY_KEY_ID=ABC123 \
NOTARY_ISSUER=12345678-1234-1234-1234-123456789012 \
SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" ./package.sh 0.1.0
```

### Signed builds in CI

`.github/workflows/release.yml` signs and notarizes when these repository secrets exist,
and quietly falls back to an ad-hoc build when they don't:

| Secret | What it is |
|---|---|
| `MACOS_CERTIFICATE_P12` | base64 of the Developer ID Application `.p12` |
| `MACOS_CERTIFICATE_PASSWORD` | the password used when exporting it |
| `APPLE_API_KEY_P8` | base64 of the App Store Connect `AuthKey_*.p8` |
| `APPLE_API_KEY_ID` | that key's ID |
| `APPLE_API_ISSUER_ID` | the issuer UUID |

Export the certificate from Keychain Access by selecting the **identity** (the certificate
with its private key underneath, not the certificate alone) → Export → `.p12`. Then:

```sh
base64 -i Certificates.p12 | gh secret set MACOS_CERTIFICATE_P12
gh secret set MACOS_CERTIFICATE_PASSWORD
base64 -i AuthKey_ABC123.p8 | gh secret set APPLE_API_KEY_P8
gh secret set APPLE_API_KEY_ID
gh secret set APPLE_API_ISSUER_ID
```

The workflow imports the certificate into a throwaway keychain that is deleted when the
job ends, reads the signing identity out of the certificate itself (so there's no sixth
secret to drift), and signs by SHA-1 hash rather than by name.

Two things that will waste an afternoon if you hit them cold:

- **Export the `.p12` with the Apple WWDR intermediate**, or the trust chain won't resolve
  on the runner. The workflow warns rather than failing, since signing often still works,
  but notarization won't be happy.
- **If you ever build a `.p12` with OpenSSL 3 rather than Keychain Access**, pass
  `-legacy -macalg sha1 -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES`. OpenSSL 3 defaults
  to an AES/SHA-256 encoding that Apple's `security import` cannot read, and the only
  symptom is `MAC verification failed during PKCS12 import (wrong password?)` — which
  sends you hunting for a password problem that doesn't exist.

## 4. Tag and publish

```sh
git push origin main

gh release create v0.1.0 \
    macos/dist/PairDrop-0.1.0-macOS-universal.dmg \
    macos/dist/PairDrop-0.1.0-macOS-universal.zip \
    macos/dist/SHA256SUMS \
    --title "PairDrop for macOS 0.1.0" \
    --notes-file docs/release-notes/v0.1.0.md
```

`gh release create` makes the tag itself, at the head of the default branch — no local
`git tag` needed. Authentication is whatever `gh auth status` reports; `gh auth setup-git`
points git at the same token so an `https://` remote pushes without a separate SSH key or
personal access token.

`.github/workflows/release.yml` builds and attaches the same artifacts when a `v*` tag is
pushed (as a draft), so `gh release create` is only needed if you'd rather upload a
locally built, signed copy. The workflow can only produce ad-hoc signed builds — the
signing identity isn't on the runner — and it has never run, so for the first release
build locally and treat the workflow as something to shake out afterwards with
`workflow_dispatch`.

Note that the tag `gh release create` makes also fires that trigger, and the workflow
would then modify the release you just published. `gh workflow disable release.yml` before
publishing by hand, and re-enable it once you're happy.

## 5. After publishing

Download the DMG from the release page on a machine that has never run the app, and check
it opens. That is the only way to see what a first-time user actually gets, quarantine
and all.

# Linux

## 1. Build the bundle

Built on an x86_64 Linux machine — the Flatpak bundle is architecture-specific and there
is no cross-build.

```sh
cd linux
./flatpak/build.sh --bundle
```

That writes `linux/pairdrop.flatpak`. Rename it with the version and architecture, and
checksum it:

```sh
mkdir -p dist
mv pairdrop.flatpak dist/PairDrop-0.1.0-linux-x86_64.flatpak
( cd dist && shasum -a 256 PairDrop-*.flatpak > SHA256SUMS )
```

## 2. Publish

Prefix the tag, so it's clear which client the release is for:

```sh
gh release create linux-v0.1.0 \
    linux/dist/PairDrop-0.1.0-linux-x86_64.flatpak \
    linux/dist/SHA256SUMS \
    --title "PairDrop for Linux 0.1.0" \
    --notes-file docs/release-notes/linux-v0.1.0.md
```

## 3. Verify

**Check the bundle installs before publishing it**, because a bundle that doesn't install is
worse than no release:

```sh
flatpak uninstall --user -y app.pairdrop.Linux
flatpak install --user -y --bundle dist/PairDrop-0.1.0-linux-x86_64.flatpak
flatpak run app.pairdrop.Linux
```

Regenerate `flatpak/cargo-sources.json` whenever `Cargo.lock` changes, or the build will
compile the versions from whenever it was last generated.
