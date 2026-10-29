"""dev/emulate's --device NAME backend: a real Chumby, paired with a
neochumbyd and driven over its developer link, so that the same scenarios
run on hardware as on the two emulators.

  start TREE   the tree into the server's state (tools/deploy --tree, or
               locally the same rsync), then `neochumbyd restart NAME`
  log, wait    `neochumbyd log NAME -f`, from the restart on
  shot, pixel, compare, wait screen
               `neochumbyd shot NAME OUT.png`
  tap          `neochumbyd tap NAME X Y --hold MS`
  button       `neochumbyd button NAME tap|press|release`, for dev/emulate's
               press|down|up
  stop         `neochumbyd restart NAME`: back to the start app
  prompt TEXT  shows a step for the person beside the Chumby (pull the
               power, plug in a stick) and waits for Enter

The server is local, or HOST over ssh (--host, NEOCHUMBY_EMU_HOST), with
its state in NEOCHUMBYD_DIR (default /var/lib/neochumbyd) and the
neochumbyd tools/deploy installs there (else one on PATH, locally). Remote
input needs the Chumby set `dev=on` (`neochumbyd set NAME dev=on`); an older
neochumbyd, from before `tap` and `button`, makes those steps skip (exit
77). A remote press goes where a real one would, so it can't reach an app
that reads the switch itself (`button = true`, Doom's FIRE): scenarios that
need that skip on a device.
The session's end is its "neochumby: CHUMBY MODE" line: the boot hook's
exit status isn't on the link.
"""

import os
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import zlib

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MACHINE = "device"
SKIPPED = 77
W, H = 320, 240
NAME = re.compile(r"[a-z0-9_-]{1,32}")
# The session's last line before it hands the Chumby back.
ENDED = re.compile(rb"neochumby: CHUMBY MODE")
# How long the log's history takes to arrive before following starts.
SETTLE = 1.0


class Server:
    """Where neochumbyd's commands run: here, or HOST over ssh."""

    def __init__(self, host, state_dir):
        self.host = host or ""
        self.dir = state_dir or "/var/lib/neochumbyd"

    def argv(self, *args):
        if self.host:
            line = shlex.join([f"{self.dir}/neochumbyd", *args, "--state", self.dir])
            return ["ssh", "-o", "BatchMode=yes", self.host, line]
        local = os.path.join(self.dir, "neochumbyd")
        exe = local if os.access(local, os.X_OK) else shutil.which("neochumbyd")
        if not exe:
            sys.exit(f"dev/emulate: no neochumbyd here (in {self.dir} or on PATH); give --host for a server")
        return [exe, *args, "--state", self.dir]

    def run(self, *args, check=True):
        r = subprocess.run(self.argv(*args), capture_output=True, text=True)
        if check and r.returncode != 0:
            sys.exit(f"dev/emulate: neochumbyd {' '.join(args)}: {(r.stderr or r.stdout).strip()}")
        return r


def server_of(read, state):
    return Server(read(state, "device-host"), read(state, "device-dir")), read(state, "device-name")


def missing(r, command):
    """Whether a neochumbyd answered with a usage that hasn't `command`: one
    from before it."""
    text = r.stderr + r.stdout
    return r.returncode == 2 and "usage:" in text and f"|{command} NAME" not in text


def start(state, opts, tree, write, read, remove):
    name = opts["device"]
    if not NAME.fullmatch(name):
        sys.exit(f"dev/emulate: not a Chumby's name: {name}")
    for unsupported in ("stick", "binds", "run", "server", "psp", "kernel", "nand", "factory_psp", "qemu"):
        if opts.get(unsupported):
            sys.exit(f"dev/emulate: --{unsupported.replace('_', '-')} isn't available with --device")
    server = Server(opts.get("host") or os.environ.get("NEOCHUMBY_EMU_HOST"), os.environ.get("NEOCHUMBYD_DIR"))
    # Paired, and the server reachable: its log answers.
    server.run("log", name)
    write(state, "machine", MACHINE)
    write(state, "device-name", name)
    write(state, "device-host", server.host)
    write(state, "device-dir", server.dir)
    write(state, "fb0", "", size=W * H * 2)
    for f in ("exit", "ready", "failed", "pid"):
        remove(state, f)
    write(state, "log", "")
    write(state, "emulate.log", "")

    pid = os.fork()
    if pid == 0:
        os.setsid()
        out = os.open(os.path.join(state.path, "emulate.log"), os.O_WRONLY | os.O_APPEND)
        os.dup2(os.open(os.devnull, os.O_RDONLY), 0)
        os.dup2(out, 1)
        os.dup2(out, 2)
        code = 1
        try:
            follow(state, server, name, write, remove)
            code = 0
        except BaseException as e:
            print(f"dev/emulate: {e!r}", flush=True)
            write(state, "failed", str(e))
        finally:
            os._exit(code)
    deadline = time.monotonic() + 30
    while read(state, "ready") is None:
        if read(state, "failed") is not None or os.waitpid(pid, os.WNOHANG)[0] or time.monotonic() > deadline:
            sys.exit(f"dev/emulate: can't follow {name}'s log:\n{read(state, 'emulate.log')}")
        time.sleep(0.1)

    if tree:
        if server.host:
            env = dict(os.environ, NEOCHUMBYD_DIR=server.dir)
            r = subprocess.run([os.path.join(REPO, "tools", "deploy"), server.host, "--tree", tree], env=env)
        else:
            r = subprocess.run(["rsync", "-rlpt", "--delay-updates", tree.rstrip("/") + "/",
                                os.path.join(server.dir, "tree") + "/"])
        if r.returncode:
            sys.exit("dev/emulate: deploying the tree failed")
    server.run("restart", name)
    print(f"dev/emulate: {name} restarting (via {server.host or 'this machine'}); its log: dev/emulate log")


def follow(state, server, name, write, remove):
    """Keeps STATE/log: the Chumby's log from the restart on (what came
    before is let pass), and STATE/exit once the session ends."""
    write(state, "pid", str(os.getpid()))
    proc = subprocess.Popen(server.argv("log", name, "-f"), stdout=subprocess.PIPE, stdin=subprocess.DEVNULL)
    signal.signal(signal.SIGTERM, lambda *_: (proc.kill(), os._exit(0)))
    os.set_blocking(proc.stdout.fileno(), False)
    quiet_since, started = time.monotonic(), time.monotonic()
    while time.monotonic() - quiet_since < SETTLE and time.monotonic() - started < 15:
        try:
            got = os.read(proc.stdout.fileno(), 65536)
        except BlockingIOError:
            got = None
        if got:
            quiet_since = time.monotonic()
        elif got == b"":
            raise ConnectionError("neochumbyd log ended")
        time.sleep(0.05)
    os.set_blocking(proc.stdout.fileno(), True)
    write(state, "ready", "")
    log = open(os.path.join(state.path, "log"), "ab", buffering=0)
    seen = b""
    try:
        for line in proc.stdout:
            log.write(line)
            seen = (seen + line)[-4096:]
            if ENDED.search(seen) and not os.path.exists(os.path.join(state.path, "exit")):
                write(state, "exit", "exit 0")
    finally:
        proc.kill()
        remove(state, "pid")


def stop(state, read, remove):
    server, name = server_of(read, state)
    pid = read(state, "pid")
    if pid and pid.isdigit():
        try:
            os.kill(int(pid), signal.SIGTERM)
        except ProcessLookupError:
            pass
    if name:
        server.run("restart", name, check=False)
    remove(state, "pid")


def tap(state, read, x, y, hold_ms):
    server, name = server_of(read, state)
    r = server.run("tap", name, str(x), str(y), "--hold", str(hold_ms), check=False)
    if missing(r, "tap"):
        print("dev/emulate tap: skipped: this neochumbyd has no `tap` yet")
        sys.exit(SKIPPED)
    if r.returncode:
        why = (r.stderr or r.stdout).strip()
        sys.exit(f"dev/emulate tap: {why}" + ("" if "dev=on" in why else f" (is {name} set dev=on?)"))


# dev/emulate's words for the button, in neochumbyd's: a press is down and
# up, as neochumbyd's tap.
BUTTON = {"press": "tap", "down": "press", "up": "release"}


def button(state, read, how):
    server, name = server_of(read, state)
    r = server.run("button", name, BUTTON[how], check=False)
    if missing(r, "button"):
        print("dev/emulate button: skipped: this neochumbyd has no `button` yet")
        sys.exit(SKIPPED)
    if r.returncode:
        why = (r.stderr or r.stdout).strip()
        sys.exit(f"dev/emulate button: {why}" + ("" if "dev=on" in why else f" (is {name} set dev=on?)"))


def fetch_screen(state, read):
    """The Chumby's screen, now, into STATE/fb0 as RGB565."""
    server, name = server_of(read, state)
    with tempfile.TemporaryDirectory() as t:
        png = os.path.join(t, "shot.png")
        if server.host:
            remote = f"/tmp/neochumby-emulate-{name}.png"
            server.run("shot", name, remote)
            with open(png, "wb") as f:
                subprocess.run(["ssh", "-o", "BatchMode=yes", server.host, shlex.join(["cat", remote])], stdout=f,
                               check=True)
        else:
            server.run("shot", name, png)
        with open(png, "rb") as f:
            pixels = png_to_rgb565(f.read())
    with open(os.path.join(state.path, "fb0"), "r+b") as f:
        f.write(pixels)


def png_to_rgb565(data):
    """A 320x240 8-bit RGB or RGBA PNG (neochumbyd's, or any) as RGB565."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    pos, idat, ihdr = 8, b"", None
    while pos < len(data):
        n = int.from_bytes(data[pos:pos + 4], "big")
        kind, body = data[pos + 4:pos + 8], data[pos + 8:pos + 8 + n]
        if kind == b"IHDR":
            ihdr = body
        elif kind == b"IDAT":
            idat += body
        pos += 12 + n
    w, h = int.from_bytes(ihdr[0:4], "big"), int.from_bytes(ihdr[4:8], "big")
    depth, colour, interlace = ihdr[8], ihdr[9], ihdr[12]
    if (w, h) != (W, H) or depth != 8 or colour not in (2, 6) or interlace:
        raise ValueError("not a 320x240 8-bit RGB PNG")
    bpp = 3 if colour == 2 else 4
    raw = zlib.decompress(idat)
    stride = w * bpp
    out = bytearray(w * h * 2)
    prev = bytearray(stride)
    for y in range(h):
        kind = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for i in range(stride):
            a = line[i - bpp] if i >= bpp else 0
            b = prev[i]
            c = prev[i - bpp] if i >= bpp else 0
            if kind == 1:
                line[i] = (line[i] + a) & 0xFF
            elif kind == 2:
                line[i] = (line[i] + b) & 0xFF
            elif kind == 3:
                line[i] = (line[i] + (a + b) // 2) & 0xFF
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 0xFF
        for x in range(w):
            r, g, bl = line[x * bpp], line[x * bpp + 1], line[x * bpp + 2]
            v = (r >> 3) << 11 | (g >> 2) << 5 | bl >> 3
            out[(y * w + x) * 2] = v & 0xFF
            out[(y * w + x) * 2 + 1] = v >> 8
        prev = line
    return bytes(out)


def prompt(text):
    """A step for the person beside the Chumby: shown, then Enter."""
    try:
        tty = open("/dev/tty", "r+")
    except OSError:
        sys.exit("dev/emulate prompt: needs a person at a terminal")
    with tty:
        tty.write(f"\n>>> {text}\n>>> Press Enter when done. ")
        tty.flush()
        tty.readline()
