# Contributing to neochumby

Thanks for helping bring the original Chumby back. A few things make
changes easy to take.

## Ground rules

- **Rust** for everything we write. Third-party C (Doom's doomgeneric,
  Quake's engine, fetch's fastfetch, htop, procps, ncurses) is a git
  submodule, pinned to an
  upstream commit and left unchanged, and linked from a Rust crate. Where
  an app needs it to do otherwise, its `build.rs` builds that file with a
  function renamed, so that the app's own takes its place, and says why
  (`apps/quake/build.rs`). A change that can't be made that way goes in a
  fork, as a commit marked with a comment naming the app
  (`// neochumby-quake: ...`), and the submodule is pinned to that commit:
  no patch files applied at build time. `tools/tree` (and so `tools/stick`
  and `tools/deploy`) checks out the submodules of the apps it builds, so a
  clone without `--recurse-submodules` works there; `cargo` alone needs
  `git submodule update --init` first.
- **Comments say what the code does and why the rule is what it is**, not
  how it was debugged. One paragraph is usually enough.
- **Licences stay per crate.** Our crates are MIT. A crate that links GPL
  code says so in its `license` field and ships the upstream licence. Never
  commit game data, firmware images or anything from a Chumby's settings.
- **None of the Chumby's own software, anywhere.** No firmware, images or
  stock artwork in the repository, releases, CI or attachments, and the
  emulator never impersonates a real device: see
  [docs/legal.md](docs/legal.md).
- **Nothing starts remote access by itself.** SSH on a Chumby is turned on
  by its owner, from its settings.

## Unsafe code

The workspace denies `unsafe` code. Every raw system call lives in
`crates/neochumby-sys`, behind safe functions, each `unsafe` block with a
`// SAFETY:` comment saying why it holds; anything else needing one adds a
function there, and uses std instead wherever std does the job safely. The
games (Doom, Quake), fetch, htop and top are allowed it as FFI to their C. We use libc
rather than rustix: rustix calls the kernel directly and needs Linux 3.2,
the Chumby runs 2.6.16, and its `ioctl` and `mmap` are `unsafe` too (see
neochumby-sys's documentation).

## Building and checking

```sh
git submodule update --init   # the third-party C: Doom, Quake, fastfetch, htop, procps, ncurses
rustup target add armv5te-unknown-linux-musleabi x86_64-unknown-linux-musl
# zig 0.17 on PATH (the C compiler, linker and musl): https://ziglang.org
# CMake 3.21 or later, for fetch
cargo chumby            # the session and neochumby-home, for the Chumby
cargo neochumbyd        # the server
tools/tree /tmp/tree doom quake airplay fetch htop top logs   # a whole tree, with apps
```

CI runs the same checks on every pull request. To run them yourself:
`cargo fmt --all --check`, `cargo clippy -- -D warnings` (on the host
crates and, with `--target armv5te-unknown-linux-musleabi`, on the Chumby's
crates), and `cargo test -p neochumby-proto -p neochumby-sync -p
neochumbyd`.

Code that runs on the Chumby but needs no Chumby goes in a crate that also
builds on the host, so CI can test it there. `neochumby-sync` is one: its
tests run whole sync rounds over a real session on loopback, with a
temporary directory standing in for the stick.

### On a Chumby

A paired Chumby can be worked on without SSH. Deploy a tree and restart
its session on it (`tools/deploy HOST [APP ...] --restart NAME`), read
what it and its apps wrote (`neochumbyd log NAME -f`) and capture its
screen (`neochumbyd shot NAME OUT.png`), all on the server. Everything
an app writes to stdout or stderr ends up in that log, so print what
you'd want to read there. The protocol is in `neochumby-proto`'s `dev`
module.

### With Claude Code (MCP)

`neochumbyd mcp` serves the same as an [MCP](https://modelcontextprotocol.io)
server on stdin and stdout, so Claude Code (or any MCP client) can read a
Chumby's log, restart it and look at its screen while you work on an app
together. Add it once, from your checkout, running it on the server over
SSH:

```sh
claude mcp add neochumby -- ssh -o BatchMode=yes HOST neochumbyd mcp
```

That adds it for you, in this project only (Claude Code's `local` scope),
and nothing is committed; `claude mcp list` or `/mcp` in Claude Code shows
it connected, with its tools and what each does. The SSH user needs what
the `neochumbyd` command does on that server, usually membership of its
group.

A loop that works well: deploy (`tools/deploy HOST APP --restart NAME`),
then ask Claude to read the log and look at the screen and say what's
wrong. Pairing stays with a person (`neochumbyd pairing`, `approve`), and
so does turning on remote input (`dev=on`): don't let a tool do that for
you.

To drive it as a scenario drives the emulators, turn on input for it
first: `neochumbyd set NAME dev=on` (and `dev=` to turn it off again). It
is off by default, and both neochumbyd and the Chumby refuse input
without it: pairing a Chumby lets its server see its log and its screen,
not use it, until its owner says so. Then:

- `neochumbyd tap NAME X Y [--hold MS]` touches the screen at (X, Y), in
  pixels (0-319, 0-239), for MS milliseconds (100 by default, at most
  10000), then lets go. It's written to the touchscreen's input device,
  through the Chumby's own calibration, so every reader sees it as a
  finger: the app in front, and the escape hatch (`tap NAME 10 10 --hold
  3500` ends the app in front).
- `neochumbyd button NAME press|release|tap` puts the top button down, up,
  or down and up again 300 ms later. It goes where a real press goes: a
  `button` line to the start app, or back to the start app with
  `NEOCHUMBY_REASON=button`. **Except to an app with `button = true`**:
  such an app reads `/dev/switch` itself, so a press from neochumbyd can't
  reach it, and the command says so. A scenario that needs the button in a
  game (FIRE in Doom) can't be run on a Chumby this way.

Each command returns once the Chumby has done it (a tap, once it has let
go), or fails with why not, or after 15 seconds if the Chumby isn't
connected. One at a time per Chumby. A press is let go, and a touch
lifted, if the link drops.

## Testing without a Chumby: dev/emulate

`dev/emulate` runs a tree on a Linux PC as if it were a Chumby: the same
static ARM binaries, unchanged, under qemu-arm user mode, chrooted in a fake
root with the Chumby's paths. The screen is a file, and the touchscreen, top
button and speaker are fakes you drive from outside. CI boots every pull
request's tree with it (`dev/emulate-smoke`).

It needs Linux, root, Python 3.9+, FUSE, util-linux's `mount` with loop
devices, qemu-user's binfmt handler (static), and dosfstools for stick
images. On a Mac, it runs itself in Linux that shares the checkout (below).

```sh
sudo apt install qemu-user-binfmt dosfstools    # Debian, Ubuntu (older: qemu-user-static)
tools/tree /tmp/tree doom
sudo dev/emulate start /tmp/tree     # the tree at /mnt/neochumby, as over NFS
dev/emulate shot home.png
dev/emulate tap 160 102              # the second row of the home screen
dev/emulate tap 5 5 --hold 3500      # the corner hold
dev/emulate button press             # the top button
dev/emulate log -f                   # what the session and its apps print
dev/emulate stop
```

- **A stick:** `sudo dev/emulate mkstick /tmp/stick.img /tmp/tree` makes a
  FAT image laid out as `tools/stick` does, and `start --stick
  /tmp/stick.img` mounts it at `/mnt/usb` and boots from it. Given a tree
  as well, the session runs from the tree and the stick starts as it is.
- **A neochumbyd:** the emulator shares the PC's network, so a neochumbyd
  running on the PC can pair with it (`start --server 127.0.0.1 ...`).
- **Inside:** `sudo dev/emulate exec /mnt/neochumby/neochumby sensors` runs
  a binary beside the session.
- **Its directory** is `/var/tmp/neochumby-emulate-UID`, root's, with the
  screen, the log and `/psp` (kept from run to run). `--state DIR` picks
  another; sudo drops `NEOCHUMBY_EMU`, so pass it with `sudo
  --preserve-env=NEOCHUMBY_EMU`.
- **Pictures and the like:** `start --bind DIR:/tmp/classic` puts a host
  directory in the fake root, read-only. `dev/emulate compare RAW ...` says
  which 320x240 RGB565 picture the screen is most like.

The session runs as root, as on the Chumby, but without the capabilities
that would let it out of the fake root or into the PC's settings: no
mounts, chroot, modules, raw devices or network setup, and `/proc/sys` is
read-only.

### On a Mac

`dev/emulate` needs Linux, so on a Mac it runs itself in Linux that shares
this checkout at the same path. Every command, run from inside the checkout
with paths inside it, goes there and runs as root there, so no sudo.

One of:
- **Apple's `container`** (macOS 26 or later, Apple silicon): install it
  from github.com/apple/container and run `container system start`. It is
  used if installed. Its kernel has no vfat, so stick images (`mkstick`,
  `start --stick IMG`, the sync scenario) need Lima.
- **Lima**: `brew install lima`. Set `NEOCHUMBY_EMU_VM=lima` to use it when
  `container` is installed too.

```sh
tools/tree target/emu-tree doom
dev/emulate vm start                 # once per boot; the first one builds the image or VM
dev/emulate-smoke target/emu-tree target/emu-smoke    # the smoke scenario
dev/emulate start target/emu-tree    # or by hand, as on Linux, without sudo
dev/emulate shot home.png
dev/emulate stop
dev/emulate vm exec CMD ...          # anything else, as root, in that Linux
```

The emulator's directory stays inside that Linux (`/var/tmp`): a folder
shared from the Mac can't hold its control socket, so don't give `--state` a
path in the checkout.

If the first `vm start` with `container` can't resolve names (apt-get exits
100), build the image with a DNS server named, then start again:

```sh
container build --dns 1.1.1.1 -t neochumby-emulate -f dev/Containerfile dev
dev/emulate vm start
```

To clean up: `dev/emulate vm stop` (it starts again with `vm start`), or
`dev/emulate vm delete` to remove the container or VM. With `container`, the
image stays (`container image rm neochumby-emulate`, about 100 MB). With
Lima, its downloaded Ubuntu image stays in Lima's cache (`limactl prune`).

### Testing the developer link in dev/emulate

A neochumbyd beside the emulator pairs with it and drives it as it would a
real Chumby (its name is `chumby`). The touchscreen and button are
dev/emulate's fakes, so this tests the protocol and the session, not the
Chumby's drivers. On Linux, run `$B` as root; on a Mac, put `dev/emulate vm
exec` before each `$B`.

```sh
tools/tree target/emu-tree doom && cargo neochumbyd
B=target/x86_64-unknown-linux-musl/release/neochumbyd
N="--state /var/tmp/nd"
$B serve $N --tree target/emu-tree --api 127.0.0.1:7340 --nfs 127.0.0.1:2049   # in another terminal
dev/emulate start --server 127.0.0.1 target/emu-tree
dev/emulate tap 80 220                 # PAIR
$B pairing $N                          # the code on the screen
$B approve CODE $N                     # the screen says PAIRED
dev/emulate stop
dev/emulate start --server 127.0.0.1 target/emu-tree   # neochumbyd: chumby connected
$B set chumby dev=on $N
$B tap chumby 160 62 $N                # DOOM: "home asks: open doom"
$B tap chumby 5 5 --hold 3500 $N       # the corner hold: "escape", home again
$B button chumby tap $N                # "the top button: told home"
$B shot chumby nd.png $N && dev/emulate shot emu.png  # the same pixels
$B set chumby dev= $N                  # tap and button refused again
dev/emulate stop
```

### Scenarios

**neochumby-classic** (`dev/emulate-classic TREE PICTURES OUT`; CI runs it
once classic is a start app, #26). classic is the start app
(`start=classic` in `/psp/neochumby.conf`), with pictures from
neochumby-classic's offline smoke render, made with `NEOCHUMBY_TREE=TREE`
so that its APPS screen lists the tree's apps: made-up data and placeholder
artwork. The script checks that:
- classic draws a slideshow page;
- the top button toggles Home and back without restarting classic;
- Home's APPS, then the DOOM tile, opens Doom through the session's pipe,
  and classic comes back when Doom exits;
- classic's service starts once, hears that Doom is in front, and keeps
  running;
- CHUMBY on Home ends the session with exit 0.

Where it taps comes from the pictures' own tap zones.

**A power cut during a sync round** (`dev/emulate-powercut TREE NEOCHUMBYD
OUT`, run by CI). The emulated Chumby pairs with a neochumbyd on the PC
through neochumby-home's PAIR, syncs its tree to a FAT stick image
(`delivery=sync`), then boots through the real boot hook (`start --boot`,
under a static busybox) while a second round writes the other set. `sudo
dev/emulate powercut OUT.img` then cuts the power: the session is frozen
where it is, the stick image copied as it stands (whatever the PC's kernel
hadn't written to it is lost), and everything killed. Booting that copy,
the log must show the hook running a complete set, `check-set` agreeing,
and the next round making the new set live, which one more boot runs. A
cut that came too late is tried again, sooner. With `--device` the person
beside the Chumby is asked to pull the plug; the full-system machine skips
it until qemu-chumby has USB. (Every cut point of a round, on the host, is
neochumby-sync's own crash test.)

**AirPlay's now-playing screen** (`dev/emulate-airplay TREE NEOCHUMBYD
OUT`, run by CI through `dev/e2e`; user mode only). A neochumbyd on the PC
runs the airplay app's companion with `dev/fake-shairport` in
shairport-sync's place: made-up metadata, a generated cover, silence, and a
sender's remote-control server. The emulated Chumby pairs through
neochumby-home's PAIR, and the script checks that:
- the stream beginning opens the screen over the home screen, with the
  cover in its box;
- play/pause and next on the screen reach the sender as DACP requests,
  with its Active-Remote token, and the screen shows their effect;
- the stream ending leaves the screen ready for AirPlay.

**Sync** (manual, not in CI). It needs a paired Chumby and neochumbyd's
side of a round, so it waits until `delivery=sync` (#10) and the session
PRs are both in `main`. Then, as root on one Linux PC (not yet run, so
expect to adjust it):

```sh
tools/tree /tmp/tree doom && cargo neochumbyd
B=target/x86_64-unknown-linux-musl/release/neochumbyd
$B serve --state /tmp/nd --tree /tmp/tree --api 127.0.0.1:7340 --nfs 127.0.0.1:2049 &
dev/emulate mkstick /tmp/stick.img            # an empty FAT stick
dev/emulate start --stick /tmp/stick.img --server 127.0.0.1 /tmp/tree
dev/emulate tap 80 220                        # PAIR, on neochumby-home
$B pairing --state /tmp/nd                    # the same code as the screen
$B approve CODE --state /tmp/nd
$B set chumby delivery=sync --state /tmp/nd   # the emulator's name: chumby
dev/emulate stop
dev/emulate start --stick /tmp/stick.img /tmp/tree   # paired now: settings fetched
dev/emulate wait log 'neochumby-sync' 120      # the receiver's first round
```

Then check, from the PC, with the stick image mounted read-only:
- `neochumby/a/` (or `b/`) holds the tree;
- its `.complete` marker is whole, and `current` names it;
- `dev/emulate exec /mnt/usb/neochumby/neochumby check-set
  /mnt/usb/neochumby/a` agrees.

For the next boot, the boot hook won't run here: it needs BusyBox. Start
the set `current` names in its place, with `--run`:

```sh
dev/emulate start --stick /tmp/stick.img --run /mnt/usb/neochumby/a/neochumby
```

Then change a file in `/tmp/tree` and wait for a round to write the other
set and flip `current`. A power cut mid-round can be approximated by
`dev/emulate stop` (SIGKILL) during a large file. That shows the journal
and resume logic, not FAT's behaviour.

### The whole Chumby: --machine chumby-classic

`--machine chumby-classic` (or `NEOCHUMBY_EMU_MACHINE=chumby-classic`)
boots the stock 2.6.16 firmware on QEMU's chumby-classic machine
(`emu/qemu/build`), with the tree in the NAND's Cache partition as a stick
and a test boot hook in the settings. It needs no root and runs natively on
a Mac. The firmware is your own device's (`--kernel`, `--nand`,
`--factory-psp`, or `NEOCHUMBY_EMU_KERNEL`, `_NAND`, `_PSP`; see
`emu/README.md`), and it stays on your machine: never in the repository,
CI or an attachment (`docs/legal.md`). Without it, `start` and the
scenarios exit 77, skipped.

```sh
export NEOCHUMBY_EMU_MACHINE=chumby-classic NEOCHUMBY_EMU_KERNEL=... NEOCHUMBY_EMU_NAND=... NEOCHUMBY_EMU_PSP=...
dev/emulate-smoke /tmp/tree /tmp/smoke     # the same scenario, its user-mode steps skipped
```

Touches go over QMP (`input-send-event`), screenshots are `screendump`,
and `log` is the console. The top button, `exec`, `selftest`, `--stick`
and `--bind` are user mode's only, for now. `python3 dev/test_emulate.py`
(run by CI) checks its images against `chumby-emu`'s and its control path
against a stub QEMU.

### A real Chumby: --device NAME

The same scenarios run on a real Chumby, paired with a neochumbyd and set
`dev=on` (`neochumbyd set NAME dev=on`, which allows remote input), over
its developer link: `NEOCHUMBY_EMU_DEVICE=NAME dev/emulate-smoke /tmp/tree
/tmp/smoke`, with `NEOCHUMBY_EMU_HOST=HOST` when neochumbyd runs elsewhere
(over ssh; its state in `NEOCHUMBYD_DIR`). `start` installs the tree there
(`tools/deploy --tree`) and restarts the Chumby; `log` is its log from then
on; `shot`, `tap` and `button` are neochumbyd's (`neochumbyd tap` and
`button`; an older neochumbyd without them skips those steps). A remote
press goes where a real one would, so it can't reach an app that reads the
switch itself (`button = true`, Doom's FIRE): a scenario needing that skips
on a device. Steps that need hands, like pulling the power, are
`dev/emulate prompt` lines, shown only with `--device`.

So that one scenario holds on every backend, it asserts on lines the
session and apps log on purpose, compares only our own screens (with what
changes masked), scales its waits by `dev/emulate scale`, and exits 77,
skipped, where a backend can't do a step.

`dev/emulate --help` lists the fake devices and what each does. What user
mode can't catch is what the full-system machine and real devices are for:

- **The 2.6.16 kernel.** System calls go to the PC's kernel. Futex flags
  that kernel lacks, `nanosleep` failing with `EINVAL` on restart, and newer
  calls and flags all work here and fail there.
- **Real FAT and USB.** The stick is the PC's vfat driver on a file in
  memory, not the old driver on a slow stick. Its directory fsync, its
  speed, a stick pulled out and a power cut aren't reproduced.
- **Real input.** Taps are clean: no echo after a release, no brief releases
  during a hold, and the button never bounces.
- **Speed.** qemu on a PC isn't a 266 MHz ARM926, so frame rates and CPU
  load mean nothing here. Only the speaker is paced, to the rate it's set to.
- **The rest of the Chumby.** The boot hook doesn't run: the session is
  started directly. There's no BusyBox, `btplay` or `chumby_set_volume`, no
  accelerometer, and no overlay or backlight to see.

## Writing an app

An app is a crate named `neochumby-<name>`. It lives in this repository
under `apps/<name>/`, or in its own repository with the crate at the root,
where it is added to a tree from its git URL or from crates.io
(`tools/tree OUT git+https://github.com/you/neochumby-weather#REV`). Its
`Cargo.toml` says what it is:

```toml
[package]
name = "neochumby-weather"
# ...

[dependencies]
neochumby = { git = "https://github.com/johnmerchant/neochumby" }

[package.metadata.neochumby]
title = "WEATHER"                    # listed in app lists
exec = "neochumby-weather"           # runs in front when the app is opened
colour = "4a90d9"                    # optional
service = "neochumby-weather-sync"   # optional: runs in the background
companion = "neochumby-weather-companion"   # optional: its server half
neochumbyd = "optional"              # "required" | "optional" | absent
start = true                         # optional: it may be the start app
interrupt = false                    # optional: no service replaces it (games)
button = true                        # optional: it gets the top button (games)
access = ["screen", "touch"]         # optional: what it reaches beyond its jail
```

`tools/tree` checks the table and writes the tree's `manifest` from it
(`neochumby-manifest`). An app with a `companion` must also say
`neochumbyd = "required"` or `"optional"`: it is never inferred. A
`files/` directory beside `Cargo.toml`, optional, is copied into the app's
directory as it is (icons, configuration). An app in its own repository
may keep third-party source in submodules: `tools/tree` checks them out
for a `git+` app (in `~/.cache/neochumby/app-sources`), and a crate
published to crates.io carries their files in its package.

Binaries named by `exec` and `service` are built for the Chumby
(`armv5te-unknown-linux-musleabi`), and a `companion` for the server. Depend
on the `neochumby` crate for the screen (`fb`), touch, squeeze and
accelerometer (`input`), the manifest and settings (`apps`, `conf`),
crash reports (`debug`), the request pipe (`session`) and a terminal for a
program written for one (`term::run`, which `apps/fetch` uses; the
touchscreen is its mouse, and `term::run_tapping` makes a tap type a key,
as `apps/top` quits). A C program that uses curses depends on
`neochumby-ncurses` and builds against `DEP_NCURSESW_INCLUDE`
(`apps/htop/build.rs`). The screen
is 320x240 RGB565, so prerender what you can, and keep in mind that the CPU
is a 266 MHz ARM926 with no FPU.

### The app contract

The session (`neochumby`) runs one app at a time in front, full-screen.
Every app is opened the same way, whoever asked for it.

**When your app is opened**, the session runs its `exec`:
- jailed, as below, in the app's directory in the tree, in a process group
  of its own;
- with `NEOCHUMBY_ROOT` (the tree) and `HOME=/tmp`. The tree is read-only;
  `/tmp` is your app's own;
- with `NEOCHUMBY_SESSION=<fd>`, its request pipe to the session (below);
- with `NEOCHUMBY_COMPANION=<fd>` if it has a companion (below);
- with `NEOCHUMBY_REASON` when the session has a reason to give: `button`
  when the top button opened it. Ignore values you don't know;
- owning the screen until it exits. What's on the screen when it starts is
  whatever was there: draw all of it;
- sharing the touchscreen: every reader gets every touch. The session
  watches the top-left corner for the escape hatch;
- **the top button** only with `button = true`. Only one process at a time
  can have `/dev/switch` open, and the session keeps it. An app with
  `button = true` is handed the open file as `NEOCHUMBY_BUTTON=<fd>`, and
  `neochumby::input::Squeeze::open` picks it up. A read returns the level
  now, so act on a 0 → 1 change.

**To leave**, your app exits. Then the session opens the start app again,
and ends anything your app left running in its process group (SIGTERM,
then SIGKILL 200 ms later), so nothing you started keeps the screen, the
speaker or the button.
- **To open another app**, write `open <app>` (the app's name, its
  directory under `apps/`) to the request pipe, then exit. The session runs
  that app next.
- **For CHUMBY MODE**, write `chumby-mode`, then exit. The session stops
  the services and hands the Chumby back to its own software.
- Lines end in `\n`. Each end ignores lines it doesn't know, and the
  session ignores an `open` for an app that isn't here or can't run. The
  last request before you exit counts.
- After a request, exit within 5 seconds. Otherwise the session ends your
  app: SIGTERM to its process group, then SIGKILL 2 seconds later.
- **The escape hatch:** holding the top-left corner for 3 seconds ends
  your app the same way, whatever it is doing, and opens `neochumby-home`.
  So handle SIGTERM by exiting promptly, and expect anything you started to
  be ended with you.
- **The top button:** unless your app says `button = true`, a press ends
  it the same way and opens the start app with `NEOCHUMBY_REASON=button`,
  as the stock Chumby does. If your app is the start app, it gets a
  `button` line on its pipe instead (below). There's no hold gesture: games hold the button
  to fire, and the corner hold is the way out of anything.
- **A service** may end your app the same way, to open another: any
  service over the start app, only the start app's over other apps (an
  alarm, say). Declare `interrupt = false` if your app mustn't be ended
  like that: games do.

In Rust:

```rust
use neochumby::session::Pipe;

if let Some(mut pipe) = Pipe::from_env() {
    let _ = pipe.open("doom"); // or pipe.chumby_mode()
}
std::process::exit(0);
```

`Pipe::from_env()` is None when the app wasn't started by the session (run
by hand).

**The jail.** The session runs as root; your app doesn't. Its `exec` and
its `service` run as a user of their own (one uid per app, from 20000), in
a private mount namespace, chrooted into a small tmpfs that holds:
- your app's directory from the tree, at the same path, read-only;
- `/tmp`, your app's own (4 MB, `nosuid,nodev,noexec`), which is `HOME`;
- `/dev/null`, `zero`, `random` and `urandom`;
- what its `access` lists, and nothing else:

| `access` | what your app gets |
|---|---|
| `screen` | `/dev/fb0`, for its `exec` only, never its `service` |
| `touch` | the touchscreen and its calibration (`/psp/ts_settings`, read-only), likewise |
| `speaker` | `/dev/dsp`, and the speaker's handover (`neochumby::audio`). The mute and the volume are the session's: `audio` asks for them over the request pipe |
| `accel` | the accelerometer |
| `system` | `/proc`, `/sys` and `/etc` |
| `network` | `/etc/resolv.conf` and `/etc/hosts`; and internet sockets (below) |
| `terminal` | `/dev/ptmx` and `/dev/pts`, for a pseudo-terminal (`neochumby::term`) |
| `logs` | the Chumby's logs, read-only, each at its own path: the session's (`/tmp/neochumby.log`), `/tmp`'s other `*.log` and `/var/log`'s files, as they are when it starts (plain files only). The kernel's ring needs no access: `neochumby_sys::klog` |
| `root` | no jail: it runs as root, as the session does. Listed alone |

It may have 64 processes and 256 open files, and dumps no core. Its users
can't signal or trace each other, or root, and can't read the Chumby's
key or write to `/psp`.

**Your binaries.** Each `exec` and `service` must be a static ARM ELF.
Before it starts one, the session refuses a file with a `PT_LOAD` below
`0x1000` (page zero), a `PT_INTERP`, or a non-ARM ELF, and it checks every
executable file in your app's directory the same way. Page zero is where
the ARM `mmap` check doesn't reach the ELF loader. Put helper programs in
the app's directory, not in `/tmp`: `/tmp` and the speaker handover are
`noexec`.

**The network.** The stock kernel can't take the network away from a
process. Instead, once a second, the session looks for internet sockets in
`/proc/net`, which lists each socket with its owner's uid. If it finds one
owned by an app whose `access` doesn't list `network`, it ends the app,
`exec` or `service`, as if it had crashed. Unix sockets aren't counted.
Ask for `network` if your app opens a socket at all, even briefly: an
unlisted one is found sooner or later.

List only what your app uses. `neochumby-manifest` (run by `tools/tree`)
refuses words it doesn't know. `root` is for `home`, which pairs the
Chumby and so writes its key. Under `dev/emulate`, which can't mount or
chroot, every app runs unjailed, as root.

**Start apps.** The start app is the one a Chumby opens at boot and comes
back to. The owner picks it (`start=<app>`), and it defaults to
`neochumby-home`. Declaring `start = true` lets your app be picked, and
promises:
- it **opens other apps** through the request pipe;
- it offers **CHUMBY MODE**, and the owner's settings, or at least points
  to where they are;
- it handles **the top button**, for example by showing its own main
  screen. A press while it's in front arrives as a `button` line on its
  pipe (read it with `Pipe::receive` and `Notice::parse`, without waiting).
  If the line can't be written (the pipe is closed or full), the session
  opens the start app again with `NEOCHUMBY_REASON=button`, as it does
  after a press over any other app;
- it **doesn't say `button = true`**, so a press always lands somewhere
  useful. The session refuses a start app that does;
- it **keeps running**. If it exits without asking for anything five times
  within a minute, the session makes `neochumby-home` the start app
  instead.

Without `start = true`, the session refuses the app as the start app and
uses `neochumby-home`, so a Chumby can't be left stuck in a game.

### Services

What has to keep working while another app is in front (alarms, a night
schedule, music) belongs in the app's `service`. The session runs each
app's service, where what it needs of a neochumbyd is here, for as long as
neochumby runs:
- with the same environment as an app (`NEOCHUMBY_ROOT`, `HOME=/tmp`,
  `NEOCHUMBY_SESSION`), in a process group of its own, in the app's jail
  but without the screen or the touchscreen;
- **restarted when it exits**, after 1 second, then 2, 4 and so on up to a
  minute. A run of a minute or more starts the wait again from 1 second;
- **told what's in front:** `front <app>` on its pipe as it starts, and
  whenever another app comes to the front. Reading these is optional. A
  line is never cut; while the pipe is full, only the latest notice waits;
- **stopped at CHUMBY MODE:** SIGTERM to its process group, then SIGKILL 5
  seconds later.

A service may write `open <app>` or `chumby-mode` to its pipe, like an
app, but it doesn't exit after. That ends the app in front, so the session
takes it only:
- over the start app, from **any service** (an alarm, say); over any other
  app, only from the **start app's service**, so services don't fight over
  the screen. `chumby-mode` only from the start app's service;
- when the app in front doesn't say `interrupt = false`;
- not after the escape hatch, until an app is opened by hand (an `open`
  from the app in front), so a service can't trap the Chumby in an app gone
  wrong;
- not once the app in front has asked for something itself: what it asked
  for wins.

Which services run is decided as the session starts: one that needs a
neochumbyd starts on a paired Chumby only, and pairing asks for a restart.

Anything else is ignored and logged, and so is an `open` for the app
already in front.

**The speaker.** When a service exits, the session mutes the speaker and
puts back the master volume the Chumby booted with (`/psp/volume`),
through `neochumby::audio`, so a service that dies while playing can't
leave it on. If another live process holds the speaker (the handover's
claim, which `audio::Unmuted` and `audio::Speaker` make), it's left alone.
As the session ends, it does the same, waiting up to 3 seconds for the
volume.

In Rust, a service reads its notices with `Pipe::receive`, which doesn't
wait, and `Notice::parse`.

**An exec talking to its own service.** `neochumby::service` gives the two
a local socket, named after the app (`neochumby::service!()`, from its
crate's name): the service `listen`s and `accept`s, the exec `connect`s,
and frames go both ways without waiting. Each end refuses a peer running
as another user. Another app could still bind the name first, which keeps
the two apart, so don't let anything critical depend on it. AirPlay's
screen and service are an example.

**Companions.** An app with a server half ships it as a second binary,
named by `companion`. neochumbyd runs it for each paired Chumby that opens
the app:
- **Opening:** the Chumby side calls
  `neochumby::companion::open(neochumby::companion!())`, which returns a
  `Link` to its companion. `companion!()` reads the app's `Cargo.toml` as
  it compiles: an app without a `companion`, or without `neochumbyd`, does
  not compile.
- **Through the session:** apps never hold the Chumby's key. The session
  opens the connection to neochumbyd for your app, and relays its frames
  over a socket it hands the app (`NEOCHUMBY_COMPANION`). It opens the
  companion of the app it started, named by the app's directory, so an app
  reaches only its own. Run by hand, as root, an app connects itself.
- **Frames:** `Link::send` and `Link::recv` (`None` once the companion has
  gone). On the companion's side they arrive on stdin and leave on stdout,
  a u32 big-endian length then the bytes (`neochumby_proto::frame`).
- **Environment:** the companion gets `NEOCHUMBY_CHUMBY` (the Chumby's name)
  and `NEOCHUMBY_STATE` (a directory of its own). It should exit when its
  stdin closes.

`apps/airplay` is a worked example of both halves, and `apps/logs` one
whose companion is optional.

## Files in the tree

With `delivery=sync`, a Chumby keeps a copy of the tree on its FAT stick.
Names must be ones it can take: A-Z a-z 0-9 `.` `_` `+` `-`, not starting
or ending with `.`, unique ignoring case, and not a DOS device name (`con`,
`aux.txt`). `neochumbyd manifest` lists anything left out. Only files that
change are sent, so keep names stable and leave identical files alone.

## Pull requests

Keep a pull request to one change, explain why in its description, and make
sure CI is green. If you're changing how a Chumby boots
(`boot/userhook2`), test on a real one or say that you couldn't: a hook
that hangs stops the Chumby's boot. The hook runs under BusyBox 1.6 `sh`,
which has `sed` but no `sha256sum`, `printf`, `od` or `head -c`. Anything
it can't do goes in the session binary as a subcommand, like `neochumby
check-set`. Raise its `neochumby-hook-version` line with every change: the
session replaces the hook installed in `/psp` only with a higher version.

### Test on a Chumby, or an emulated one, and show it

CI builds and unit-tests everything, but it can't tell whether a change
works on a Chumby. So a pull request that changes what runs on the Chumby
(`crates/neochumby`, `crates/neochumby-sys`, `apps/`, `boot/`) is
approved only once its description, or a comment, shows it running. There
are three ways, from quickest to most faithful:

1. **`dev/emulate`** (user mode, on Linux, or a Linux VM on a Mac): your
   tree's binaries against fake devices, on the PC's kernel. `sudo dev/e2e
   TREE NEOCHUMBYD OUT` runs every scenario, as CI does, or drive it by
   hand with `start`, `tap`, `button`, `shot` and `log`. Enough for most
   app and session changes.
2. **The emulator** (`emu/`, QEMU with the Chumby Classic machine): the
   Chumby's stock 2.6.16 kernel and firmware, from **your own device's**
   flash backup (`emu/README.md` says how to build the images; they never
   leave your machine). Use it for anything that depends on the kernel,
   the flash, FAT, the boot or the handover to the stock software:
   `NEOCHUMBY_EMU_MACHINE=chumby-classic dev/e2e TREE NEOCHUMBYD OUT` runs
   the same scenarios on it. CI can't, having no firmware (and it never
   will), so this suite is yours to run locally, and its results are the
   evidence.
3. **A real Chumby**, if you have one: deploy, then `neochumbyd log`,
   `neochumbyd shot` (see [On a Chumby](#on-a-chumby)). Needed for what no
   emulator models: timing, the touchscreen's noise, a USB stick's own
   behaviour, power cuts.

**The evidence to include:**
- which of the three, and the commit you tested;
- the commands you ran, and what they reported: `dev/e2e`'s
  `OUT/summary.txt` (each scenario ok, skipped or FAILED), or a short log
  excerpt showing the behaviour;
- screenshots of **neochumby's** screens or your app's, as PNG, showing
  the change.

**Never attach:**
- the stock software's screens or artwork (the control panel, the Chumby
  logo or octopus, widgets), firmware, images, or anything from a
  device's settings ([docs/legal.md](docs/legal.md));
- anything that identifies you, your network or your device: addresses,
  host and network names, Wi-Fi names, a device's name or serial. Cut
  them from logs before pasting.

If you couldn't test something (no Chumby, or a path no emulator covers
yet), say which and why. A reviewer may run it on a Chumby for you.

### Scenarios run everywhere

The automated scenarios (`dev/emulate-smoke`, `dev/emulate-powercut`,
`dev/emulate-classic`, and any you add to `dev/e2e`) use only `dev/emulate`'s verbs, `start`, `log`/`wait log`,
`shot`, `tap`, `button` and `stop`, so the same scenario runs on user
mode, on the emulator (`--machine chumby-classic`) and on a real Chumby
(`--device NAME`, through `neochumbyd`; remote input needs that Chumby set
`dev=on`). Write them so they pass on all three:

1. **Assert on log lines** the session and apps print on purpose
   (`opening home`, `escape: ending …`), not on timing or on the order of
   unrelated lines. If a scenario needs to see something happen, print a
   line for it.
2. **Compare screenshots by regions of our own screens**, masking what
   changes (the clock, time-dependent pictures). Never compare against, or
   commit, the stock software's screens.
3. **Wait with timeouts the backend scales**: user mode is fast, a real
   Chumby and the emulator in real time are not.
4. **What a backend can't do is skipped there, not failed**, with the
   reason: remote input can't reach an app that holds the top button
   itself (`button = true`), and there's no stick on the emulator until
   it models USB.
5. **Physical steps** (pull the power, plug in a stick) are prompts the
   runner shows and waits on, checked from the log afterwards; they run
   only with `--device` and someone there.
