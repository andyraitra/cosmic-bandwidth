# Security policy

## Reporting a vulnerability

Please do not open a public issue. Report it privately through
[GitHub private vulnerability reporting](https://github.com/andyraitra/cosmic-bandwidth/security/advisories/new).

You should get an answer within a week. Only the latest release is supported.

## Scope

The applet runs as the desktop user, reads `/sys/class/net`, and writes only under
`~/.local/state/cosmic-bandwidth` and `~/.config/cosmic`. Issues that let a value from those
files escape that scope, corrupt or inflate the saved usage, or crash the applet are in scope.

Known advisories in transitive dependencies (pulled in by libcosmic) are tracked, with their
reasons, in [`osv-scanner.toml`](osv-scanner.toml).
