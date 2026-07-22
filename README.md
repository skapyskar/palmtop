# Palmtop

Use your Linux laptop from your Android phone. The laptop's screen streams to
the phone; you tap and type on the phone and it lands on the laptop.

Everything runs directly between your two devices. There is no account, no
cloud service, and no server anyone else operates — the video never leaves
your network, and the connection is encrypted end to end.

**Status:** working and used daily, but early. Linux + Wayland only for now.

---

## What you need

| | |
|---|---|
| **Laptop** | Linux with a **Wayland** desktop (GNOME, KDE, Hyprland, Sway, …) |
| **Phone** | Android 11 or newer |
| **Both** | On the same Wi-Fi — *or* the laptop joined to the phone's hotspot |
| **Laptop packages** | `ffmpeg` (almost certainly already installed) |

X11 is not supported yet. To check which you have, run `echo $XDG_SESSION_TYPE`
on the laptop — it should print `wayland`.

---

## Setup

### 1. Install the app on your phone

Download the `.apk` from the [latest release](../../releases/latest) and open it.

Android will warn you about installing from an unknown source. That is
expected — the app is not distributed through the Play Store, so your phone
has no way to vouch for it. You are trusting this repository instead.

### 2. Install the host on your laptop

Download the `.tar.gz` from the same release, then:

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

Either way this installs `palmtopd`, starts it as a background service that
survives reboots, and prints a QR code. Nothing needs `sudo`.

Installing is always a **clean** install: any previous install is removed
first, so you can never end up running a stale binary under a config written
by an older version. That includes the pairing token, so paired phones will
need to pair again — use `./install.sh --keep-pairing` when upgrading if you
would rather not re-pair.

### Removing it

```bash
./uninstall.sh
```

Removes the service, the binary, the pairing token and the host's private
key. It asks first, and `--keep-pairing` keeps your phones paired.

### 3. Pair the two

Pick whichever suits you. The result is the same: the laptop is saved in the
phone's **Devices** list and you reconnect by tapping it.

#### Over a USB cable *(recommended for the first time)*

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

#### By scanning a QR code *(no cable needed)*

```bash
./scripts/show-pair-qr.sh
```

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

The left column has everything else:

| | |
|---|---|
| **⟳ Reconnect** | Retry after a network hiccup |
| **🖥 Devices** | Switch laptops, or pair another |
| **⚙ Mode** | Quality preset — see below |
| **▭ Aspect** | Best Fit / 16:9 / 4:3 / 1:1 |
| **📊** | Live latency stats |
| **📋** | Session log — what the laptop is doing, and what failed |

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
end versus about **90 ms** for Balanced. Your numbers will differ; the **📊**
button shows real ones for your own setup.

The app tells the laptop its screen size, refresh rate and decoder limits when
it connects, and the laptop sizes the stream to match. There is nothing to
configure per phone.

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
really encodes through your GPU, and names the exact fix for anything broken:

```
palmtopd --doctor
```

Two things worth knowing. The share prompt is only requested **when a phone
actually connects**, so it appearing at pairing time is not expected. And the
app has a **📋 log button**: it shows what the laptop reported at every stage,
including which one failed, so you rarely have to read the laptop's logs at
all.

**`pair-usb.sh` says no phone found.**
Check USB debugging is on, the cable carries data (some charge-only cables do
not), and that you accepted the "Allow USB debugging?" prompt on the phone.
`./adb-tools/adb devices` (the release tarball bundles its own `adb`, so no
separate install is needed) should list it.

**It connected once and now will not.**
Your laptop's IP probably changed. Re-pair — the saved entry updates in place
rather than duplicating, because devices are tracked by identity rather than
address.

**The picture lags during video playback.**
Switch to **Sync** mode. If it is still bad, the network is the bottleneck —
check the **📊** stats, and prefer the phone's hotspot over shared Wi-Fi.

**Logs:**

```bash
journalctl --user -u palmtopd -f
```

---

## Security

- **Encrypted end to end** (Noise protocol) — video and input, not just the
  handshake. Nobody on your network can watch your screen.
- **No cloud, no account.** Nothing leaves your network.
- **No root required.** Screen capture goes through the desktop's own
  permission prompt; input goes through the compositor. If Palmtop asked for
  root, that would be a much larger thing to trust.
- **Pair over USB or QR** to hand over the laptop's identity privately. The
  network-discovery path announces it over the LAN instead, which is
  convenient but weaker — noted plainly rather than glossed over.
- **Forget a device** any time: long-press it in the Devices list.

---

## Building from source

```bash
cargo build --release -p palmtopd     # laptop daemon
cd android && ./gradlew assembleDebug # phone app
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

- **Wayland only.** No X11 support.
- **Linux host only.** No macOS or Windows host.
- **One phone at a time** per laptop.
- **No audio.** Video and input only.
- **Latency figures exclude the phone's own display response**, which needs
  external hardware to measure. They are honest lower bounds, not
  glass-to-glass numbers.
