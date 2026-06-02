# Security

## Threat Model

`dtch` protects sessions from other local user IDs on the same host. Sessions
are not isolated from processes already running as the same user. A process
with the same effective UID can attach to a session, send terminal input, and
read terminal output.

## Session Sockets

Session sockets are stored in `/tmp/dtch-<uid>/<name>.sock`. The per-user
directory is validated as a real directory owned by the current effective UID
and restricted to mode `0700`. Each socket is restricted to mode `0600`.

Clients and servers also verify the connected Unix socket peer UID. This
prevents a socket served by another local user from being accepted even if a
filesystem permission check is bypassed or behaves differently across Unix
platforms.

## Reporting

Please report suspected vulnerabilities privately to the repository owner
before opening a public issue.
