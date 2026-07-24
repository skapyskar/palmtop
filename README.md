# Palmtop

Use your Linux or Windows laptop from your Android phone. The laptop's screen
streams to the phone; you tap and type on the phone and it lands on the
laptop.

Everything runs directly between your two devices. There is no account, no
cloud service, and no server anyone else operates — the video never leaves
your network, and the connection is encrypted end to end.

Same phone app either way — you just download the host package that matches
your laptop's OS.

**Status:** working and used daily, but early. Linux (Wayland) is battle-
tested; Windows support is new and has not yet had the same live-device
verification the Linux path has — see [Known limits](#known-limits).

---

## What you need

| | |
|---|---|
| **Laptop** | **Linux** with a Wayland desktop (GNOME, KDE, Hyprland, Sway, …) — or **Windows 10** version 1903+ / **Windows 11** |
| **Phone** | Android 11 or newer |
| **Both** | On the same Wi-Fi — *or* the laptop joined to the phone's hotspot |
| **Laptop packages** | `ffmpeg` — already installed on most Linux systems; bundled in the Windows download |

X11 is not supported on Linux. To check which you have, run
`echo $XDG_SESSION_TYPE` on the laptop — it should print `wayland`.

---

## Setup

### 1. Install the app on your phone

Download the `.apk` from the [latest release](../../releases/latest) and open it.

Android will warn you about installing from an unknown source. That is
expected — the app is not distributed through the Play Store, so your phone
has no way to vouch for it. You are trusting this repository instead.

### 2. Install the host on your laptop

Pick the tab for your laptop's OS.

<details open>
<summary><b>Linux</b></summary>

Download the `.tar.gz` from the [latest release](../../releases/latest), then:

```bash
tar -xzf palmtopd-*-linux-x86_64.tar.gz
cd palmtopd-*
./install.sh --from-release
```

Or build it yourself:

```bash
git clone https://github.com/skapyskar/palmtop
cd palmtop
./scripts/install.sh
```

Either way this installs `palmtopd`, starts it as a **systemd `--user`
service** that survives reboots, and prints a QR code. Nothing needs `sudo`.

**Removing it:**

```bash
./uninstall.sh
```

</details>

<details>
<summary><b>Windows</b></summary>

Download the `.zip` from the [latest release](../../releases/latest), extract
it, then in PowerShell:

```powershell
.\install.ps1
```

This installs `palmtopd.exe` (and the bundled `ffmpeg.exe`) to
`%LOCALAPPDATA%\Palmtop`, registers a **logon Scheduled Task** — the closest
Windows equivalent to Linux's systemd service, since a real Windows Service
runs in a session that cannot capture your desktop or inject input into it —
and writes the pairing QR to `%TEMP%\palmtop-pair.svg`. Nothing needs
Administrator.

Windows Defender Firewall may prompt to allow `palmtopd.exe` the first time a
phone connects; allow it.

**Removing it:**

```powershell
.\uninstall.ps1
```

</details>

Both installers are always a **clean** install: any previous install is
removed first, so you can never end up running a stale binary under a config
written by an older version. That includes the pairing token, so paired
phones will need to pair again — pass `--keep-pairing` (`-KeepPairing` on
Windows) when upgrading if you would rather not re-pair. Uninstalling removes
the pairing token and the host's private key too; it asks first, and
`--keep-pairing`/`-KeepPairing` keeps your phones paired there as well.

### 3. Pair the two

Pick whichever suits you. The result is the same: the laptop is saved in the
phone's **Devices** list and you reconnect by tapping it.

#### Over a USB cable *(Linux, recommended for the first time)*

The pairing secret travels over the wire and never touches the network, so
nothing on your Wi-Fi can observe or interfere with it.

1. On the phone, enable USB debugging:
   **Settings → About phone → tap "Build number" seven times**, then
   **Settings → Developer options → USB debugging**
2. Plug the phone into the laptop and accept the prompt that appears
3. On the laptop:

```bash
./scripts/pair-usb.sh
```

Then unplug. The laptop is saved on the phone and reconnects wirelessly from
now on.

> USB debugging is a developer setting that lets a computer control your
> phone. Turn it back off once you are done pairing if you would rather not
> leave it on.

USB pairing is Linux-only for now — on Windows, use the QR code below.

#### By scanning a QR code *(no cable needed)*

**Linux:**

```bash
./scripts/show-pair-qr.sh
```

**Windows:** `install.ps1` already wrote it to `%TEMP%\palmtop-pair.svg` —
open that file in any image viewer or browser.

On the phone: **Devices → Add by scanning QR**, and point the camera at it.

#### By finding it on the network

On the phone: **Devices → Find on this network**, tap your laptop, and type
the pairing token that `palmtopd` printed.

Convenient, but the weakest of the three: the laptop's identity key is
announced over the network here rather than handed over privately, so on a
network you do not trust, prefer USB or QR.

---

## Using it

Tap where you want to click — it works like a touchscreen, not like a laptop
trackpad. Drag to drag. The **⌨** button opens the keyboard.

The left column keeps only what you use during a session:

| | |
|---|---|
| **⚙** | Settings — everything below |
| **⌨** | Keyboard |
| **Joystick** | Nudge the cursor precisely, for window edges and text carets |
| **L** / **R** | Left and right click, wherever the cursor currently is |
| **Ctrl Alt Shift ❖** | Modifier keys — tap to latch, tap again to release |
| **Esc** / **Tab** / **↵** | Keys the phone keyboard hides or swallows. Latch Alt + tap Tab = Alt+Tab |

Tapping the video still clicks exactly where you tapped — the joystick is an
addition, not a replacement. It is there for the things a fingertip is too
blunt for: grabbing a window edge, placing a text caret. Because the **L**
button sends a real press and release rather than a synthesised click,
**holding L while moving the joystick drags** — which is how you resize a
window or select text.

### Modifier keys

The modifier buttons **latch**: tap **❖** and it stays held (the button
lights up) until you tap it again. They latch rather than needing to be held
down because Android's keyboard covers the screen in landscape — the button
would not be there at the moment you type the second key.

They press the real key, so all of this works:

| | |
|---|---|
| **❖** then a letter | `Super+D`, `Super+E`, … — tap **❖** off afterwards |
| **❖** on, then off | Opens the GNOME overview / KDE launcher, same as tapping Super |
| **❖** on, then joystick + **L** | `Super+drag` — move a window from anywhere in it |
| **Ctrl** then a letter | `Ctrl+C`, `Ctrl+T`, … |

Latched modifiers are released automatically if the connection drops, so
your laptop can never be left with a stuck Super key.

Everything else lives behind **⚙**:

| | |
|---|---|
| **Status** | Whether you are connected, and to what |
| **⟳ Reconnect** | Retry after a network hiccup |
| **🖥 Devices** | Switch laptops, or pair another |
| **⚙ Mode** | Quality preset — see below |
| **▭ Aspect** | Best Fit / 16:9 / 4:3 / 1:1 |
| **🔊 Volume** | Mute / down / up on the laptop |
| **🕹 Sensitivity** | How fast the joystick moves the cursor |
| **📋 Session log** | What the laptop is doing, and what failed |
| **📊 Stats** | Live latency figures |

**Three fingers** on the video zooms and pans, for reading something small.
One-finger taps keep working normally.

### Quality modes

| Mode | Best for |
|---|---|
| **Sync** | Lowest delay. Use when you are actually working on the laptop. |
| **Balanced** | The default — full resolution, still responsive. |
| **Quality** | Sharpest picture, for reading rather than interacting. |
| **Battery** | Longest phone battery life and least data. |

Measured on a mid-range phone over a hotspot, Sync runs about **52 ms** end to
end versus about **90 ms** for Balanced. Your numbers will differ; **⚙ → 📊
Stats** shows real ones for your own setup.

The app tells the laptop its screen size, refresh rate and decoder limits when
it connects, and the laptop sizes the stream to match. There is nothing to
configure per phone.

### Choosing the encoder

The laptop finds a working hardware encoder by itself, so this needs no
attention until it does. But it picks the **first** one that works, and on a
machine where several work — a hybrid-GPU laptop with both an iGPU and a
discrete NVIDIA card, say — that is not necessarily the one that feels best.
They differ in latency, power draw, fan noise and picture quality, and which
trade-off you want depends on things no probe can see.

See what your machine can actually do:

```bash
palmtopd --list-encoders
```

Then either pick one directly, or use the menu:

```bash
palmtopd --set-encoder h264_nvenc
./choose-encoder.sh              # same thing, with a menu and a restart
```

Valid values are `auto` (the default), `h264_vaapi`, `h264_nvenc`,
`h264_qsv`, `h264_amf` and `libx264`. A pinned encoder that later stops
working — driver update, GPU swapped out — falls back to auto-detection with
a loud warning rather than refusing to stream.

If the stream feels sluggish and **Sync** mode didn't fix it, this is the
next thing worth trying.

---

## If something goes wrong

**The phone cannot find the laptop.**
Many Wi-Fi networks — especially guest, café and university networks — block
devices from talking to each other directly. Test it by joining the laptop to
your phone's hotspot instead; a phone acting as its own access point cannot
block a device from reaching it. This is the single most common cause.

**The screen-share prompt never appears / the phone connects but the screen
stays black.**
Run the built-in check on the laptop first — it tests the real portal and
really encodes through your GPU, and names the exact fix for anything broken.
It tries VA-API, NVENC and software encoding and reports which ones actually
work on this machine, not just whether the hardware for one is nominally
present — any single one working is enough:

```
palmtopd --doctor
```

Two things worth knowing. The share prompt is only requested **when a phone
actually connects**, so it appearing at pairing time is not expected. And the
app keeps a session log under **⚙ → 📋 Session log**: it shows what the laptop
reported at every stage, including which one failed, so you rarely have to read
the laptop's logs at all.

**`pair-usb.sh` says no phone found.**
Check USB debugging is on, the cable carries data (some charge-only cables do
not), and that you accepted the "Allow USB debugging?" prompt on the phone.
`./adb-tools/adb devices` (the release tarball bundles its own `adb`, so no
separate install is needed) should list it.

**The volume buttons do nothing.** *(Linux)*
They send the same `XF86AudioRaiseVolume` / `LowerVolume` / `Mute` key presses
your laptop's own volume keys do — but a key only does something if your
desktop *binds* it. GNOME and KDE bind them out of the box. A bare wlroots
compositor does not, so on **Hyprland** or **Sway** you need the binding in
your own config, for example:

```
# Hyprland
bindl = , XF86AudioRaiseVolume, exec, wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%+
bindl = , XF86AudioLowerVolume, exec, wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%-
bindl = , XF86AudioMute,        exec, wpctl set-mute   @DEFAULT_AUDIO_SINK@ toggle
```

Once those work from the laptop's own keyboard, they work from the phone. On
**Windows** this doesn't come up — the volume keys map to `VK_VOLUME_*` and
work with no configuration.

**It connected once and now will not.**
Your laptop's IP probably changed. Re-pair — the saved entry updates in place
rather than duplicating, because devices are tracked by identity rather than
address.

**The picture lags during video playback.**
Switch to **Sync** mode. If it is still bad, the network is the bottleneck —
check **⚙ → 📊 Stats**, and prefer the phone's hotspot over shared Wi-Fi.

**Logs:**

```bash
journalctl --user -u palmtopd -f        # Linux
```

```powershell
Get-Content "$env:LOCALAPPDATA\Palmtop\palmtopd.log" -Wait   # Windows
```

That file is everything the daemon printed on its most recent start —
including the pairing token/QR path and the reason for any startup failure
that would otherwise be invisible, since Task Scheduler doesn't show a
console window. You can also check Task Scheduler → Task Scheduler Library
→ **Palmtop** for its last run result, or run `palmtopd.exe` directly from a
terminal to see output live instead of through the log.

---

## Security

- **Encrypted end to end** (Noise protocol) — video and input, not just the
  handshake. Nobody on your network can watch your screen.
- **No cloud, no account.** Nothing leaves your network.
- **No root or Administrator required.** On Linux, screen capture goes
  through the desktop's own permission prompt and input through the
  compositor. On Windows, capture and input both work at ordinary user
  privilege. If Palmtop asked for elevated access, that would be a much
  larger thing to trust.
- **Pair over USB or QR** to hand over the laptop's identity privately. The
  network-discovery path announces it over the LAN instead, which is
  convenient but weaker — noted plainly rather than glossed over.
- **Forget a device** any time: long-press it in the Devices list.

---

## Building from source

```bash
cargo build --release -p palmtopd     # laptop daemon (builds for the host OS)
cd android && ./gradlew assembleDebug # phone app
```

Cross-compiling the Windows daemon from Linux (what CI does):

```bash
rustup target add x86_64-pc-windows-gnu
sudo apt install gcc-mingw-w64-x86-64   # or your distro's equivalent
cargo build --release -p palmtopd --target x86_64-pc-windows-gnu
```

Tests:

```bash
cargo test --workspace
cd android && ./gradlew testDebugUnitTest
```

---

## Documentation

- **[docs/WALKTHROUGH.md](docs/WALKTHROUGH.md)** — how this was built, what was
  measured, and the bugs found along the way. Written as an engineering record,
  including the things that did not work.
- **[docs/superpowers/specs/](docs/superpowers/specs/)** — design documents.

---

## Known limits

- **Linux: Wayland only.** No X11 support.
- **Windows: no elevated input.** A non-elevated `palmtopd` (which is how it
  always runs — see Security above) cannot send input to windows running as
  administrator, by Windows' own design (UIPI). Task Manager, admin-elevated
  apps, and UAC prompts are unreachable from the phone.
- **No macOS host.**
- **One phone at a time** per laptop.
- **No audio.** Video and input only.
- **No Unicode text input** on either platform yet — ASCII typing only (see
  `Message::Text` in the protocol, a documented gap on both hosts).
- **Latency figures exclude the phone's own display response**, which needs
  external hardware to measure. They are honest lower bounds, not
  glass-to-glass numbers.
- **Windows support is new.** The Linux host has been used daily for weeks;
  the Windows host has been built to the same standard but not yet verified
  on real hardware by anyone other than a first-time installer. If something
  doesn't work, `palmtopd.exe --doctor` is the first thing to run, and an
  issue report is very welcome.
