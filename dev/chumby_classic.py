"""dev/emulate's full-system machine, `--machine chumby-classic`: QEMU's
chumby-classic machine (qemu-chumby, built by emu/qemu/build) booting the
Chumby's own firmware from your own device's flash backup, with a tree
on it as a neochumby stick would carry it.

- The images: a copy of your NAND image with two partitions rewritten for
  each boot. The settings (PSP) are your factory-like settings directory,
  plus `disable_intro`, the machine's neutral touch calibration, the test
  boot hook as `rfs1/userhook2`, and whatever `--psp DIR` adds. The Cache
  partition (24 MiB) is a JFFS2 image laid out as tools/stick lays out a
  stick: the tree's `userhook2`, and the tree as `neochumby/`. The machine
  has no USB, so the test hook mounts the Cache partition at /mnt/usb and
  runs the stick's hook from it, unchanged.
- The machine runs through emu/qemu/run when it is there (else the same
  command line): headless, the console's output logged to STATE/log, QMP
  on STATE/qmp.sock. A supervisor serves the same control socket as user
  mode, in QMP's terms: touches are `input-send-event` on the absolute
  axes, the screen is `screendump`, stop is `quit`.
- The session's end is the boot hook's "neochumby exited: N" on the
  console.

Firmware comes only from your own backup, by flag or environment, and
stays on your machine (docs/legal.md): never in the repository, CI or an
attachment. Without it `start` exits 77, which the scenarios take as
skipped. Nothing here needs root.
"""

import json
import os
import re
import select
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import zlib

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MACHINE = "chumby-classic"
SKIPPED = 77

# The NAND chip, as nanddump and the machine store it: 131072 records of a
# 512-byte page and its 16 spare bytes.
PAGE, SPARE = 512, 16
RECORD = PAGE + SPARE
PAGES = 131072
NAND_BYTES = PAGES * RECORD
# The partitions rewritten for a boot: (offset, size), as in emu/src/image.rs.
PSP = (0x0100000, 0x0200000)
CACHE = (0x2680000, 0x1800000)
ERASE_BLOCK = 0x4000
# A fixed time for every JFFS2 node, so that images are reproducible.
JFFS2_TIME = 1230768000

# The machine's touchscreen spans the ADC's full range, which this
# calibration maps onto the screen (qemu-chumby's README).
NEUTRAL_CALIBRATION = "0,4095,4095,-4095,0"
W, H = 320, 240

TEST_HOOK = os.path.join(REPO, "dev", "emulate-userhook2")
# The boot hook's line when the session returns (boot/userhook2).
EXITED = re.compile(rb"neochumby exited: (\d+)")


# JFFS2, as emu/src/image.rs writes it: little-endian, 16 KiB erase blocks,
# uncompressed, no clean markers.


def jffs2_crc(data):
    """CRC-32 from 0 with no final inversion, as JFFS2 has it."""
    return zlib.crc32(data, 0xFFFFFFFF) ^ 0xFFFFFFFF


S_IFDIR, S_IFREG, S_IFLNK = 0o040000, 0o100000, 0o120000
DT_DIR, DT_REG, DT_LNK = 4, 8, 10


class Jffs2:
    def __init__(self):
        self.out = bytearray()
        self.next_ino = 2

    def push(self, node):
        size = (len(node) + 3) & ~3
        room = ERASE_BLOCK - len(self.out) % ERASE_BLOCK
        if size > room:
            self.out += b"\xff" * room
        self.out += node + bytes(size - len(node))

    @staticmethod
    def header(kind, total):
        h = (0x1985).to_bytes(2, "little") + kind.to_bytes(2, "little") + total.to_bytes(4, "little")
        return h + jffs2_crc(h).to_bytes(4, "little")

    def dirent(self, pino, version, ino, name, dtype):
        n = self.header(0xE001, 40 + len(name))
        for v in (pino, version, ino, JFFS2_TIME):
            n += v.to_bytes(4, "little")
        n += bytes([len(name), dtype, 0, 0])
        n += jffs2_crc(n[:32]).to_bytes(4, "little") + jffs2_crc(name).to_bytes(4, "little") + name
        self.push(n)

    def inode(self, ino, version, mode, isize, offset, data):
        n = self.header(0xE002, 68 + len(data))
        for v in (ino, version, mode):
            n += v.to_bytes(4, "little")
        n += bytes(4)  # uid, gid: root
        for v in (isize, JFFS2_TIME, JFFS2_TIME, JFFS2_TIME, offset, len(data), len(data)):
            n += v.to_bytes(4, "little")
        n += bytes(4)  # no compression, flags 0
        n += jffs2_crc(data).to_bytes(4, "little")
        n += jffs2_crc(n[:60]).to_bytes(4, "little") + data
        self.push(n)

    def add_dir(self, path, ino):
        for version, name in enumerate(sorted(os.listdir(path), key=os.fsencode), 1):
            child_path = os.path.join(path, name)
            raw = os.fsencode(name)
            st = os.lstat(child_path)
            perm = st.st_mode & 0o7777
            child = self.next_ino
            self.next_ino += 1
            if os.path.isdir(child_path) and not os.path.islink(child_path):
                self.inode(child, 1, S_IFDIR | perm, 0, 0, b"")
                self.dirent(ino, version, child, raw, DT_DIR)
                self.add_dir(child_path, child)
            elif os.path.islink(child_path):
                target = os.fsencode(os.readlink(child_path))
                self.inode(child, 1, S_IFLNK | 0o777, len(target), 0, target)
                self.dirent(ino, version, child, raw, DT_LNK)
            else:
                with open(child_path, "rb") as f:
                    data = f.read()
                if not data:
                    self.inode(child, 1, S_IFREG | perm, 0, 0, b"")
                for i in range(0, len(data), 4096):
                    self.inode(child, 1 + i // 4096, S_IFREG | perm, len(data), i, data[i:i + 4096])
                self.dirent(ino, version, child, raw, DT_REG)


def mkjffs2(path, size):
    """A JFFS2 image of the directory, padded with erased flash to `size`."""
    j = Jffs2()
    j.add_dir(path, 1)
    if len(j.out) > size:
        raise ValueError(f"{path} doesn't fit: {len(j.out)} bytes of JFFS2 in a {size}-byte partition")
    return bytes(j.out) + b"\xff" * (size - len(j.out))


def with_spare(data):
    """Plain data as the chip's records, with blank spare areas."""
    return b"".join(data[i:i + PAGE] + b"\xff" * SPARE for i in range(0, len(data), PAGE))


def patch_nand(nand, psp_image, cache_image):
    """Rewrites the settings and Cache partitions of a NAND image file in
    place: each image is a partition's data, or None to leave it."""
    if os.path.getsize(nand) != NAND_BYTES:
        raise ValueError(f"{nand}: not a NAND image of {NAND_BYTES} bytes")
    with open(nand, "r+b") as f:
        for (offset, size), data in ((PSP, psp_image), (CACHE, cache_image)):
            if data is None:
                continue
            assert len(data) == size
            f.seek(offset // PAGE * RECORD)
            f.write(with_spare(data))


# The images for one boot.


def psp_dir(work, factory, extra):
    """The settings for a boot: the factory-like set, then `extra`'s files
    over it, with the intro off, the machine's calibration, and the test
    boot hook."""
    psp = os.path.join(work, "psp")
    shutil.copytree(factory, psp, symlinks=True)
    if extra:
        shutil.copytree(extra, psp, symlinks=True, dirs_exist_ok=True)
    open(os.path.join(psp, "disable_intro"), "w").close()
    with open(os.path.join(psp, "ts_settings"), "w") as f:
        f.write(NEUTRAL_CALIBRATION + "\n")
    os.makedirs(os.path.join(psp, "rfs1"), exist_ok=True)
    hook = os.path.join(psp, "rfs1", "userhook2")
    shutil.copy(TEST_HOOK, hook)
    os.chmod(hook, 0o755)
    return psp


def stick_dir(work, tree):
    """The Cache partition's contents, as tools/stick lays out a stick."""
    stick = os.path.join(work, "stick")
    os.makedirs(stick)
    if tree:
        shutil.copytree(tree, os.path.join(stick, "neochumby"), symlinks=True)
        shutil.copy(os.path.join(tree, "userhook2"), os.path.join(stick, "userhook2"))
        os.chmod(os.path.join(stick, "userhook2"), 0o755)
    return stick


def build_nand(out, nand, factory_psp, extra_psp, tree):
    shutil.copyfile(nand, out)
    with tempfile.TemporaryDirectory() as work:
        psp = mkjffs2(psp_dir(work, factory_psp, extra_psp), PSP[1])
        cache = mkjffs2(stick_dir(work, tree), CACHE[1])
    patch_nand(out, psp, cache)


# The machine.


def find_qemu(given):
    """The qemu-system-arm with the machine: given, $QEMU, emu/qemu/build's,
    else one on PATH that has it; or None."""
    for q in (given, os.environ.get("QEMU"), os.path.join(REPO, "emu", "qemu", "build", "obj", "qemu-system-arm")):
        if q and os.access(q, os.X_OK):
            return q
    q = shutil.which("qemu-system-arm")
    if q:
        r = subprocess.run([q, "-machine", "help"], capture_output=True, text=True)
        if re.search(rf"^{MACHINE}\s", r.stdout, re.M):
            return q
    return None


def command_line(qemu, kernel, nand, state_path, fast):
    """How the machine is run: emu/qemu/run's command line (through it, when
    this checkout has it), headless, the console logged, QMP in STATE."""
    console = (f"socket,id=con,path={state_path}/console.sock,server=on,wait=off,"
               f"logfile={state_path}/log,logappend=on")
    extra = ["-chardev", console, "-serial", "chardev:con", "-display", "none"]
    if fast:
        extra += ["-icount", "shift=auto,sleep=off"]
    run = os.path.join(REPO, "emu", "qemu", "run")
    if os.access(run, os.X_OK):
        # run puts QMP at ./qmp.sock: the supervisor runs it in STATE.
        return [run, kernel, nand] + extra, {"QEMU": qemu}
    return [qemu, "-M", MACHINE, "-kernel", kernel, "-drive", f"if=mtd,file={nand},format=raw",
            "-qmp", f"unix:{state_path}/qmp.sock,server=on,wait=off"] + extra, {}


class Qmp:
    def __init__(self, path, deadline):
        while True:
            try:
                self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                self.sock.connect(path)
                break
            except OSError:
                self.sock.close()
                if time.monotonic() > deadline:
                    raise
                time.sleep(0.1)
        self.buf = b""
        self.reply()  # the greeting
        self.run("qmp_capabilities")

    def line(self):
        while b"\n" not in self.buf:
            got = self.sock.recv(65536)
            if not got:
                raise ConnectionError("QMP closed")
            self.buf += got
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def reply(self):
        """The next message that isn't an event."""
        while True:
            m = self.line()
            if "event" not in m:
                return m

    def run(self, command, **arguments):
        msg = {"execute": command}
        if arguments:
            msg["arguments"] = arguments
        self.sock.sendall(json.dumps(msg).encode() + b"\n")
        r = self.reply()
        if "error" in r:
            raise RuntimeError(f"{command}: {r['error'].get('desc', r['error'])}")
        return r.get("return")


def axis(pixel, span):
    """A screen pixel's centre on QEMU's absolute axis (0-32767)."""
    return min(32767, (2 * pixel + 1) * 32767 // (2 * span))


def touch_events(x, y):
    return [{"type": "abs", "data": {"axis": "x", "value": axis(x, W)}},
            {"type": "abs", "data": {"axis": "y", "value": axis(y, H)}},
            {"type": "btn", "data": {"button": "left", "down": True}}]


RELEASE_EVENTS = [{"type": "btn", "data": {"button": "left", "down": False}}]


def ppm_to_rgb565(ppm):
    """A binary PPM of the 320x240 screen as the framebuffer's RGB565."""
    m = re.match(rb"P6\s+(\d+)\s+(\d+)\s+(\d+)\s", ppm)
    if not m or (int(m.group(1)), int(m.group(2))) != (W, H) or m.group(3) != b"255":
        raise ValueError("not a 320x240 8-bit PPM")
    px = ppm[m.end():m.end() + W * H * 3]
    out = bytearray(W * H * 2)
    for i in range(W * H):
        r, g, b = px[3 * i], px[3 * i + 1], px[3 * i + 2]
        v = (r >> 3) << 11 | (g >> 2) << 5 | b >> 3
        out[2 * i] = v & 0xFF
        out[2 * i + 1] = v >> 8
    return bytes(out)


def start(state, opts, tree, write, read, remove):
    """Builds the images and starts the supervisor; exits 77 without the
    firmware or the machine."""
    kernel = opts.get("kernel") or os.environ.get("NEOCHUMBY_EMU_KERNEL")
    nand = opts.get("nand") or os.environ.get("NEOCHUMBY_EMU_NAND")
    factory = opts.get("factory_psp") or os.environ.get("NEOCHUMBY_EMU_PSP")
    missing = [what for what, v in (("--kernel or NEOCHUMBY_EMU_KERNEL", kernel), ("--nand or NEOCHUMBY_EMU_NAND", nand),
                                    ("--factory-psp or NEOCHUMBY_EMU_PSP", factory)) if not v or not os.path.exists(v)]
    if missing:
        print(f"dev/emulate: skipped: --machine {MACHINE} needs your own firmware: {', '.join(missing)}")
        sys.exit(SKIPPED)
    qemu = find_qemu(opts.get("qemu"))
    if not qemu:
        print(f"dev/emulate: skipped: no qemu-system-arm with the {MACHINE} machine (emu/qemu/build, or --qemu)")
        sys.exit(SKIPPED)
    for unsupported in ("stick", "binds", "run", "server"):
        if opts.get(unsupported):
            sys.exit(f"dev/emulate: --{unsupported} isn't available with --machine {MACHINE}")
    path = state.path
    nand_copy = os.path.join(path, "nand.img")
    try:
        build_nand(nand_copy, nand, factory, opts.get("psp"), tree)
    except ValueError as e:
        sys.exit(f"dev/emulate: {e}")
    argv, env = command_line(qemu, os.path.abspath(kernel), nand_copy, path, opts.get("fast"))
    write(state, "machine", MACHINE)
    write(state, "fb0", "", size=W * H * 2)
    for name in ("exit", "ready", "failed", "pid", "qmp.sock", "console.sock"):
        remove(state, name)
    write(state, "log", "")
    write(state, "emulate.log", "")

    pid = os.fork()
    if pid == 0:
        os.setsid()
        out = os.open(os.path.join(path, "emulate.log"), os.O_WRONLY | os.O_APPEND)
        os.dup2(os.open(os.devnull, os.O_RDONLY), 0)
        os.dup2(out, 1)
        os.dup2(out, 2)
        code = 1
        try:
            supervise(state, argv, env, write, remove)
            code = 0
        except BaseException as e:
            print(f"dev/emulate: {e!r}", flush=True)
            write(state, "failed", str(e))
        finally:
            os._exit(code)
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if read(state, "ready") is not None:
            print(f"dev/emulate: {MACHINE} booting ({path}); the console: dev/emulate log")
            return
        if read(state, "failed") is not None or os.waitpid(pid, os.WNOHANG)[0]:
            break
        time.sleep(0.1)
    sys.exit(f"dev/emulate: failed to start:\n{read(state, 'emulate.log')}")


def supervise(state, argv, env, write, remove):
    path = state.path
    write(state, "pid", str(os.getpid()))
    machine = subprocess.Popen(argv, cwd=path, env=dict(os.environ, **env), stdin=subprocess.DEVNULL)
    try:
        qmp = Qmp(os.path.join(path, "qmp.sock"), time.monotonic() + 30)
        ctl = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        here = os.getcwd()
        os.chdir(path)
        try:
            ctl.bind("ctl")
        finally:
            os.chdir(here)
        os.chmod(os.path.join(path, "ctl"), 0o600)
        ctl.listen(4)
        write(state, "ready", "")
        exited = None
        while machine.poll() is None:
            ready, _, _ = select.select([ctl, qmp.sock], [], [], 0.5)
            if qmp.sock in ready:
                # Events, between commands: read and dropped.
                got = qmp.sock.recv(65536)
                if not got:
                    break
                qmp.buf += got
                while b"\n" in qmp.buf:
                    _, qmp.buf = qmp.buf.split(b"\n", 1)
            if ctl in ready:
                conn, _ = ctl.accept()
                with conn:
                    conn.settimeout(10)
                    try:
                        words = conn.recv(256).decode().split()
                        conn.sendall((command(qmp, path, words) + "\n").encode())
                    except (OSError, ValueError, IndexError, RuntimeError) as e:
                        conn.sendall(f"error {e}\n".encode())
            if exited is None:
                with open(os.path.join(path, "log"), "rb") as f:
                    m = EXITED.search(f.read())
                if m:
                    exited = int(m.group(1))
                    write(state, "exit", f"exit {exited}")
        if exited is None:
            write(state, "exit", f"qemu exited {machine.wait()}")
    finally:
        if machine.poll() is None:
            machine.send_signal(signal.SIGTERM)
            machine.wait()
        remove(state, "pid")


def command(qmp, path, words):
    if words[0] == "touch":
        qmp.run("input-send-event", events=touch_events(int(words[1]), int(words[2])))
    elif words[0] == "release":
        qmp.run("input-send-event", events=RELEASE_EVENTS)
    elif words[0] == "button":
        raise ValueError(f"the top button isn't driven in --machine {MACHINE} yet")
    elif words[0] == "screen":
        ppm = os.path.join(path, "screen.ppm")
        qmp.run("screendump", filename=ppm, format="ppm")
        with open(ppm, "rb") as f:
            data = ppm_to_rgb565(f.read())
        with open(os.path.join(path, "fb0"), "r+b") as f:
            f.write(data)
    elif words[0] == "status":
        s = qmp.run("query-status")
        return "ok " + json.dumps({"machine": MACHINE, "qemu": s.get("status")})
    elif words[0] == "stop":
        try:
            qmp.run("quit")
        except ConnectionError:
            pass
    else:
        raise ValueError(f"unknown command {words[0]}")
    return "ok"
