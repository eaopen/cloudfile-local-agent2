# Seafile Open via Client compatibility (Agent2, 2026-10-17)

## Scope

Agent2 remains a per-file editor, not a SeaDrive replacement or a virtual
filesystem. This change accepts **locators** of the form:

`seafile://openfile?repo_id=<UUID>&path=%2Ffolder%2Ffile.docx`

It does not treat a link, optional `email`, or the operating system's default
application association as file authorization.

- An existing Agent-owned copy with the exact repository UUID and path can be
  opened again. Its original edit task retains ownership of future uploads.
- Otherwise the link resolves only against previously trusted CloudFile service
  origins. If there is one match, Agent opens the **normal authenticated
  Seahub file page**. The user must sign in and explicitly select local edit
  to receive the CloudFile V2 scoped `cloudfile-open://v2/open` ticket.
- A missing or ambiguous server is rejected. No network-wide server discovery,
  credential capture, full-library synchronization, raw Seafile admin token,
  or upload without a current grant is introduced.
- A generic, unmodified Seafile CE/Pro backend is **not yet** a supported
  uploading target. This is an entry compatibility step for CloudFile V2.

## Windows: registered candidate, not a takeover

Unpack Agent2 (Native Host and desktop beside each other) in an ordinary
user-writable directory. No file-system driver or administrator access is
required by our installer commands; organizational WDAC/AppLocker and WebView2
availability are still external constraints.

```
cloudfile-native-host.exe --register-protocols
cloudfile-native-host.exe --protocol-state
```

Registration creates **HKCU-only** application ProgIDs and Windows
`RegisteredApplications` capabilities for `seafile` and `cloudfile-open`.
If neither the current user nor the machine has an existing URL-scheme handler,
it also registers a direct HKCU scheme so the URL can actually launch without
SeaDrive. It refuses to shadow an existing merged HKCR handler. It
intentionally does **not** change `UserChoice` or the default handlers
already selected for Seafile Sync Client / SeaDrive. Users can select CloudFile
Agent2 via *Windows Settings → Apps → Default apps → Choose defaults by link
type*. Unknown or non-Agent-owned registry classes are never overwritten.

```
cloudfile-native-host.exe --unregister-protocols
```

Unregistration deletes only classes bearing this Agent's ownership marker,
not SeaDrive's classes, SeaDrive's files, user work copies or `UserChoice`.
Switch the default away from Agent2 first if Windows still uses its ProgID.

The registered command invokes
`cloudfile-native-host.exe --open-uri "<uri>"`. A resident Tauri task center
is started through the existing same-user IPC mailbox. If unavailable, the
opening fails without treating an unverified operation as successful.

## macOS and Linux

URI parsing, trusted origin resolution and the explicit `--open-uri` CLI
are cross-platform. macOS application URL association requires a packaged
`.app` with LaunchServices URL declarations; automatic registration is
**not** part of this first Windows-focused change. Linux desktop URI
registration and user installation are deferred. Chrome Native Messaging
continues to work where permitted.

## SeaDrive coexistence

- Installed SeaDrive / Sync Client keep the `seafile://` default unless the
  user **explicitly** selects Agent2.
- Files opened by SeaDrive are owned and uploaded by SeaDrive. Agent2 does
  not scan or write inside its virtual drive/cache.
- Files opened by Agent2 are separate Agent-owned working copies and managed
  with the existing Journal, snapshot, File ID and Checkout rules.
- `cloudfile-open://v2/open` never falls back to a legacy .NET/Go Host.

## Verification gates still needed

1. Windows 10/11 **non-administrator** installation/URL defaults, no UAC.
2. Fresh Office file: browser login → V2 handoff → desktop open → automatic
   upload → new Seafile version and customer display.
3. Existing SeaDrive default retained; switching and uninstall restoration;
   no dual writers on the same local file.
4. Cross-process authorization, ticket expiry/reauthorization, network loss,
   file replacement on save, conflicts and incomplete receipts.
5. Windows WebView2 missing/blocked diagnosis, and macOS signed/unsigned
   LaunchServices packaging.
