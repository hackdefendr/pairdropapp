# Cutting a release

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
runtime on for a real identity.

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
