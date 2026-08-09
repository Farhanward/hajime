# hajime-wa bridge

Speaks WhatsApp's multi-device protocol through
[whatsmeow](https://github.com/tulir/whatsmeow) and exposes a loopback HTTP API
for `hajime-wa`.

## Why a second process

The device keys live here. Restarting the Rust API therefore never drops the
WhatsApp session or forces a fresh QR scan. It also keeps the only Go
dependency in the system contained to one binary.

## Build

Go is in the FreeBSD ports tree as `lang/go`.

```sh
pkg install go
cd hajime-wa/bridge
go mod tidy
go build -o hajime-wa-bridge .
```

## First run: pairing

```sh
HAJIME_WA_STORE=/vault/hajime/wa.db ./hajime-wa-bridge
```

It prints a QR code. Scan it from WhatsApp under *Linked devices*. The pairing
is stored in the database file and survives restarts.

## Settings

| Variable | Default | Meaning |
|---|---|---|
| `HAJIME_WA_BRIDGE_BIND` | `127.0.0.1:3001` | listen address |
| `HAJIME_WA_STORE` | `./hajime-wa.db` | session database |
| `HAJIME_WA_WEBHOOK` | none | where inbound messages are posted |

Bind to loopback only. The bridge has no authentication of its own: it is
reached through `hajime-wa`, which does.

## API

```
GET  /session/{name}   -> {"name","status","phone"}
POST /send/text        <- {"session","chatId","text"}
                       -> {"id","chat_id"}
GET  /health           -> {"status"}
```

`status` uses WAHA's vocabulary: `WORKING`, `SCAN_QR_CODE`, `STARTING`,
`FAILED`.

## Inbound events

Text messages are posted to `HAJIME_WA_WEBHOOK` in WAHA's shape:

```json
{
  "event": "message",
  "session": "default",
  "payload": {
    "id": "...", "from": "9665...@c.us",
    "fromMe": false, "body": "...", "timestamp": 1754000000
  }
}
```

The existing `WAHA Auto-Reply + Forward` workflow reads exactly `event`,
`session`, `payload.from`, `payload.fromMe` and `payload.body`. Those names are
fixed: renaming one breaks that workflow without any error.

## Status

**Built and smoke-tested on FreeBSD 14.4** (2026-08-04).

```
hajime-wa-bridge: ELF 64-bit LSB executable, x86-64, for FreeBSD 14.4
23M

GET /health          -> {"status":"SCAN_QR_CODE"}
GET /session/default -> {"name":"default","status":"SCAN_QR_CODE"}
```

It starts, serves both endpoints and prints a pairing QR code. `SCAN_QR_CODE`
is the correct state for a store with no paired device.

Two things had to be fixed to get here, and neither was visible from the
development machine because Go was not installed there:

1. The hand-written `go.mod` pinned a whatsmeow pseudo-version that does not
   exist. Module versions are now resolved by `go get` and committed.
2. whatsmeow's API had moved on: `sqlstore.New` and `container.GetFirstDevice`
   both take a `context.Context` now.


## Terms of service

whatsmeow is an unofficial, reverse-engineered client. Using it can violate
WhatsApp's terms and accounts have been banned for it. The same risk applies to
WAHA, which this replaces, so the exposure is unchanged rather than new.

## Build dependency note (verified on FreeBSD 14.4)

`go.mod` originally pinned a pseudo-version of whatsmeow that does not exist.
Resolve real versions with `go get` rather than hand-writing them:

```sh
go get go.mau.fi/whatsmeow@latest
go get github.com/mattn/go-sqlite3@latest
go get github.com/mdp/qrterminal/v3@latest
go mod tidy
```
