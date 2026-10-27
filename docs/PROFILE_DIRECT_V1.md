# CloudFile Rust Agent2 — direct ApplicationProfile V1 consumer

## Design

Agent2 remains the **active file application**. A business application may publish Generic Local Bridge ApplicationProfile V1 without installing or running Local Bridge. This consumer is optional: the legacy Chrome / Native Host / CloudFile Hub/Server file view, normal edit and exclusive edit flows remain unchanged.

- Discovery: `GET {application-origin}/admin-api/local-bridge/profile/v1`, or the **exact same endpoint** through an existing public API prefix (e.g. `/prod-api/admin-api/local-bridge/profile/v1`); no vendor names in Agent2.
- Initial sync: user explicitly runs `cloudfile-native-host --sync-app-profile <canonical-origin-or-discovery-url>`. It accepts only the fixed discovery path with an optional canonical reverse-proxy prefix, never an arbitrary URL. No browser or arbitrary web page can invoke this CLI route through Native Messaging.
- Verification: same app origin, V1 schema and TTL, cap on body size, no redirects, permitted logo/web entries, declared CloudFile file capabilities, and `services.files` resolving to a **previously trusted** local CloudFile service. Existing trusted service paths must agree.
- Storage: user-private `application-profiles/<profile_id>.json`; a profile cannot overwrite the same ID from another app origin, provider or environment, or roll back the revision. The saved profile has **no credentials**.
- Display: `--list-app-profiles` and `--status` show redacted labels; the desktop task list may show the application name instead of the raw file-server origin when the mapping is unambiguous.
- Offline/expired: metadata can label an existing task but **never authorizes a new file action**. Download failure leaves previous data, trusted services and file tasks untouched.
- Upgrade/update: `update.source_id` is an inert label, **not** a URL/public-key override. DEV/PROD remain distinct profile instance IDs and continue using separate Host/install identities; no automatic Host takeover or workspace migration.

## Manual DEV setup

```sh
# Existing CloudFile service must already be trusted/paired as before.
cloudfile-native-host --sync-app-profile http://eap-dev.example.test:48080

# An actual Etech Web reverse proxy may expose EAP under /prod-api/.
# Use the deployed site's exact publicly reachable discovery URL:
cloudfile-native-host --sync-app-profile http://etech.stcetech.ad01.sec.com/prod-api/admin-api/local-bridge/profile/v1

cloudfile-native-host --list-app-profiles
```

The enterprise app must actually deploy its online V1 endpoint. For Etech specifically, existing front-end code serves a stable `/favicon.ico` and registers `/aiChatList`, while `/obpm-api` is the public AI API proxy. Those are application metadata only; Agent2 does not use them to authorize files. A direct backend-only Origin may not serve Web UI assets. EAP `dev` yields `etech-dev`, `prod` yields `etech-prod`, and these are simply examples of **application-provided IDs**; Agent2 code is provider-neutral. If the URL belongs to an unknown fileserver, the CLI refuses to bind it and does not call `session::trust`.

Refresh currently uses another explicit `--sync-app-profile` call; background auto-refresh, authenticated Profile enrollment, persistent trusted-origin pinning for unrelated services and runtime Bridge takeover are *not* part of this slice. Production rollout needs real Windows/macOS EAP/CloudFile verification first.

### Compatibility

No modifications to existing `config.json` allowed origin logic, `service_paths`, device pairing keys, `task.json`, journal snapshots, update manifests, Hub ticket flows, edit/commit, or API permissions. Missing/invalid Profile is nonfatal to all existing file tasks. The full ApplicationProfile contract is owned by the [Generic Local Bridge](https://github.com/eaopen/local-bridge/blob/dev/docs/APPLICATION_PROFILE_V1.md).

Note: public HTTP endpoints supply metadata without confidentiality/integrity; do not interpret a fetched Profile as proof of enterprise authorization. For confidential organizational data, use HTTPS and appropriate app enrollment. User-specific SSO and file ACL checks remain exclusively in existing business/CloudFile flows.
