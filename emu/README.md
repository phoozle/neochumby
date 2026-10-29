# The emulator

neochumby's emulator is QEMU with a Chumby Classic machine,
`qemu-system-arm -M chumby-classic`. It boots the Chumby's own stock
firmware (kernel, root disk and settings partition) from your own flash
backup, unmodified, to the stock control panel, with the screen, the
touchscreen, the serial console and the flash all working: for running
neochumby without the device.

The machine (an i.MX21 SoC and the Chumby's board) lives in its own
repository, [qemu-chumby](https://github.com/johnmerchant/qemu-chumby): a
series of patches against a pinned QEMU release, GPL-2.0-or-later like
QEMU. `qemu/` here builds it at a pinned commit of that repository.

The emulator models the hardware for interoperability and contains none
of the Chumby's own software. Its images come from your own device's
flash backup and stay on your machine: never in git, releases, CI or
attachments, and no screenshots of the stock artwork either. Its crypto
processor has a fixed, made-up identity and refuses key and challenge
requests; it must never impersonate a real device. Not affiliated with
or endorsed by Chumby Industries, Blue Octy or anyone holding rights in
Chumby; "Chumby" is a trademark of its owner. (Policy, not legal advice.)

This directory also holds `chumby-emu`, a host tool in Rust with no
dependencies (MIT, built apart from the neochumby workspace) that makes
the images the emulator boots.

## Building QEMU

Install QEMU's build dependencies, then run `emu/qemu/build`:

| System | Packages |
| --- | --- |
| macOS (Homebrew, with the Xcode command line tools) | `brew install python ninja pkg-config glib pixman libpng` |
| Debian, Ubuntu | `sudo apt install build-essential git python3 python3-venv ninja-build pkg-config libglib2.0-dev libpixman-1-dev libpng-dev zlib1g-dev curl xz-utils` |
| Fedora | `sudo dnf install gcc git make python3 ninja-build pkgconf glib2-devel pixman-devel libpng-devel zlib-devel curl xz patch` |

    emu/qemu/build

qemu-chumby is a submodule, `emu/qemu/qemu-chumby`, at the commit this
repository pins; `build` checks it out if it isn't already. Its own build
script fetches QEMU 11.1.2, checks its SHA-256, applies the patches and
builds only `qemu-system-arm`, into `emu/qemu/out/obj/qemu-system-arm`. A rebuild
reuses what is there. On macOS the window is Cocoa; on Linux, add the SDL
or GTK development packages for one (VNC works either way).
[qemu-chumby's README](https://github.com/johnmerchant/qemu-chumby#building)
describes other ways to build it.

## Images

Firmware never goes in git. Build a NAND image from a backup (the
`mtdN-*.nanddump` files) and a settings directory:

    cd emu
    cargo build --release
    target/release/chumby-emu mkpsp PSPDIR psp.jffs2
    target/release/chumby-emu mknand BACKUPDIR psp.jffs2 nand.img

`mknand` keeps every partition as dumped except the settings partition
(PSP), which it replaces with `psp.jffs2`, and the update cache, which it
erases. The result is the whole 64 MiB chip in nanddump's layout: a
528-byte record (512 data bytes, then 16 spare) per page. PSPDIR is a
fresh, factory-like settings set:

| File | Contents |
| --- | --- |
| `hostname` | `chumby` |
| `timezone`, `localtime` | `UTC`, and the root disk's own UTC zone file |
| `ts_settings` | `0,4095,4095,-4095,0`: the ADC's full range, which the emulated touchscreen reports against |
| `firsttime` | `0`, so the boot keeps that calibration rather than asking for one |
| `volume`, `mute`, `pan` | `50`, `0`, `0` |

Nothing else is needed. The kernel is the zImage at the start of the
Kernel1 partition:

    head -c $((0x15ad34)) BACKUPDIR/mtd4-Kernel1.bin > zImage

## Running

    emu/qemu/run zImage nand.img

which is

    qemu-system-arm -M chumby-classic -kernel zImage \
        -drive if=mtd,file=nand.img,format=raw \
        -serial stdio -qmp unix:qmp.sock,server=on,wait=off

plus any further QEMU options given after the two files. UART1, the
console, is on stdin and stdout: the kernel's messages, then a shell once
the first-run screens are done ("Please press Enter to activate this
console"). The kernel gets the stock command line unless `-append` gives
another.

The screen opens in a window; the mouse is the pen. `-display none`
runs headless, `-vnc :0` serves the screen over VNC.

The NAND image is written as the Chumby writes its flash. To keep it
unchanged, add `snapshot=on` to the drive (writes go to a temporary
overlay) or `readonly=on` (writes stay in memory):

    emu/qemu/run zImage nand.img -snapshot

Emulated time follows the host's clock, as on the device. With
`-icount shift=auto,sleep=off` it runs ahead while the Chumby is idle,
and the boot to the control panel takes a few seconds rather than half
a minute.

### Touch, the top button, screenshots and the console

`chumby-ctl` (`emu/qemu/qemu-chumby/chumby-ctl` once built) sends
touches, presses the top button and takes screenshots over the QMP
socket, in screen pixels:

    chumby-ctl qmp.sock tap 278 22          # press and release
    chumby-ctl qmp.sock down 100 120        # press and hold
    chumby-ctl qmp.sock up
    chumby-ctl qmp.sock button tap          # the top button, for 300 ms
    chumby-ctl qmp.sock button down         # hold it; button up releases
    chumby-ctl qmp.sock shot screen.png

It sends QMP's `input-send-event` (`abs` events on the `x` and `y` axes,
0 to 32767 across the screen, and the `left` button) and `screendump`
(`"format": "png"`), which any QMP client can send. The top button
(`/dev/switch` in the guest) is the machine's `button` property:
`qom-set` on `/machine`, `true` while pressed. From the human
monitor (`-monitor unix:mon.sock,server=on,wait=off`, or the window's),
`screendump screen.png -f png` writes a screenshot.

The console can be a socket too, logged to a file, for scripts:

    emu/qemu/run zImage nand.img \
        -chardev socket,id=con,path=con.sock,server=on,wait=off,logfile=console.log \
        -serial chardev:con

(`run` leaves out its own `-serial stdio` when given a `-serial`.)

The UART's receive FIFO holds 32 characters: type into it, or send long
lines in pieces.

Debugging uses QEMU's own tools: `-s -S` for gdb, `-d` for traces,
`info qtree` and `info mtree` in the monitor.

## What is modelled

The ARM926EJ-S is QEMU's. The rest is in qemu-chumby's README; in short:

| Block | State |
| --- | --- |
| AITC interrupt controller, GPT timers | Complete |
| UARTs | UART1 is the console; UART3 has the crypto processor |
| Crypto processor (CP) | Answers `cpi` as hardware 3.8 with a made-up identity (GUID `C40BB1E0-0000-4000-8000-0E4000000001`, serial `0000000000000000`); no keys |
| Clocks (CRM) | Registers, PLLs locked at the boot loader's settings |
| NAND controller and chip | Complete: reads, programs, erases, written back to the image |
| LCD controller | Background plane and graphic window, as a QEMU display |
| CSPI and TSC2100 | Touches reach the stock driver and the control panel; audio registers only |
| USB host | Control registers and an empty root hub only |
| GPIO, PWM | Complete enough for the drivers |
| Keypad port | As GPIO: the top button (`/dev/switch`) is row 0 |
| DMA, SSI audio, watchdog | Not yet: registers read back as written |
