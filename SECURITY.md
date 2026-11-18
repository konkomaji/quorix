# Security policy

## Supported versions

Quorix is in early development. There is no stable release.
Security repairs go to the `main` branch only.

## How to report a problem

Do not report a security problem in a public issue, a discussion or a pull request.

Use one of these two methods:

1. **Private report on GitHub (preferred).** Open the [Security tab](https://github.com/konkomaji/quorix/security/advisories/new) of the repository and select "Report a vulnerability".
2. **Email.** Send the report to konkomaji@gmail.com. Put `Quorix security` in the subject line.

## What to put in the report

- The version or the commit that you used.
- The operating system.
- The steps to cause the problem.
- The result that you expected and the result that you got.
- The effect, if you know it. Example: a file written out of the selected folder.

Do not send real passwords, tokens or private links. Replace them with example values.

## What occurs after your report

1. You get an answer that confirms the report.
2. The maintainer examines the problem and tells you the result.
3. The maintainer prepares a repair in private.
4. The repair is published together with an advisory. The advisory gives you credit, unless you do not want it.

Quorix is a volunteer project. There are no fixed response times.

## Scope

These items are in scope:

- A file written out of the folder that the user selected.
- A file replaced without permission from the user.
- A download shown as complete when its data is incorrect.
- A bypass of TLS validation or of the redirect rules.
- A leak of links, tokens or credentials into files, logs or messages.
- A web page that can control the browser extension or the native host.
- Code execution caused by data from a server.

These items are not in scope:

- A problem in a server that Quorix downloads from.
- A problem that needs an attacker who already controls the computer of the user.
- The content of the files that a user downloads.

## Security rules of the project

The design rules are in section 13 of [Quorix_SOP.md](Quorix_SOP.md).
The current protections are in the Security part of [README.md](README.md).
