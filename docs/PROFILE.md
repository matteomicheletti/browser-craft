# Moving BrowseCraft to another computer

## Passwords from Firefox or Chrome

Browsers export saved passwords only as a clear-text CSV file:

- Firefox: `about:logins` → `⋯` → *Export Passwords…*
- Chrome: *Settings → Passwords → Export passwords*

Then:

```sh
browsecraft passwords import-csv ~/Downloads/logins.csv
```

The logins go into the KeePass vault (`browsecraft-passwords.kdbx`, which also opens in KeePassXC). Without a vault, one is made in the configuration folder, with a password asked twice in the terminal. Logins already in the vault (same site and user name) are skipped. The CSV is deleted afterwards (`--keep-file` keeps it).

## Bookmarks and history

```sh
browsecraft import firefox --merge --replace-history
```

`--replace-history` clears BrowseCraft's own history first. History older than the retention setting (*Settings → History*, default 365 days) is not imported: set it to *Forever* first to bring everything.

## The whole profile

```sh
browsecraft profile export ~/browsecraft.bcprofile    # on the old computer
browsecraft profile import ~/browsecraft.bcprofile    # on the new one, BrowseCraft closed
```

The file is encrypted with a password asked in the terminal (at least 10 characters): Argon2id (64 MiB, 3 passes) and XChaCha20-Poly1305. It holds settings (`flags.txt`), containers and their rules, dark mode, session presets, user agents, site permissions, bookmarks, history, the password vault and the authenticator vault. The two vaults stay encrypted with their own passwords inside it.

Not included: cookies and site data (logins to web sites do not travel), open windows, plugin folders and automation settings.

Import replaces those files and moves the previous ones to `backup-<time>` in the configuration folder.
