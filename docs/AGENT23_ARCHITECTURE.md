# CloudFile Agent 2.3 — architecture and upgrade baseline

2026-10-17. Product is pre-release; no existing end-user Agent migration is required.
This branch is an initial implementation, NOT a claim that the complete headless
Core split, cross-platform update promotion or real editing acceptance is finished.

## Product and installation

One Rust codebase, two isolated release identities:
- Dev / Preview: CloudFile Agent 2.3 Preview, 2.3.0-dev.N, signed Dev feed.
- Stable: CloudFile Agent, final SemVer, independently signed Stable feed.
- Windows user-owned portable ZIP is the primary constrained-terminal model.
  No MSI, Windows Service, SeaDrive driver or administrator permissions needed.
- macOS ships a Preview .app ZIP. Proper stable distribution needs code signing,
  notarization and **whole-Bundle** update; in-place Bundle mutation is refused.
- Linux reuses the pure Rust Core and Launcher, packaging/desktop integration later.

## Fixed Launcher

The root contains cloudfile-launcher and current.json; each immutable version
is stored under bin/<semver>/ with cloudfile-native-host and cloudfile-desktop.
Every external Chrome NativeMessaging, Windows seafile:// handler and user login
item is registered to the fixed Launcher, not to a version-specific binary.
Launcher routes a Chrome origin/CLI command to the current Native Host,
and --autostart or ordinary GUI opening to the current Desktop.
A user-selected default URL handler is never silently taken from SeaDrive.
The same Browser API, Journal, file store and Guard are reused.

During update the signed Native Host and Desktop artifacts are verified, staged,
started with --version / --component probes, and only then marked pending.
The Launcher checks the user-owned desktop.lock and the durable upgrade gate on
subsequent startup, switching the current.json pointer only when safe. Previous
version remains available for rollback. User data lives outside versioned bins.

**Current phase limitation:** Desktop still owns worker lifetime and checkout
renewal. Extracting a headless cloudfile-core-daemon from Tauri remains the next
P0 workstream. Until then the Launcher waits for the running Desktop to exit
before switching, and an open editing task must be explicitly resolved.

## HTTP and HTTPS

Both HTTP and HTTPS remain first-class for CloudFile/Seafile service origins:
trusted instances, OIDC/SSO, browser handoff, downloads, uploads and native edit
permissions remain governed by their respective existing security checks.

Update distribution ALSO accepts canonical HTTP or HTTPS origins for enterprise
intranets. It deliberately NEVER drops:
- pinned P-256 manifest signature;
- manifest expiry and signed channel/platform/version/protocol fields;
- SHA-256 digest for each binary;
- exact origin AND scheme matching for artifact URLs; redirects disabled;
- fail-closed authentication or update validation.

On HTTP, transport metadata can be observed and network traffic denied/replayed
within allowed expiry. The pinned signature prevents new unauthorized code but
does not provide confidentiality or availability; recommend HTTPS wherever
possible. Do NOT copy the CloudFile application HTTP setting into update code as
a reason to disable release signature or artifact checks.

## Channels

The build's final SemVer (e.g. 2.3.0) is Stable; a pre-release SemVer
(2.3.0-dev.N or 2.3.0-rc.N) is Dev. The signed channel is checked at use.
Release manifest path: <origin>/cloudfile-agent23-<channel>-<os-arch>.json.
Signed release contents are scoped to platform, channel, protocol revision,
expiry and artifact digest. Build packages do not embed a signing private key.

The first 2.3 Dev build is 2.3.0-dev.1. Installed trust source and pinned
public key still require provisioning. A release feed/signing CI workflow is
not yet delivered; thus **do not claim unattended installation is operational**.
Native status no longer makes a synchronous network request: explicit update
checks cache a bounded local status result.

## Hard no-interruption gate

The upgrade gate reads the durable task store, Guard/Journal, active upload
locks and local file digest. An active editing task, local uncommitted changes,
unconfirmed intent, live Checkout, or unreadable state blocks activation.

This is deliberately stricter than safe_to_exit. CLI and UI cannot silently
override the gate. The download and stage step may run with active edits, but
the version pointer only changes when the old desktop is not running and the
gate passes. Never delete work copies or check in files just to upgrade.

TODO: an explicit normal-edit session closure is needed for regular Dev auto
activation after a user finishes a file; legacy files are conservatively treated
as active. The production final UI and headless daemon must share a single
upgrade lock/IPC owner.

## Phased acceptance (not yet passed)

1. macOS ARM64/Intel & Windows x64 CI lint, tests, package archive, Launcher smoke.
2. Windows normal-user no-UAC ZIP install; Chrome, URL, autostart, protocol
   registration all resolve to the same version (and don't take SeaDrive).
3. Real Office/CAD application and checkout/commit/checkin during update prepare.
4. No WebView2 scenario once headless Core is separated.
5. Dev signed feed publishing & safe auto-check/preload; Stable promotion and
   manual confirmation; signed rollback and corrupted-manifest failure.
6. Complete signed macOS .app replacement and Linux desktop packaging.
