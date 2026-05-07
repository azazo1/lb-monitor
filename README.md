# Leaderboard Monitor

For [https://dataagent.top/leaderboard](https://dataagent.top/leaderboard).

The service keeps SQLite as the source of truth, exposes a read-only JSON API, and can send email when the leaderboard changes.

By default, config is read from `~/.config/lbm/config.toml`. Use `--config <path>` to override it. Path values inside config support `shellexpand`, such as `~`, `$HOME`, and `${VAR:-default}`.

Config is grouped by command:

```toml
[database]
path = "lb-monitor.sqlite3"

[serve.fetch]
url = "https://dataagent.top/leaderboard"
interval_seconds = 300

[serve.http]
listen = "127.0.0.1:8080"

[serve.mail]
enabled = false

[serve.mail.smtp]
host = "smtp.example.com"
port = 587
username = "user@example.com"
password = "secret"
from = "sender@example.com"
to = ["alpha@example.com", "beta@example.com"]
security = "starttls"

[tui]
refresh_seconds = 5
source = "sqlite"
# database_path = "lb-monitor.sqlite3"
# api_base_url = "https://example.com"
```

## Serve

Run the fetch loop, persist snapshots into SQLite, and expose the local API:

```bash
lb-monitor serve
```

Useful flags:

```bash
lb-monitor serve --listen 127.0.0.1:8080
lb-monitor serve --interval 300
lb-monitor serve --once
```

`--once` fetches and stores one snapshot, then exits. It does not keep the API server running.

## TUI

Default mode keeps the original local SQLite read path:

```bash
lb-monitor
lb-monitor tui --source sqlite --db ./lb-monitor.sqlite3
```

Remote mode reads from an HTTP or HTTPS API base URL:

```bash
lb-monitor tui --source remote-api --api-base-url http://127.0.0.1:8080
lb-monitor tui --source remote-api --api-base-url https://example.com
```

If `--api-base-url` is set without `--source`, the TUI switches to remote API mode automatically.

## API

The service exposes these read-only endpoints:

```text
GET /api/v1/state
GET /api/v1/snapshot
GET /api/v1/leaderboard
GET /api/v1/events?limit=100&team=<team_id>
GET /api/v1/chart?team_ids=<team_a>,<team_b>
```

## Mail Notifications

When mail is enabled, the service sends one initial snapshot email after first persistence, and sends update emails for later leaderboard changes. All configured recipients receive the same message.

CLI overrides:

```bash
lb-monitor serve \
  --notify \
  --smtp-host smtp.example.com \
  --smtp-port 587 \
  --smtp-username user \
  --smtp-password secret \
  --smtp-from sender@example.com \
  --smtp-to alpha@example.com,beta@example.com \
  --smtp-security starttls
```

Supported SMTP security modes:

```text
plain
starttls
tls
```
