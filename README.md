# neochumby

New software for the original Chumby (the Chumby Classic, 2008): apps in
Rust, booted from a stick or over the network, with the Chumby's own
firmware left as it is.

Nothing is flashed. The stock boot script runs a hook from a stick or from
the Chumby's writable settings partition, and the hook runs neochumby from
the stick or from an NFS export. **CHUMBY MODE** in the app list hands
the Chumby back to its stock software, and a reboot brings neochumby back.
Every binary is paged in as it runs, so apps cost the Chumby nothing until
opened.

An independent project, not affiliated with or endorsed by Chumby
Industries, Blue Octy or anyone holding rights in Chumby; "Chumby" is a
trademark of its owner. None of the Chumby's own software is distributed
here: see [docs/legal.md](docs/legal.md).

## The hardware

Freescale i.MX21: an ARM926EJ-S at about 266 MHz, with no FPU and 64 MB of
RAM. The screen is 320x240 RGB565 (`/dev/fb0`), with a resistive touchscreen
(`/dev/input/event0`), a squeeze switch in the case (`/dev/switch`) and an
accelerometer (`/dev/accel`). It runs Linux 2.6.16 with glibc 2.8. Our
binaries are static (musl), so they don't depend on its userland.

## What's here

| Path | What | Licence |
| --- | --- | --- |
| `crates/neochumby` | The session (`neochumby`): runs one app at a time in front, starting with the start app, and the apps' services. And the library every app uses: screen, touch, squeeze, accelerometer, a terminal for programs written for one, the apps' manifests, the request pipe, crash reports and a profiler | MIT |
| `crates/neochumby-softfloat` | libgcc's hand-written ARM soft float, which the library links into every app: float maths about twice as fast as the generic routines | GPL-3.0 with the GCC Runtime Library Exception |
| `crates/neochumby-proto` | How a Chumby and neochumbyd talk: TLS 1.3 with pinned keys, or Noise as a build option | MIT |
| `crates/neochumby-sync` | `delivery=sync`: the rounds that keep a copy of a Chumby's tree on its stick, both ends, and the checks the boot hook relies on. The receiver, `neochumby-sync`, is a binary of `crates/neochumby` | MIT |
| `crates/neochumby-ncurses` | [ncurses](https://invisible-island.net/ncurses/) (6.6, a submodule), built static with the terminal's xterm-256color compiled in, for the apps whose programs use curses | MIT, and ncurses' own X11-style licence |
| `crates/neochumbyd` | The server a Chumby pairs with: its apps over NFS or synced to its stick, pairing, and the apps' server halves | MIT |
| `apps/home` | neochumby-home, the app list: the installed apps, pairing, the time, CHUMBY MODE | MIT |
| `apps/airplay` | AIRPLAY: a receiver per Chumby on neochumbyd, played on the Chumby's speaker, with a now-playing screen (the track, its cover, progress, previous/play/pause/next) | MIT |
| `apps/logs` | LOGS: the Chumby's logs (the session's, the kernel's, /tmp and /var/log), and with a neochumbyd the host's, followed and scrolled by touch | MIT |
| `apps/doom` | Doom: [doomgeneric](https://github.com/ozkl/doomgeneric) (e257a095, a submodule) with a Rust platform layer | GPL-2.0 |
| `apps/fetch` | neochumby-fetch: [fastfetch](https://github.com/fastfetch-cli/fastfetch) (2.70.0, a submodule) on the library's terminal, beside a Chumby | MIT |
| `apps/htop` | neochumby-htop: [htop](https://github.com/htop-dev/htop) (3.5.3, a submodule) on the library's terminal, the touchscreen its mouse (F10, Quit, is past the 64th column: the corner hold leaves it) | GPL-2.0-or-later |
| `apps/top` | neochumby-top: [procps-ng](https://gitlab.com/procps-ng/procps)' top (4.0.7, a submodule) on the library's terminal, until tapped | GPL-2.0-or-later |
| `apps/cmatrix` | MATRIX: [cmatrix](https://github.com/abishekvashok/cmatrix) (v2.0, a submodule) on the library's terminal, until tapped | GPL-3.0-or-later |
| `apps/clock` | CLOCK: [tty-clock](https://github.com/xorg62/tty-clock) (8868f3e, a submodule), until tapped | BSD-3-Clause |
| `apps/bonsai` | BONSAI: [cbonsai](https://gitlab.com/jallbrit/cbonsai) (v1.4.2, a submodule) growing a tree, a new one every 20 seconds, until tapped | GPL-3.0-only |
| `apps/pipes` | PIPES: [pipes-rs](https://github.com/dnorhoj/pipes-rs) (v0.1.1, a submodule), pipes.sh in Rust, until tapped | GPL-3.0-only |
| `apps/aquarium` | AQUARIUM: [asciiquarium-rs](https://github.com/cablehead/asciiquarium-rs) (beef5b7, a submodule), asciiquarium in Rust, until tapped | GPL-2.0-or-later |
| `apps/genact` | GENACT: [genact](https://github.com/svenstaro/genact) (1.6.0, from crates.io), looking busy, until tapped | MIT |
| `apps/quake` | neochumby-quake: id's WinQuake software renderer ([id-Software/Quake](https://github.com/id-Software/Quake), a submodule), made fast on the ARM926 (below) | GPL-2.0-or-later |
| `boot/userhook2` | The boot hook | MIT |
| `tools/` | The cross-compiling wrappers and `deploy` | MIT |
| `dev/` | Device-side test helpers: framebuffer capture to PNG, touch injection, mount test | MIT |
| `emu/` | The emulator: QEMU's `chumby-classic` machine, from [qemu-chumby](https://github.com/johnmerchant/qemu-chumby) at a pinned commit, which boots the stock firmware from your own flash backup; and chumby-emu, a host tool that builds its images | MIT (the machine: GPL-2.0-or-later, like QEMU) |

Everything we write is MIT, except in `apps/doom`, `apps/quake`,
`apps/htop`, `apps/top`, `apps/cmatrix`, `apps/bonsai`, `apps/pipes` and
`apps/aquarium`. They link GPL programs (the games' engines, htop, procps'
top, cmatrix, cbonsai, pipes-rs, asciiquarium-rs), so each of those crates, our Rust in it included, is
GPL and lives in its own directory, with the program's upstream licence
in its submodule (`apps/quake/src/span.rs` is a port of
the engine's span drawer, so it could be nothing else). `apps/fetch` links
fastfetch, which is MIT as we are (its `LICENSE` is in its submodule), and
`crates/neochumby-ncurses` ncurses, whose licence is MIT's in other words.
Nothing links an app: each runs as a separate
program. The
soft-float routines' runtime exception puts no conditions on the programs
they're linked into.

## Apps

Everything you see is an app, the app list included. An app is one crate,
`neochumby-<name>`, which installs as a directory `apps/<name>/` in the
tree, holding its binaries and a `manifest`. The manifest comes from the
crate's `Cargo.toml`:

```toml
[package.metadata.neochumby]
title = "QUAKE"                      # listed in app lists
exec = "neochumby-quake"             # binaries, by name; exec runs in front
colour = "8c5a28"                    # optional
service = "neochumby-airplay"        # runs in the background, restarted if it exits
companion = "neochumby-airplay-companion"   # its server half, run by neochumbyd
neochumbyd = "required"              # or "optional"; absent: never needed
start = true                         # it may be the start app
interrupt = false                    # no service may replace it in front
button = true                        # it gets the top button while in front
access = ["screen", "touch", "speaker", "network"]   # what it reaches beyond its jail
```

An app can use any of these keys:
- **`neochumbyd`:** Doom and Quake don't need a neochumbyd, so they run from
  a stick on a Chumby nobody paired. AirPlay is `required`: on an unpaired
  Chumby it's listed greyed out and its service doesn't start.
- **`companion`:** an app with a server half ships it as a second binary.
  neochumbyd runs it for each Chumby that asks, and relays frames between
  the two halves over the paired connection. neochumbyd knows no app by
  name. An app with a companion sets `neochumbyd` too, and its Chumby half
  opens it with `neochumby::companion!()`, which fails the build if the
  app's `Cargo.toml` declares no companion.
- **`exec`:** what runs, full-screen, when the app is opened. The session
  (`neochumby`) runs one app at a time in front. An app opens another, or
  CHUMBY MODE, by asking the session over its request pipe
  (`NEOCHUMBY_SESSION`) and exiting. When an app exits, the session goes
  back to the start app. [CONTRIBUTING](CONTRIBUTING.md#the-app-contract)
  has the details.
- **`start`:** the start app is the one a Chumby opens first and comes back
  to. It's named by `start=<app>` in this Chumby's settings from its
  neochumbyd (`neochumbyd set NAME start=<app>`, kept in
  `/psp/neochumby.settings`), else in `/psp/neochumby.conf`, or else it's
  `home` (neochumby-home). Only an app with `start = true` may be the start
  app; the session refuses any other, logs why, and uses `home`. `neochumbyd pairing`
  names the start apps in the tree, and suggests one to a Chumby whose
  `start` isn't set; `neochumbyd set NAME start=<app>` warns about a name
  that isn't one. **Holding the top-left
  corner of the screen for 3 seconds** always opens `neochumby-home`,
  ending whatever was in front. That's the way out of an app gone wrong.
- **`service`:** runs in the background for as long as neochumby does, and
  is started again if it exits. It's told which app is in front. Any
  service may open an app over the start app, and the start app's service
  over any app, unless the one in front says **`interrupt = false`**, as
  Doom and Quake do.
- **`button`:** the session keeps the top button (`/dev/switch`, which only
  one process can have open). A press returns to the start app, as on the
  stock Chumby, unless the app in front says `button = true`: then it has
  the button. Doom and Quake fire with it. With the start app in front, a
  press is a `button` line on its pipe.
- **`access`:** each app runs as a user of its own, jailed in a directory
  holding only its own files, a `/tmp` of its own and what `access` lists:
  `screen`, `touch`, `speaker`, `accel`, `system` (`/proc`, `/sys`, `/etc`)
  `network` and `terminal`. It never sees the Chumby's key. `root`, listed alone, runs
  it unjailed, as root: only `home` does, as it pairs the Chumby.
  [SECURITY](SECURITY.md) says what that protects, and what it doesn't.

## Building

You'll need Rust with the `armv5te-unknown-linux-musleabi` target and
[zig](https://ziglang.org) on `PATH`. zig is the C compiler and linker, and
supplies musl (`tools/zigcc`).

Third-party source (Doom, Quake, fetch's fastfetch, htop, procps,
ncurses) is in git submodules, and fetch builds fastfetch with
[CMake](https://cmake.org) 3.21 or later. ncurses is built with its own
configure and make, which need only a shell, `make`, `awk` and a host C
compiler. `tools/tree`, `tools/stick` and `tools/deploy` check out
the submodules of the apps they build; for `cargo` on its own, check them
out first:

```sh
git submodule update --init   # or clone with --recursive
rustup target add armv5te-unknown-linux-musleabi x86_64-unknown-linux-musl
cargo chumby          # the session and apps, for the Chumby
cargo neochumbyd      # the server and the apps' companions, for x86_64 Linux
```

A Chumby and its neochumbyd talk TLS 1.3. To use Noise instead, build both
with `--features neochumby-proto/noise`. Nothing is negotiated, so the two
ends must be built the same way.

## Running it

A Chumby runs neochumby from a tree of the session and its apps:

```
neochumby                  the session
neochumby-sync             the receiver for delivery=sync (not on a stick made by hand)
userhook2                  the boot hook, which pairing installs
apps/home/                 neochumby-home, manifest
apps/doom/                 neochumby-doom, manifest, doom1.wad
apps/quake/                neochumby-quake, manifest, id1/pak0.pak, id1/autoexec.cfg
apps/airplay/              neochumby-airplay, neochumby-airplay-screen, neochumby-airplay-companion, manifest
apps/logs/                 neochumby-logs, neochumby-logs-companion, manifest
apps/fetch/                neochumby-fetch, manifest, fastfetch.jsonc, chumby.txt
apps/htop/                 neochumby-htop, manifest
apps/top/                  neochumby-top, manifest
apps/cmatrix/              neochumby-cmatrix, manifest (and so on for clock, bonsai, pipes, aquarium, genact)
```

`tools/tree OUT [-d DATA] [APP ...]` builds that tree. It always includes
the session and neochumby-home, plus the apps you name, from wherever
they are:

| APP | From |
| --- | --- |
| `doom` | `apps/doom` in this repository |
| `./path`, `/path` | a crate on this machine |
| `git+https://github.com/you/neochumby-x#REV` | a git repository with the crate at its root, at commit REV (unpinned, it prints the commit to pin) |
| `crate:neochumby-x@1.0.0` | crates.io (unversioned, the newest) |

Each app's binaries are built for the Chumby, and its companion for the
server, with the toolchain above. Its manifest is made from its
`Cargo.toml`, and its `files/` directory, if it has one, is copied as it
is. Game data is added from `DATA`, your own copies laid out the same way.
The tree can come from a stick or from a server.

### From a stick

```sh
tools/stick /Volumes/STICK [-d DATA] [APP ...]
```

Any FAT-formatted USB stick works. Plug it into a Chumby and power on:
neochumby boots with the stick's apps. **Nothing on the Chumby changes**:
its stock boot runs the stick's `userhook2`. Without the stick it boots
as it always did.

### From a server: neochumbyd

neochumbyd serves the tree read-only over NFSv3, with NFS and MOUNT on
one TCP port. It also pairs Chumbys and runs their services:

```sh
neochumbyd serve --state /var/lib/neochumbyd --shairport $(command -v shairport-sync)
```

`tools/deploy HOST [APP ...]` installs the tree and neochumbyd in the server's
state directory (`/var/lib/neochumbyd`), leaving game data alone.
BusyBox's `mount` asks the port mapper on port 111 even when given the
ports, so the server should refuse port 111 rather than drop it. Otherwise
the mount hangs.

**Pairing** makes a Chumby boot from neochumbyd with no stick:

1. Put a `neochumby.conf` holding `server=<neochumbyd's address>` on a
   neochumby stick, and boot the Chumby from it.
2. The app list shows **PAIR**. Tap it: the Chumby shows a 6-digit code.
3. On the server, `neochumbyd pairing` lists the request with the same
   code. If it matches, run `neochumbyd approve <code>`.
4. The Chumby writes its key, the server's key and the boot hook to
   `/psp`. Remove the stick and restart.

The code comes from the connection itself, so it matches on both screens
only if nothing sits between them. After pairing, each end checks the
other's key on every connection.

**AirPlay** (the airplay app): while a paired Chumby is connected, its
companion on neochumbyd runs an AirPlay receiver named after it ("Kitchen
Chumby"), and the Chumby plays the audio. The host provides
`shairport-sync` on `PATH`, avahi, and the base of eighty ports as
`NEOCHUMBY_AIRPLAY_PORT` (default 7350): RTSP from the base, UDP from
base + 10. Each Chumby keeps the same ports.

AIRPLAY's screen shows what's playing: the title, artist, album and cover
art, where the track has got to, and previous, play/pause and next. A
stream beginning opens it over the start app; otherwise it shows "Ready for
AirPlay" and the name to choose. For that, shairport-sync must be built
with metadata (`--with-metadata`, or `--with-metadata-pipe` from 5.0); the
companion turns its metadata pipe on and scales the cover art for the
Chumby. Without it, AirPlay plays as before, with no details. The buttons
go to the phone or Mac as DACP remote-control requests. Their port comes
from shairport-sync when it's built with a remote-control interface
(D-Bus, MPRIS or MQTT), else from avahi's `avahi-browse` (avahi-utils).
Senders that offer no remote control get dimmed buttons. Text
outside ISO 8859-1 (Japanese, emoji) shows as `?`: the screen's fonts are
Latin-1.

**Logs** (the logs app): the Chumby's own logs first, then, on a paired
Chumby, the host's, from its companion. ◀ and ▶ along the top go from
log to log; dragging scrolls, and at the end it follows. The host's are
whatever its owner lists in `<state>/apps/logs/sources` (or
`sources.NAME` for one Chumby), one a line, a file or a command:

```
syslog=/var/log/syslog
web=|tail -n 200 -f /srv/web/access.log
```

Without that file it serves /var/log/syslog or /var/log/messages, and the
journal where `journalctl` runs.

### Delivery: NFS or sync

Each paired Chumby gets its tree one of two ways, chosen on the server:

```sh
neochumbyd set kitchen delivery=sync   # or nfs, the default
```

- **`nfs`:** the boot hook mounts the tree from neochumbyd on every boot.
- **`sync`:** the Chumby keeps a whole copy of its tree on its USB stick,
  in step with neochumbyd, and boots from it. A Chumby on sync boots
  without the server, and loads from the stick rather than the network.
  Plug in any FAT stick; the first sync fills it.

Sync works like an A/B update. The stick's `neochumby/` holds two whole
sets, `a/` and `b/`, and `current` names the live one. While its settings
say sync, the session runs `neochumby-sync` as a service of its own,
restarted like the apps' services but never part of the interruption
rules (it opens no app). Once a minute, it:

1. sends neochumbyd the digest of its live set's manifest. If it matches
   the tree, nothing is written;
2. otherwise sends the standby set's SHA-256 manifest, and neochumbyd sends
   only the files that differ, whole;
3. writes them into the standby set only, each under a temporary name,
   hashed as it streams, and renamed into place. Files the tree no longer
   has are removed. Files are synced in batches, and each batch is listed
   in the set's journal only after it is on the stick;
4. hashes any file kept from a round that was cut short, then writes the
   set's completion marker (`a.complete`, carrying its manifest) last, and
   rewrites `current` to make it live from the next boot.

Files arrive only over the paired session, from the pinned neochumbyd key,
never SSH. Writes stay inside the stick's `neochumby/`, and happen only
while a stick is mounted at `/mnt/usb`. No `..`, absolute paths, symbolic
links or names FAT refuses (`con`, `aux.txt`), at most 64 MB a file and 512
MB a set, and only if the stick has room. A round has an hour in all. The
live set is never written. A rename on FAT can tear in a power cut, so the
boot hook doesn't trust `current` alone. It runs the set `current` names
only if that set's marker is whole and every file it lists is there at its
size (`neochumby check-set`, 30 seconds at most for both sets), else the
other set, else it boots as before. A power cut at any point leaves a
complete set to boot. The session keeps the boot hook in `/psp` up to date
with the tree's, so Chumbys paired before sync boot their sets too.

On the Chumby (with SSH enabled by its owner):

```sh
neochumby-sync status     # the sets, which is live
neochumby-sync rollback   # the other set is live from the next boot; syncing holds
neochumby-sync resume     # sync again
```

A Chumby on sync is sent the tree NFS serves. `neochumbyd manifest`
prints it, with anything a Chumby couldn't take. Files for one Chumby only
(a renderer's output, say) would be laid over the tree where neochumbyd's
`sync.rs` scans it; that isn't built yet.

**Developing on a paired Chumby** needs no shell on it. Its session
keeps a connection to neochumbyd, the developer link, over the same
pinned-key session as everything else:

```sh
neochumbyd log den [-f]          # its log: the session's, its apps' and services' output
neochumbyd restart den           # end its apps and start its session again from the tree
neochumbyd shot den den.png      # its screen
tools/deploy HOST doom --restart den    # deploy, then restart den on the new tree
```

Once its owner allows it (`neochumbyd set den dev=on`; off by default),
the same link also takes input, so the scenarios that drive the emulators
can drive a real Chumby:

```sh
neochumbyd tap den 160 120 [--hold MS]     # touch the screen there (pixels)
neochumbyd button den press|release|tap    # the top button
```

neochumbyd keeps the last 2000 lines of each Chumby's log in its state
directory (`logs/`), and limits what a Chumby may send. The commands work
through the state directory as `approve` does, so they need no port of
their own. A restart ends the app in front and the services as CHUMBY
MODE does, then runs the tree's `neochumby` again in the same process.

`neochumbyd mcp` serves the same as an [MCP](https://modelcontextprotocol.io)
server on stdin and stdout, so Claude Code (or any MCP client) can read a
Chumby's log, restart it and look at its screen while working on an app.
Its tools are `chumbys` (the pairing list and settings), `log`, `restart`,
`screenshot` and `set`. Pairing itself is left to a person. Run it on the
server, over SSH from wherever the client is:

```sh
claude mcp add neochumby -- ssh HOST neochumbyd mcp
```

When a stick and a server are both available, the stick wins. The hook
times out every step that could block, and returns to the stock boot if
anything fails. To skip it, create `/psp/neochumby-off`, or put a file
named `neochumby-off` on a stick. neochumby never starts SSH; enable it
yourself from the Chumby's settings (Chumby info → π) when you need it.

## The emulator

QEMU's `chumby-classic` machine runs a Chumby without the device: the
stock kernel, root disk and settings partition from your own flash
backup, unmodified, to the stock control panel, with the screen,
touchscreen, serial console and flash working. The machine lives in
[qemu-chumby](https://github.com/johnmerchant/qemu-chumby) (patches
against a pinned QEMU release); `emu/` builds it at a pinned commit and
makes the images. [`emu/README.md`](emu/README.md) has the details.

```sh
emu/qemu/build                       # QEMU with the machine (deps: emu/README.md)
(cd emu && cargo build --release)    # chumby-emu, the image tools
emu/target/release/chumby-emu mkpsp PSPDIR psp.jffs2
emu/target/release/chumby-emu mknand BACKUPDIR psp.jffs2 nand.img
emu/qemu/run zImage nand.img         # console on stdio, QMP on qmp.sock
emu/qemu/qemu-chumby/chumby-ctl qmp.sock tap 160 120      # touch at a pixel
emu/qemu/qemu-chumby/chumby-ctl qmp.sock shot screen.png  # screenshot
```

The NAND image is written as the Chumby writes its flash; add `-snapshot`
to keep it unchanged, and `-icount shift=auto,sleep=off` to boot in a few
seconds rather than in real time.

## Game data

None is included, and none may be redistributed with it. Use the shareware
episodes:

- Doom: `doom1.wad` (shareware v1.9) in `apps/doom/`. [Freedoom](https://freedoom.github.io) works too.
- Quake: `id1/pak0.pak` from the shareware `quake106` release, in `apps/quake/`.

## neochumby-quake

Quake's renderer is floating point throughout, and the ARM926 has no FPU.
Here's the work so far, measured with `timedemo demo1` at 320x240 on a
Chumby Classic:

| Step | fps |
| --- | --- |
| WinQuake's portable C paths, with the Chumby platform layer | 6.0 |
| libgcc's hand-written ARM soft float in place of the generic routines | 12.1 |
| Fixed-point span drawers in Rust, one fixed-point plane per surface | 15.6 |

`apps/quake/tools/bench` runs the timedemo on the device. Any app can be
profiled: run it with `NEOCHUMBY_PROF=file` and read the file with
`tools/profile file neochumby-quake`. The profile is read against the
unstripped binary, and soft-float time is charged to its callers.
