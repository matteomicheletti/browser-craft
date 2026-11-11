# Manual tests

Checks that need a person: real accounts, keyboard focus under Wayland
(synthetic key events do not reach XWayland windows on GNOME), audio/video.
Record the result (OK / KO + note) and the date in the last column.

Build under test: `browsecraft --version`, installed by
`scripts/install-local.sh`.

## 1. Keyboard (inside a page and in the toolbar)

| # | Action | Expected | Result |
| --- | --- | --- | --- |
| 1.1 | Click inside a page, press Ctrl+T / Ctrl+W | New tab / tab closed | |
| 1.2 | Inside a page, Ctrl+L, type a URL, Enter | Address bar focused, page opens | |
| 1.3 | Inside a page, F5 / Ctrl+R | Page reloads | |
| 1.4 | Inside a page, Alt+Left / Alt+Right | Back / forward | |
| 1.5 | Inside a page, F12 and Ctrl+Shift+I | Web inspector opens | |
| 1.6 | Ctrl+P (and F1) | Command palette opens | |
| 1.7 | In a text field of a page, Ctrl+Left/Right | Cursor moves by word (not history) | |
| 1.8 | Ctrl+1…Ctrl+9, Ctrl+PageUp/PageDown, Ctrl+Shift+T | Chrome behaviour | |

## 2. Logins and sessions (real accounts)

| # | Site | Check | Result |
| --- | --- | --- | --- |
| 2.1 | github.com | Log in with password + 2FA; close and reopen the browser: still logged in | |
| 2.2 | gitlab.com | Log in; open a merge request; comment box works | |
| 2.3 | Google account (accounts.google.com) | Log in; Google may refuse "unsupported browser": note the message | |
| 2.4 | Gmail / Google Drive / Docs | Inbox loads, a Doc opens and accepts typing | |
| 2.5 | Microsoft 365 (outlook.office.com, office.com) | Log in (incl. MFA/Authenticator push), Outlook web and a Word doc | |
| 2.6 | Teams web | Chat loads; note call/camera behaviour | |
| 2.7 | Atlassian (Jira/Confluence Cloud) | Log in with SSO; edit a page | |
| 2.8 | Bitbucket | Log in; open a PR diff | |
| 2.9 | A bank or PSD2 login | Only if you are comfortable; note any "browser not supported" | |
| 2.10 | Sites with SSO pop-ups (OAuth "Sign in with Google/Microsoft") | The pop-up opens as a tab and the login returns to the site | |
| 2.11 | Passkeys / WebAuthn (github.com → passkey) | Note whether WebKitGTK offers it | |
| 2.12 | Cookie banner + "remember me" | Choice persists after restart | |

## 3. Containers (after wave 1)

| # | Check | Result |
| --- | --- | --- |
| 3.1 | GitHub personal in container "Personal", work account in "Work", both open at once, no logout in either | |
| 3.2 | Restart: both sessions still there | |
| 3.3 | Temporary container: log in, close the tab, reopen in a new temporary container: logged out | |
| 3.4 | Rule "github.com → Work": opening github.com from the address bar lands in Work | |
| 3.5 | Tab colour/indicator shows the container | |
| 3.6 | Export containers, delete one, import: restored | |

## 4. Authenticator (after wave 1)

| # | Check | Result |
| --- | --- | --- |
| 4.1 | Add a test account from an `otpauth://` URI; code matches another app (e.g. phone authenticator) for the same secret | |
| 4.2 | Import from a QR image | |
| 4.3 | Lock, restart, unlock with the master password | |
| 4.4 | Encrypted export, import on a fresh profile | |
| 4.5 | Real 2FA login on one site using the code | |

## 5. Media and the rest

| # | Check | Result |
| --- | --- | --- |
| 5.1 | YouTube: video plays with sound, fullscreen button | |
| 5.2 | Netflix/Spotify (DRM): note result (WebKitGTK usually has no Widevine) | |
| 5.3 | Download a file; upload a file in a form | |
| 5.4 | Camera/microphone permission (meet.jit.si) | |
| 5.5 | Print (palette → Print) | |
| 5.6 | Screen recording (palette) produces a playable file | |
| 5.7 | Dark mode on a light site, off on an excluded site | |
| 5.8 | HiDPI: GNOME scaling 125%/200%: text sharpness, page size correct | |
