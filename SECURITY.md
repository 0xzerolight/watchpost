# Security

## No authentication

**There is no authentication.** No login, no users, no API key: anyone who can reach the port gets
the whole app — every metric, plus write access to events, to the tracked-repo list and to the
sync button. Syncing spends the token's GitHub rate budget, so an open instance is also a way for
a stranger to exhaust it. The token itself is never rendered (`--doctor` prints its last 4
characters and length, nothing else), but everything it can read is on the page.

Both defaults keep the port private. Outside Docker `WATCHPOST_HOST` is `127.0.0.1`, and
`compose.yml` publishes to `127.0.0.1:8080`. The container itself listens on `0.0.0.0` — that is
what makes publishing work at all — but the host offers the port to the loopback and nowhere else,
so as shipped nothing on your network can reach it.

Widening that is one line, and worth a moment's thought first:

```yaml
ports:
  - "8080:8080"      # every interface on the host
```

Do that only behind a reverse proxy that does the authenticating. Otherwise leave it on the
loopback and reach it through an SSH tunnel. Do not expose it directly.

## Behind a TLS-terminating proxy

**Forward `X-Forwarded-Proto: https`.** The CSRF cookie is marked `Secure` only when that header
says the browser spoke HTTPS; setting it unconditionally would make the cookie invisible to a
plain-HTTP deployment and 403 every POST there. Only the first hop's value is read.

## What the token can reach

The token is never rendered in the UI, and `--doctor` prints only its last 4 characters and its
length. That protects the credential itself, not what it unlocks. Everything the token can read is
on the page, so the token's scope is the instance's blast radius — grant it the four read
permissions the README lists and nothing more.

## Backups

**Backups are taken on migration, not on a schedule.** Before a schema upgrade the database is
copied to `data/watchpost.v{schema}.{timestamp}.bak` and the newest three are kept. That copy goes
through SQLite's backup API, so it is consistent even if a write is in flight.

For backups of your own, copying `data/watchpost.db` is not enough. WAL mode means committed data
can still be sitting in the `watchpost.db-wal` sidecar, so a copy of the main file alone is stale at
best and torn at worst. Either stop the container first, or let SQLite take the snapshot while it
runs:

```sh
sqlite3 data/watchpost.db ".backup data/snapshot.db"
```

## Reporting a vulnerability

Report privately through GitHub's
[security advisories](https://github.com/0xzerolight/watchpost/security/advisories/new) rather than
opening a public issue.
