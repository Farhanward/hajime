# Hajime OS

A FreeBSD server system, written in Rust, for people whose Docker stack has
outgrown the machine it runs on.

It is not a distribution and not a container runtime. It is a set of native
services, a workflow engine that reads n8n's export format, and an installer
that turns a plain FreeBSD box into something that serves websites, runs
automations, and can be rolled back with one command.

## The problem it was built for

A small server was running nineteen containers on eight gigabytes of memory:
an automation engine, a shop, a reverse proxy, a WhatsApp gateway, a local
language model, and the databases behind them. It worked until it did not.
Containers began dying with SIGKILL under memory pressure, several of them at
once, and the thing that noticed was a customer.

The usual advice is to buy more memory. This is what the other answer looks
like: remove the layer that costs the most and gives the least on a machine
this size, and replace each container with the native service that was
underneath it all along.

That is the whole idea. Docker earns its overhead on a fleet. On one box, in
one room, with one administrator, it is mostly a tax.

## What replaces what

| Container | Native replacement |
|---|---|
| n8n | `hajime-workflow`, which reads n8n's own export format |
| nginx-proxy-manager | Caddy, generated from a single site table |
| Ollama | `llama.cpp` under `hajime-ai` |
| A Node control panel | `hajime-console`, one page, compiled in |
| MariaDB / PostgreSQL / Redis containers | the FreeBSD packages |
| Docker restart policies | `rc.d` and `hajimectl` |
| Image rollback | ZFS boot environments |

## What it is made of

Nine Rust crates, about seventeen thousand lines, MIT licensed.

| Crate | What it does |
|---|---|
| `hajime-core` | auth, policy, history, secret storage, the HTTP scaffold |
| `hajime-workflow` | the engine: eleven node types, a scheduler, a shadow mode |
| `hajime-sys` | `hajimectl` — services, snapshots, jails, readiness |
| `hajime-console` | the single administration page, localised, palette compiled in |
| `hajime-ai` | a local model behind an HTTP interface |
| `hajime-model` | a small predictor for what the machine is doing |
| `hajime-fetch` | fetching, with the retry and timeout policy in one place |
| `hajime-wa` | a WhatsApp bridge |
| `hajime-social` | scheduled posting |

Plus the parts that are not Rust: `hajime-brand` generates the whole visual
identity from two TOML files, `hajime-wm` sets up a Wayfire desktop, `hajime-web`
turns a site table into a Caddyfile, and `hajime-jails` puts services in jails on
their own datasets.

## The workflow engine

This is the part that decides whether a migration off n8n is a weekend or a
quarter.

`hajime-workflow` reads n8n's export JSON directly. Not a converter, not a
best-effort import: the same file n8n writes is the file the engine loads. It
implements eleven node types — `webhook`, `scheduleTrigger`, `manualTrigger`,
`executeWorkflowTrigger`, `httpRequest`, `code`, `executeCommand`, `ssh`,
`readWriteFile`, `rssFeedRead`, `respondToWebhook` — and validates a workflow
file before running any of it:

```sh
HAJIME_WORKFLOWS=/path/to/workflows.json hajime-workflow --validate
```

Against a real production export of seventeen workflows, all seventeen parse and
every node kind in the file is one the engine implements. Your export may use
node types this does not have; `--validate` tells you which, before you commit
to anything.

## Rolling back

ZFS boot environments, not snapshots of a container image:

```sh
bectl list
bectl activate <environment> && reboot
```

The installer creates one before it changes anything, and prints how to activate
it if the first reboot goes wrong. This is the FreeBSD feature that makes the
whole approach defensible: a system-wide change that can be undone from the boot
menu is a change you can make on a Friday.

## The rule the code follows

**Refuse rather than guess.** Every script here stops and explains instead of
doing something plausible with missing information.

An installer that invents a domain, a backup that reports success while a
container was switched off, a gateway that returns a message ID it never sent —
all three existed in this project and all three were removed. If something
cannot be known, the output says so and the exit code is non-zero. Nothing is
wrapped in `|| true` to keep a log green.

The same rule shows up in small places. The Caddyfile generator refuses an empty
site table, refuses a document root that does not exist and names the line
number, refuses a PHP site when php-fpm is not installed, and lets Caddy itself
be the final judge — if `caddy validate` rejects the output, the previous file is
put back.

## One source, never two

The colour palette lived in three files once, and the desktop and the console
started disagreeing about what red meant. Now `palette.toml` and `brand.toml`
generate the CSS, the GTK theme, the loader's Lua graphics, the sixteen console
colour slots, the ANSI art, and the Rust constants the console compiles in. CI
regenerates everything into a temporary directory and diffs it byte for byte, in
both directions, so a hand-edited generated file fails the build.

The launcher artwork used to promise six applications while the installer
shipped three. Now the artwork generator reads the `.desktop` files, so it cannot
draw an icon for something that is not installed.

## Installing

Requires FreeBSD 14.4 on ZFS. The installer refuses UFS, because the rollback
story is boot environments and there is no point pretending otherwise.

```sh
sh install_hajime_os.sh --dry-run   # the whole plan, every blocker
sh install_hajime_os.sh
```

The dry run counts blockers and keeps going rather than stopping at the first
one, because stopping hides the four behind it.

Two further layers, both optional and neither needed to serve a site:

```sh
sh hajime-jails/create_jails.sh     # services on their own datasets
sh hajime-wm/install_desktop.sh     # Wayfire, the pixel theme, a browser
```

## The desktop

Optional, and unusual for a server: a Wayfire session themed as pixel art, with
the palette generated from the same two files as everything else. It exists
because the machine sits in a room with a monitor, and a server you can walk up
to should not greet you with a login prompt and nothing else.

It ships a file manager, a terminal, an image viewer, and `badwolf` — a minimal
WebKitGTK browser, chosen because the console needs a real JavaScript engine and
a full Chromium is not something to spend memory on here.

## Localisation

Arabic and English, done the way an operating system does it rather than by
translating strings in one place.

The kernel console is the constraint: `vt(4)` has no bidirectional layout and no
shaping, so it cannot render Arabic and a system that prints broken Arabic to
prove it speaks Arabic is worse than one that prints plain English. So the
machine speaks English at the console, and Arabic reaches users through the
locale: login classes in `/etc/login.conf`, `Name[ar]` in the `.desktop` files,
and the console's own language switch, which resolves from a query parameter and
then `Accept-Language`.

## Testing

```sh
cargo test --workspace          # 409 tests
cargo clippy --workspace --all-targets -- -D warnings
```

CI runs the suite on Linux and then again inside a real FreeBSD 14.4 virtual
machine with network access, where it installs every package the installer
manages, starts PostgreSQL, MariaDB and Redis and puts a real query through
each, serves a page through Caddy, generates a Caddyfile and validates it, and
starts the workflow engine as an rc service.

That job exists because a package rename in the ports tree should fail in CI
rather than halfway through an install on a machine nobody can reach.

Test names are sentences describing the failure they prevent:
`a_dry_run_tool_call_is_distinguishable_from_a_real_one`.

## What is proven, and what is not

The project's own rule applies to its README.

**Proven:** the test suite; every managed package installing on FreeBSD 14.4; the
databases starting and answering queries; Caddy serving a generated
configuration; the workflow engine parsing a real seventeen-workflow n8n export;
the theme installing on real FreeBSD and reaching the kernel, verified by reading
`kenv` rather than by reading back the file that was written.

**Not proven:** a complete install on physical hardware, end to end, has not been
performed. The console and the desktop have been built and packaged but never
run on a real machine. The database restore path has never been executed against
real dumps.

If you are considering this for something you care about, that list is the honest
answer to "is it ready", and it is short on purpose.

## Licence

MIT.
