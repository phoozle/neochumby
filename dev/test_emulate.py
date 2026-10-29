#!/usr/bin/env python3
"""Tests of dev/emulate that need neither root nor firmware: the
full-system machine's images, and its whole control path against a stub
QEMU that speaks QMP (so what the real machine would be sent is checked,
not what it would do with it). CI runs them; so can a Mac.

    python3 dev/test_emulate.py
"""
import hashlib
import json
import os
import socket
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.dont_write_bytecode = True
sys.path.insert(0, HERE)
import chumby_classic as cc  # noqa: E402

EMULATE = os.path.join(HERE, "emulate")


def fake_dir(root):
    """A settings directory with a file in a subdirectory, an empty file, a
    file over one 4 KiB node, a link and an executable: made the same way
    everywhere, modes included."""
    os.makedirs(os.path.join(root, "rfs1"))
    files = {"hostname": b"kitchen\n", "empty": b"", "rfs1/big": bytes(range(256)) * 20,
             "rfs1/run": b"#!/bin/sh\necho hi\n"}
    for name, data in files.items():
        with open(os.path.join(root, name), "wb") as f:
            f.write(data)
        os.chmod(os.path.join(root, name), 0o644)
    os.chmod(os.path.join(root, "rfs1/run"), 0o755)
    os.chmod(os.path.join(root, "rfs1"), 0o755)
    os.symlink("hostname", os.path.join(root, "name"))
    return root


# chumby-emu (emu/, #32) `mkpsp` of fake_dir: the SHA-256 of its output, so
# the port is checked against the original byte for byte.
MKPSP_SHA256 = "d8cd340cee3faf4f6716f82ed1fe14a6dbb02eb1778f8f440ab05233a56a1284"


class Images(unittest.TestCase):
    def test_crc(self):
        # JFFS2's CRC: CRC-32 from 0, not inverted. Of b"123456789", the
        # reflected 0xEDB88320 CRC with those settings is 0x2DFD2D88.
        self.assertEqual(cc.jffs2_crc(b"123456789"), 0x2DFD2D88)

    def test_mkjffs2_matches_chumby_emu(self):
        with tempfile.TemporaryDirectory() as t:
            image = cc.mkjffs2(fake_dir(os.path.join(t, "psp")), cc.PSP[1])
        self.assertEqual(len(image), cc.PSP[1])
        self.assertEqual(image[:2], b"\x85\x19")
        self.assertEqual(hashlib.sha256(image).hexdigest(), MKPSP_SHA256)

    def test_too_big(self):
        with tempfile.TemporaryDirectory() as t:
            with self.assertRaises(ValueError):
                cc.mkjffs2(fake_dir(os.path.join(t, "psp")), 4096)

    def test_patch_nand(self):
        with tempfile.TemporaryDirectory() as t:
            nand = os.path.join(t, "nand.img")
            with open(nand, "wb") as f:
                f.truncate(cc.NAND_BYTES)
            psp = bytes([1]) * cc.PSP[1]
            cc.patch_nand(nand, psp, None)
            with open(nand, "rb") as f:
                chip = f.read()
        at = cc.PSP[0] // cc.PAGE * cc.RECORD
        records = cc.PSP[1] // cc.PAGE * cc.RECORD
        self.assertEqual(chip[at - 1], 0, "the partition before is untouched")
        self.assertEqual(chip[at:at + 512], bytes([1]) * 512)
        self.assertEqual(chip[at + 512:at + 528], b"\xff" * 16, "a blank spare area")
        self.assertEqual(chip[at + records], 0, "the partition after is untouched")
        self.assertEqual(chip.count(1), cc.PSP[1])

    def test_patch_nand_refuses_wrong_size(self):
        with tempfile.NamedTemporaryFile() as f:
            f.truncate(1024)
            with self.assertRaises(ValueError):
                cc.patch_nand(f.name, None, None)

    def test_touch_events(self):
        ev = cc.touch_events(0, 239)
        self.assertEqual(ev[0], {"type": "abs", "data": {"axis": "x", "value": 51}})
        self.assertEqual(ev[1]["data"]["value"], 32698)
        self.assertEqual(ev[2], {"type": "btn", "data": {"button": "left", "down": True}})
        self.assertLessEqual(cc.axis(319, 320), 32767)

    def test_ppm(self):
        ppm = b"P6\n320 240\n255\n" + bytes([255, 0, 8]) * (320 * 240)
        out = cc.ppm_to_rgb565(ppm)
        self.assertEqual(len(out), 320 * 240 * 2)
        self.assertEqual(int.from_bytes(out[:2], "little"), 0xF801)


# A stand-in for qemu-system-arm -M chumby-classic: checks its command
# line and the NAND image it's given, logs a boot to the console's logfile,
# and answers QMP, recording each input event.
STUB = textwrap.dedent(r'''
    #!/usr/bin/env python3
    import json, os, socket, sys
    a = sys.argv[1:]
    def opt(name):
        return a[a.index(name) + 1]
    assert opt("-M") == "chumby-classic" and opt("-display") == "none"
    drive = dict(kv.split("=", 1) for kv in opt("-drive").split(","))
    qmp = opt("-qmp").split(",")[0].removeprefix("unix:")
    chardev = dict(kv.split("=", 1) for kv in opt("-chardev").split(",")[1:])
    log = open(chardev["logfile"], "ab", buffering=0)
    nand = drive["file"]
    psp = 0x100000 // 512 * 528
    with open(nand, "rb") as f:
        f.seek(psp)
        magic = f.read(2)
    log.write(b"stub: nand %d bytes, psp magic %s\n" % (os.path.getsize(nand), magic.hex().encode()))
    log.write(b"emutest: stick mounted\nneochumby: opening home\n")
    events = open(os.environ["STUB_EVENTS"], "a")
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.bind(qmp)
    s.listen(1)
    while True:
        c, _ = s.accept()
        f = c.makefile("rwb", buffering=0)
        f.write(b'{"QMP": {"version": {}, "capabilities": []}}\n')
        for line in f:
            m = json.loads(line)
            cmd, args = m["execute"], m.get("arguments", {})
            reply = {"return": {}}
            if cmd == "input-send-event":
                for e in args["events"]:
                    events.write(json.dumps(e) + "\n")
                events.flush()
                if args["events"][-1] == {"type": "btn", "data": {"button": "left", "down": False}}:
                    log.write(b"neochumby: home asks: chumby-mode\nneochumby exited: 0\n")
            elif cmd == "screendump":
                assert os.path.isabs(args["filename"]) and args["format"] == "ppm"
                with open(args["filename"], "wb") as out:
                    out.write(b"P6\n320 240\n255\n" + bytes([16, 16, 33]) * (320 * 240))
            elif cmd == "query-status":
                reply = {"return": {"status": "running", "running": True}}
            f.write(b'{"event": "NOISE"}\n')
            f.write(json.dumps(reply).encode() + b"\n")
            if cmd == "quit":
                sys.exit(0)
''').lstrip()


class Machine(unittest.TestCase):
    def setUp(self):
        # Short: a Unix socket's path is limited to about 100 bytes.
        self.t = tempfile.mkdtemp(prefix="emu.", dir="/tmp")
        self.state = os.path.join(self.t, "s")
        self.stub = os.path.join(self.t, "qemu")
        with open(self.stub, "w") as f:
            f.write(STUB)
        os.chmod(self.stub, 0o755)
        self.events = os.path.join(self.t, "events")
        self.kernel = os.path.join(self.t, "zImage")
        open(self.kernel, "wb").close()
        self.nand = os.path.join(self.t, "nand.img")
        with open(self.nand, "wb") as f:
            f.truncate(cc.NAND_BYTES)
        self.psp = fake_dir(os.path.join(self.t, "psp"))
        self.tree = os.path.join(self.t, "tree")
        os.makedirs(os.path.join(self.tree, "apps", "home"))
        for name in ("neochumby", "userhook2", "apps/home/neochumby-home"):
            with open(os.path.join(self.tree, name), "w") as f:
                f.write("#!/bin/sh\n")
        self.env = dict(os.environ, STUB_EVENTS=self.events, NEOCHUMBY_EMU_MACHINE="chumby-classic")
        for k in ("NEOCHUMBY_EMU_KERNEL", "NEOCHUMBY_EMU_NAND", "NEOCHUMBY_EMU_PSP", "QEMU", "NEOCHUMBY_EMU"):
            self.env.pop(k, None)

    def tearDown(self):
        self.emu("stop")
        subprocess.run(["rm", "-rf", self.t])

    def emu(self, *args):
        return subprocess.run([EMULATE, "--state", self.state, *args], env=self.env, capture_output=True,
                              text=True, timeout=60)

    def start(self):
        r = self.emu("start", self.tree, "--kernel", self.kernel, "--nand", self.nand, "--factory-psp", self.psp,
                     "--qemu", self.stub)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_skipped_without_firmware(self):
        r = self.emu("start", self.tree)
        self.assertEqual(r.returncode, 77)
        self.assertIn("needs your own firmware", r.stdout)

    def test_a_session(self):
        self.start()
        self.assertEqual(self.emu("wait", "log", "neochumby: opening home", "20").returncode, 0)
        log = self.emu("log").stdout
        # The copy it booted: the right size, the settings partition JFFS2.
        self.assertIn(f"stub: nand {cc.NAND_BYTES} bytes, psp magic 8519", log)
        self.assertEqual(os.path.getsize(self.nand), cc.NAND_BYTES)
        status = self.emu("status").stdout
        self.assertIn("machine: chumby-classic", status)
        self.assertIn("qemu: running", status)
        self.assertEqual(self.emu("pixel", "0", "0").stdout.strip(), "#101021")
        self.assertEqual(self.emu("shot", os.path.join(self.t, "s.png")).returncode, 0)
        with open(os.path.join(self.t, "s.png"), "rb") as f:
            self.assertEqual(f.read(8), b"\x89PNG\r\n\x1a\n")
        r = self.emu("button", "press")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("isn't driven", r.stderr)
        self.assertEqual(self.emu("exec", "/bin/true").returncode, 77)
        self.assertEqual(self.emu("tap", "160", "120").returncode, 0)
        with open(self.events) as f:
            sent = [json.loads(line) for line in f]
        self.assertEqual(sent, cc.touch_events(160, 120) + cc.RELEASE_EVENTS)
        r = self.emu("wait", "exit", "20")
        self.assertEqual(r.stdout.strip(), "exit 0", r.stdout + r.stderr)
        self.assertEqual(self.emu("stop").returncode, 0)
        deadline = time.monotonic() + 10
        while "running" in self.emu("status").stdout.split("\n")[0] and time.monotonic() < deadline:
            time.sleep(0.2)
        self.assertNotEqual(self.emu("status").stdout.split("\n")[0], "running")

    def test_settings_and_stick(self):
        """What the images hold: built again here from the same inputs, and
        compared with the copy the machine was given."""
        extra = os.path.join(self.t, "extra")
        os.makedirs(extra)
        with open(os.path.join(extra, "neochumby.conf"), "w") as f:
            f.write("start=classic\n")
        r = self.emu("start", self.tree, "--kernel", self.kernel, "--nand", self.nand, "--factory-psp", self.psp,
                     "--qemu", self.stub, "--psp", extra)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        want = os.path.join(self.t, "want.img")
        cc.build_nand(want, self.nand, self.psp, extra, self.tree)
        with open(want, "rb") as a, open(os.path.join(self.state, "nand.img"), "rb") as b:
            self.assertEqual(hashlib.sha256(a.read()).digest(), hashlib.sha256(b.read()).digest())
        with tempfile.TemporaryDirectory() as w:
            psp = cc.psp_dir(w, self.psp, extra)
            for name in ("disable_intro", "neochumby.conf", "hostname", "rfs1/big", "rfs1/userhook2"):
                self.assertTrue(os.path.exists(os.path.join(psp, name)), name)
            with open(os.path.join(psp, "ts_settings")) as f:
                self.assertEqual(f.read().strip(), cc.NEUTRAL_CALIBRATION)
            stick = cc.stick_dir(w, self.tree)
            self.assertTrue(os.path.exists(os.path.join(stick, "userhook2")))
            self.assertTrue(os.path.exists(os.path.join(stick, "neochumby", "apps", "home", "neochumby-home")))


# A stand-in for neochumbyd's developer link, in its state directory as
# tools/deploy installs it: one paired Chumby, "kitchen", whose log is a
# file (with a line from before, which dev/emulate must let pass).
FAKE_NEOCHUMBYD = textwrap.dedent(r'''
    #!/usr/bin/env python3
    import os, struct, sys, time, zlib
    a = sys.argv[1:]
    state = a[a.index("--state") + 1]
    a = a[:a.index("--state")]
    log = os.path.join(state, "fake-log")
    def say(line):
        with open(log, "a") as f:
            f.write(line + "\n")
    def usage():
        # As neochumbyd's: an older one's lacks tap and button.
        cmds = "serve|log NAME [-f]|restart NAME|shot NAME OUT.png"
        if not os.environ.get("FAKE_WITHOUT_INPUT"):
            cmds += "|tap NAME X Y [--hold MS]|button NAME press|release|tap"
        print(f"usage: neochumbyd {cmds} [--state DIR]", file=sys.stderr)
        sys.exit(2)
    cmd, name = a[0], a[1] if len(a) > 1 else ""
    if name != "kitchen":
        print(f"no Chumby paired as {name}", file=sys.stderr)
        sys.exit(1)
    if cmd == "log":
        with open(log) as f:
            sys.stdout.write(f.read())
            sys.stdout.flush()
            while "-f" in a:
                line = f.readline()
                if line:
                    sys.stdout.write(line)
                    sys.stdout.flush()
                else:
                    time.sleep(0.05)
    elif cmd == "restart":
        say("neochumby: opening home")
        print(f"{name}: restarting")
    elif cmd == "shot":
        rows = b"".join(b"\x00" + bytes([16, 16, 33]) * 320 for _ in range(240))
        def chunk(t, d):
            return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
        png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 320, 240, 8, 2, 0, 0, 0))
        png += chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")
        open(a[2], "wb").write(png)
    elif cmd in ("tap", "button"):
        if os.environ.get("FAKE_WITHOUT_INPUT"):
            usage()
        if os.environ.get("FAKE_DEV_OFF"):
            print(f"neochumbyd: {name} takes no taps or button presses from here: its owner turns that on "
                  f"with `neochumbyd set {name} dev=on` (and off with `dev=`)", file=sys.stderr)
            sys.exit(1)
        if cmd == "button" and a[2:] not in (["press"], ["release"], ["tap"]):
            usage()
        with open(os.path.join(state, "fake-input"), "a") as f:
            f.write(" ".join(a) + "\n")
        if a[:4] == ["tap", "kitchen", "160", "220"]:
            say("neochumby: home asks: chumby-mode")
            say("neochumby: CHUMBY MODE")
    else:
        usage()
''').lstrip()


class Device(unittest.TestCase):
    def setUp(self):
        self.t = tempfile.mkdtemp(prefix="emu.", dir="/tmp")
        self.nd = os.path.join(self.t, "nd")
        os.makedirs(os.path.join(self.nd, "tree"))
        exe = os.path.join(self.nd, "neochumbyd")
        with open(exe, "w") as f:
            f.write(FAKE_NEOCHUMBYD)
        os.chmod(exe, 0o755)
        with open(os.path.join(self.nd, "fake-log"), "w") as f:
            f.write("neochumby: opening home\nneochumby: CHUMBY MODE\n")
        self.tree = os.path.join(self.t, "tree")
        os.makedirs(self.tree)
        with open(os.path.join(self.tree, "neochumby"), "w") as f:
            f.write("session\n")
        self.env = dict(os.environ, NEOCHUMBYD_DIR=self.nd, NEOCHUMBY_EMU_DEVICE="kitchen")
        for k in ("NEOCHUMBY_EMU_MACHINE", "NEOCHUMBY_EMU_HOST", "NEOCHUMBY_EMU", "NEOCHUMBY_EMU_SLOW"):
            self.env.pop(k, None)
        self.state = os.path.join(self.t, "s")

    def tearDown(self):
        self.emu("stop")
        subprocess.run(["rm", "-rf", self.t])

    def emu(self, *args, env=None):
        return subprocess.run([EMULATE, "--state", self.state, *args], env=env or self.env, capture_output=True,
                              text=True, timeout=60)

    def test_backends(self):
        self.assertEqual(self.emu("backend").stdout.strip(), "device")
        self.assertEqual(self.emu("scale").stdout.strip(), "5")
        user = {k: v for k, v in self.env.items() if k != "NEOCHUMBY_EMU_DEVICE"}
        self.assertEqual(self.emu("backend", env=user).stdout.strip(), "user")
        self.assertEqual(self.emu("scale", env=user).stdout.strip(), "1")
        self.assertEqual(self.emu("prompt", "pull the power", env=user).returncode, 77)
        self.assertEqual(self.emu("scale", env=dict(user, NEOCHUMBY_EMU_MACHINE="chumby-classic")).stdout.strip(), "10")

    def test_unpaired(self):
        r = self.emu("start", self.tree, env=dict(self.env, NEOCHUMBY_EMU_DEVICE="lounge"))
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("no Chumby paired as lounge", r.stderr)

    def test_a_session(self):
        r = self.emu("start", self.tree)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        with open(os.path.join(self.nd, "tree", "neochumby")) as f:
            self.assertEqual(f.read(), "session\n", "the tree was installed")
        self.assertEqual(self.emu("wait", "log", "opening home", "10").returncode, 0)
        # Only the restart's line: the history before it was let pass.
        self.assertEqual(self.emu("log").stdout, "neochumby: opening home\n")
        self.assertIn("kitchen, a Chumby", self.emu("status").stdout)
        self.assertEqual(self.emu("pixel", "5", "5").stdout.strip(), "#101021")
        self.assertEqual(self.emu("exec", "/bin/true").returncode, 77)
        # dev/emulate's press, down and up are neochumbyd's tap, press and release.
        for how in ("press", "down", "up"):
            self.assertEqual(self.emu("button", how).returncode, 0)
        self.assertEqual(self.emu("tap", "160", "220", "--hold", "150").returncode, 0)
        with open(os.path.join(self.nd, "fake-input")) as f:
            self.assertEqual(f.read(), "button kitchen tap\nbutton kitchen press\nbutton kitchen release\n"
                                       "tap kitchen 160 220 --hold 150\n")
        self.assertEqual(self.emu("wait", "exit", "10").stdout.strip(), "exit 0")

    def test_without_remote_input(self):
        self.assertEqual(self.emu("start", self.tree).returncode, 0)
        env = dict(self.env, FAKE_WITHOUT_INPUT="1")
        r = self.emu("tap", "1", "1", env=env)
        self.assertEqual(r.returncode, 77)
        self.assertIn("no `tap` yet", r.stdout)
        self.assertEqual(self.emu("button", "press", env=env).returncode, 77)

    def test_input_refused(self):
        """A Chumby not set dev=on: a failure, saying so, not a skip."""
        self.assertEqual(self.emu("start", self.tree).returncode, 0)
        r = self.emu("tap", "1", "1", env=dict(self.env, FAKE_DEV_OFF="1"))
        self.assertEqual(r.returncode, 1)
        self.assertIn("neochumbyd set kitchen dev=on", r.stderr)
        self.assertNotIn("(is kitchen set dev=on?)", r.stderr)

    def test_png(self):
        import zlib
        # Each of PNG's filters, on rows that need them.
        rows = b""
        for y in range(240):
            rows += bytes([y % 5]) + bytes([(x * 7 + y) & 0xFF for x in range(320 * 3)])
        png = b"\x89PNG\r\n\x1a\n"
        for t, d in ((b"IHDR", (320).to_bytes(4, "big") + (240).to_bytes(4, "big") + bytes([8, 2, 0, 0, 0])),
                     (b"IDAT", zlib.compress(rows)), (b"IEND", b"")):
            png += len(d).to_bytes(4, "big") + t + d + zlib.crc32(t + d).to_bytes(4, "big")
        import device
        out = device.png_to_rgb565(png)
        self.assertEqual(len(out), 320 * 240 * 2)
        # Row 0 is unfiltered: its first pixel is (0, 7, 14).
        self.assertEqual(int.from_bytes(out[:2], "little"), (0 >> 3) << 11 | (7 >> 2) << 5 | 14 >> 3)


if __name__ == "__main__":
    unittest.main()
