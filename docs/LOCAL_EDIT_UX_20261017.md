# CloudFile Agent2 local opening and editing UX

Date: 2026-10-17. Scope: lightweight task-centre language, safety actions, and shared application launch rules.

## Three separate user intents

| Entry | Primary goal | Write-back | User-visible action |
| --- | --- | --- | --- |
| Local open | Read, inspect, reference, print | Never automatically | Open file; local edits are not uploaded |
| Normal edit | Office documents and routine business files | Auto-upload only after stable save and server confirmation | Open; immediately upload; pause/resume |
| Exclusive edit | CAD, controlled drawings, enterprise custom apps | Explicit commit or check-in | Commit version / check in and release / query unknown result |

Local save is not cloud publication. Pending outcomes always outrank ticket-expiry messaging.
Never delete a working copy because a remote operation failed. Release of a dirty Checkout requires explicit confirmation.

## Application profiles

Office (Word, Excel, PowerPoint): recommend normal edit, unless business requirements say otherwise.
PDF/images/reference files: recommend local open; editing requires new authorized action.
CAD (DWG, STEP, SolidWorks, Inventor, Revit): recommend exclusive when appropriate, but do not auto-Checkout by extension alone.
Enterprise custom EXE: reuse validated absolute program path and argv template; application selection does not alter server permissions.
Exclusive edit and optimistic edit now use the same local-edit application rule family; local-view remains separate.

## CAD / professional boundaries

Assemblies and drawings may reference multiple dependencies. Initial Agent2 handles one file only and does not create a virtual filesystem.
Do not claim Xrefs, part trees, dependent assets, local plugin files or output directories are synchronized.
Keep the original file name and show an unobtrusive reference warning for common CAD extensions.
Only add dependency retrieval when a real business scenario provides a trusted file manifest and permission contract.

## Subsequent slices, not part of this patch

- Etech/CloudFile real local-view read-only grant (not merely task label).
- GUI for per-extension local application selection, including vetted corporate EXE commands and safe working directory.
- Office atomic-save, AutoCAD Xref/SolidWorks assembly, and one custom enterprise app acceptance tests.
- Refresh browser authorization for long-lived CAD editing without a new local task or work copy.
- Windows standard-user E2E and macOS installed-app URL registration.

References: https://support.box.com/hc/en-us/articles/360044195453-Box-Edit-Frequently-Asked-Questions
https://helpdesk.egnyte.com/hc/en-us/articles/115001597352-Automatic-File-Locking-in-the-Egnyte-Desktop-App
https://help.autodesk.com/cloudhelp/2026/ENU/Vault-General/files/GUID-2317620D-2A98-430A-8F6E-0465D6B66799.htm
