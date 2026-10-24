# Security

## Supported versions

Only the [latest release](https://github.com/TonyWong0086/Evict/releases/latest) is supported. Check that you're on it before reporting.

## Reporting

Don't open a public issue. Use [private reporting](https://github.com/TonyWong0086/Evict/security/advisories/new) and include:

- What the problem is and what someone could do with it.
- Steps to reproduce.
- Your Evict and Windows versions.

You'll get a reply within a week. After a fix is released, the advisory is published and you're credited unless you ask not to be.

## In scope

Evict runs as administrator and deletes files, so these count:

- Deleting anything that wasn't in the confirmed list.
- A crafted registry entry, shortcut or folder name making Evict delete or overwrite the wrong path.
- Running code with Evict's admin rights, for example through DLL loading or uninstall strings.
- Removing a protected location such as a drive root, `Windows` or Program Files.

## Out of scope

- A program's own uninstaller misbehaving. Evict only starts it.
- The unsigned exe warning. You can build it from source instead.
- Removing items you ticked yourself.
