# Working on Hajime OS

Instructions for any coding agent pointed at this repository: Antigravity,
Codex, Cursor, Claude Code. Read this before the first edit.

This is a FreeBSD server system written in Rust that replaces a Docker stack of
nineteen containers on a machine with 8 GB of memory. It runs, or will run, on
one physical server with no remote console. That last fact is the reason for
most of the rules below: a mistake here is not a failing test, it is a machine
nobody can reach.

## The one rule everything else follows

**Refuse rather than guess.** Every script in this tree stops and explains
instead of doing something plausible with missing information. An installer
that guesses a domain, a backup that reports success while a container was
switched off, a gateway that returns a message ID it never sent -- all three
existed here and all three were removed. If you cannot know something, say so
in the output and exit non-zero.

The corollary: never wrap a failure in `|| true` to keep the output green.

## Language

Prose in this repository is Arabic. `README.md`, `STATUS.md`, `decisions.md`
and every component README are Arabic and stay Arabic.

Code, comments, commit messages and command output are English. The machine
speaks English because its console cannot render Arabic -- `vt(4)` has no
bidirectional layout and no shaping -- and a system that prints broken Arabic to
prove it speaks Arabic is worse than one that prints plain English. Arabic
reaches users through the locale: `/etc/login.conf` classes, `Name[ar]` in
`.desktop` files, and the console's own language switch.

## Comments

Comments say **why**, never what. A comment restating the line above it is
noise; a comment naming the failure the line prevents is the most valuable
thing in the file.

```rust
// Running, not merely present. `docker inspect` succeeds for a stopped
// container, so the first version of this check waved postiz-postgres through
// and the dump died against a container that exists and is switched off.
```

Where a bug was fixed, the comment records what broke. Those are the comments
people read at three in the morning.

## Generated files are not edited

`hajime-brand/out/` is generated. Change `palette.toml`, `brand.toml` or the
code in `hajime-brand/tools/`, then run:

```bash
python hajime-brand/tools/emit.py
python hajime-brand/tools/emit.py --check   # what CI runs
```

The check compares text byte for byte in both directions -- missing files and
leftover ones. It does not compare the pictures, because their lettering is
rasterised by whatever FreeType the machine has.

The same rule applies to `/usr/local/etc/caddy/Caddyfile`, which comes from
`hajime-web/sites.conf`, and to `hajime-console/src/../out/brand.rs`, which the
console compiles in rather than keeping a second copy of the repository URL.

## One source, never two

Before adding a constant, search for it. The palette lived in three files once
and the desktop and the console started disagreeing about what red meant. The
launcher list lived in the artwork generator once and promised six applications
while the installer shipped three.

If a value has to appear in two places, one of them must be generated from the
other, and CI must diff them.

## Shell scripts

- POSIX `sh`, not bash, for anything that runs on the server. `hajime-migrate`
  is the exception and says `#!/usr/bin/env bash`.
- LF endings, always. FreeBSD's `sh` rejects a CRLF script with a syntax error
  naming the wrong line, Git Bash tolerates it, and CI checks for it because it
  has bitten twice.
- `sh -n` and `shellcheck` clean before committing.
- Anything that writes to a system file writes inside a marked block
  (`# --- BEGIN hajime ... --- END hajime ---`) so a second run replaces rather
  than stacks, and copies the original to `<file>.hajime-orig` once.
- `--dry-run` shows the **whole** plan. It counts blockers and keeps going;
  stopping at the first one hides the four behind it.

## rc.d

Every service in `hajime-sys/src/service.rs` whose name begins with `hajime`
must have an rc script in some `hajime-*/rc.d/` directory. A test enforces it.
This is not bureaucracy: the console and the WhatsApp bridge were both listed
as services, both installed as binaries, and neither had a script -- so the
message of the day pointed at a port where nothing listened.

## Tests

```bash
cargo test --workspace          # 404 tests, zero failures
cargo clippy --workspace --all-targets -- -D warnings
```

Test names are sentences describing the failure they prevent:
`a_dry_run_tool_call_is_distinguishable_from_a_real_one`. Follow that.

A test that only proves the happy path is half a test. Where something can be
absent, missing, stopped or malformed, test that case -- most of the bugs found
in this repository were in the absent branch.

## FreeBSD specifics you will get wrong from memory

- Package names are versioned and change: `php84`, `mariadb1011`,
  `postgresql17-server`. **Verify against the ports tree before writing one.**
  The FreeBSD CI job installs the exact list, so a rename fails there rather
  than halfway through an install on the server.
- `/tmp` on the production host is a 1 GB tmpfs. Staging anything large there
  spends memory on a machine that swaps.
- `service mysql` does not exist; the port installs `mysql-server` while the
  rcvar is `mysql_enable`. That is why `Service` has an `rc_script` field.
- The loader's graphics format is `gfx-<name>.lua` returning a table with
  `ascii` and `fb` keys, on FreeBSD 14.x. Validate with
  `/usr/libexec/flua -e 'assert(loadfile(...))'` before touching
  `/boot/loader.conf`: a syntax error there is a boot that stops at a traceback.

## Never do these

- Commit anything from `/vault/secrets`, `credentials`, `.env` files, tokens,
  or the contents of `C:\hajime-backups`.
- `git add -A`. It swept an unrelated directory into a commit once. Stage paths.
- Push to `main` directly. Branch, open a PR, let CI run.
- Invent a domain, a tunnel ID, a sponsor link or a password. Leave the field
  empty and make the surface that would have shown it drop the line.

## Where things are

| Path | What |
|---|---|
| `hajime-core` | auth, policy, history, secrets, HTTP scaffold, JS |
| `hajime-workflow` | the engine, 11 node types, scheduler, shadow mode |
| `hajime-sys` | `hajimectl`: services, snapshots, jails, readiness |
| `hajime-console` | the one page, localised, palette compiled in |
| `hajime-brand` | palette, identity, mascot, generator, theme installer |
| `hajime-web` | site table, Caddyfile generator, tunnel template |
| `hajime-wm` | wayfire and GTK |
| `hajime-migrate` | backup, restore, rehearsal |

`decisions.md` holds the reasoning behind every non-obvious choice, newest
last. Read the entry before changing what it decided, and add one when you
decide something a reader would otherwise question.

## graphify

This project has a knowledge graph at graphify-out/ with god nodes, community structure, and cross-file relationships.

When the user types `/graphify`, use the installed graphify skill or instructions before doing anything else.

Rules:
- For codebase questions, first run `graphify query "<question>"` when graphify-out/graph.json exists. Use `graphify path "<A>" "<B>"` for relationships and `graphify explain "<concept>"` for focused concepts. These return a scoped subgraph, usually much smaller than GRAPH_REPORT.md or raw grep output.
- Dirty graphify-out/ files are expected after hooks or incremental updates; dirty graph files are not a reason to skip graphify. Only skip graphify if the task is about stale or incorrect graph output, or the user explicitly says not to use it.
- If graphify-out/wiki/index.md exists, use it for broad navigation instead of raw source browsing.
- Read graphify-out/GRAPH_REPORT.md only for broad architecture review or when query/path/explain do not surface enough context.
- After modifying code, run `graphify update .` to keep the graph current (AST-only, no API cost).

@RTK.md
