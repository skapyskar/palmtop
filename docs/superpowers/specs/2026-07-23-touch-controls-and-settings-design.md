# Touch controls: settings consolidation, edge padding, and a cursor joystick

Date: 2026-07-23
Status: designed, not yet implemented

## Problem

v0.3.0 removed the persistent title bar and restyled the app, and the layout is
sound. Three usability problems remain, all reported from real use:

1. **The video's top corners are hard to hit.** With the title bar gone the
   video now starts flush against the top edge of the phone. Taps intended for
   the laptop's top-corner UI (window close buttons, menu bars) land badly or
   get swallowed by the system's own edge gestures.
2. **The left column carries every control at once.** Status, Reconnect,
   Devices, Mode, Aspect, Log and HUD are all permanently on screen, but almost
   all of them are set-once-and-forget. They occupy the column that the
   controls used *during* a session should own.
3. **There is no fine cursor control.** Absolute tap-to-click is the right
   default and stays the default, but it cannot express "nudge the cursor two
   pixels" — which is what resizing a window edge or placing a text caret needs.

## Non-goals

- **Changing the video's width or aspect ratio.** Explicitly ruled out by the
  user: the picture must stay exactly the size and shape it is today. The
  control column keeps its current fixed 170dp width for precisely this reason,
  and `VideoFit` is not touched.
- **Replacing absolute tap-to-click.** The joystick is purely additive. Tapping
  the video keeps working exactly as it does now, unchanged. This preserves the
  property the whole input design rests on: correctness never depends on
  watching a lagging cursor arrive (see `MainActivity`'s class doc).
- **A client-drawn cursor.** The host's real cursor is composited into the
  captured frames (`CursorMode::Embedded`), so joystick motion is already
  visible in the stream. Drawing a second, local cursor would show two.
- **Trackpad mode / gesture remapping.** Out of scope; the joystick is one
  bounded addition, not a general input-mode rework.
- **Protocol or host changes.** None are needed — see below.

## What already exists (verified, not assumed)

The entire input vocabulary this design needs is already implemented and wired
end to end. Verified by reading the source on both sides:

| Need | Wire message | Host handler | Client sender |
|---|---|---|---|
| Joystick motion | `PointerMotionRelative { dx, dy }` (tag 5) | `input.rs:86` → `pointer.motion(dx, dy)` | `Protocol.pointerMotionRelative()` |
| L / R click | `PointerButton { button, pressed }` (tag 7) | `input.rs:96` → `BTN_LEFT` / `BTN_RIGHT` | `Protocol.pointerButton()` |
| Existing tap | `PointerMotionAbsolute { x, y }` (tag 6) | `input.rs:90`, normalized ×`EXTENT` | unchanged |

**Consequences, and they are large:** no `PROTOCOL_VERSION` bump, no Rust
rebuild, no host release, no version-skew risk. This ships as an APK-only
change. It also means the joystick cannot desync the two ends — it emits
messages the host has understood since before this feature existed.

`PointerMotionRelative` is delivered to the compositor verbatim as `f64`
logical pixels. The joystick's motion math is therefore denominated in **host
desktop pixels**, not phone pixels — a distinction that matters because the two
have different densities and the video is scaled between them.

## Design

### 1. Top edge inset

A fixed **12dp top padding** on `mainLayout` (the horizontal `LinearLayout`
holding the column and the video).

Applied to the parent rather than to `videoContainer` deliberately. Parent
padding reduces the child's allocated size, so `videoContainer.getHeight()`
already reflects the inset and `resizeSurfaceToFit` re-fits correctly with no
change to its logic. Padding `videoContainer` itself would *not* work: it
measures `getWidth()`/`getHeight()` including its own padding and centers
`videoClip` within the full bounds, so the inset would be ignored.

Insetting the parent also insets the control column, which fixes the same
edge-reachability problem for the ⚙ gear that will sit at its top.

Fixed 12dp rather than the system window inset: predictable, small, and
independent of notch/cutout variation between devices. The video loses 12dp of
height and keeps its aspect ratio exactly (`VideoFit` letterboxes as always).
This is the one intentional size change in this spec, and it is the entire
point of item 1.

### 2. Settings sheet

A **⚙ gear** at the top of the left column opens a full-screen Settings
overlay, following the existing `Ui.sheet` overlay pattern already used by
`showSessionLog()` and `showDeviceListOverlay()` — same construction, same
dismissal, same visual language. Nothing overlays the video.

Contents, all relocated out of the column:

- **Connection status** — the `statusView` mono line, with its live colour
  (accent connecting / green connected / red failed)
- **⟳ Reconnect**
- **🖥 Devices**
- **⚙ Mode** (quality preset)
- **▭ Aspect**
- **📋 Session log**
- **📊 Stats (HUD)**

The `HudView` itself stays anchored in the left column, not in the sheet — it
is a live overlay meant to be read *while* using the session, so it must remain
visible after the sheet is dismissed. Only its toggle moves into Settings.

`setControlsVisible()` is updated to cover the new control set, so the
discovery/device overlays still hide every interactive control as they do now.

### 3. Left column — always-visible controls

The column keeps its current **fixed 170dp width** (unchanged, so the video's
width is byte-for-byte identical to today) and its existing `ScrollView`
wrapper. It holds, top to bottom:

```
┌─────────┐
│   ⚙     │  settings gear
│   ⌨     │  keyboard
│ ╭─────╮ │
│ │  ●  │ │  joystick pad
│ ╰─────╯ │
│ ┌──┬──┐ │
│ │ L│ R│ │  click buttons
│ └──┴──┘ │
│  [HUD]  │  (only when toggled on)
└─────────┘
```

Vertical budget: 170dp column minus 12dp padding each side leaves ~146dp
usable width. Gear 44 + keyboard 44 + pad 128 + L/R row 42, plus margins, is
roughly 300dp of height — fits a typical ~360dp landscape phone, and the
existing `ScrollView` absorbs any overflow on shorter screens exactly as it
was built to.

### 4. Joystick and click buttons

Split into two units with a clean seam, because the interesting logic is worth
testing without a device attached.

**`JoystickView`** — a self-contained custom `View`. Draws a circular pad and a
nub; on touch, clamps the nub to the pad radius and reports a **normalized
vector** (x, y each in −1..1, magnitude clamped to 1) to a listener. On release
the nub returns to center and it reports the zero vector. It knows nothing
about the network, the protocol, or the session. The clamp/normalize math is
extracted as a static pure function so it is unit-testable.

**`CursorDriver`** — converts that vector into a stream of relative-motion
messages. A `Handler`-driven tick at **60 Hz** (16 ms) while the vector is
non-zero; stopped entirely when it is zero, so an idle joystick costs nothing.
Per tick it computes a host-pixel delta:

- **Deadzone 0.12** — below this raw magnitude the vector is treated as zero.
  Without it, a thumb resting on the pad drifts the cursor continuously.
- **Rescale after the deadzone**, so speed is continuous rather than jumping at
  the deadzone edge: `m = (raw − 0.12) / (1 − 0.12)`, clamped to 0..1. This
  means full deflection still reaches exactly full speed.
- **Response curve `m²`** applied to that rescaled `m` — fine control near
  center (where precision work happens) with full speed still reachable at the
  rim. This is the whole reason a joystick beats a d-pad here.
- **Max speed 1200 host px/s** at full deflection → ~20px per 16ms tick.
- Delta is scaled by *actual elapsed* time since the previous tick, not the
  nominal 16ms, so cursor speed stays constant if the UI thread is briefly
  busy.

The per-tick delta computation is a **pure function** of (vector, elapsed ms) —
the unit under test.

**L / R buttons** send `PointerButton(button, true)` then
`PointerButton(button, false)` on tap: a complete press-release pair. They act
at the cursor's current host position, so they compose with the joystick
without either needing to know about the other.

**Drag** falls out for free and is worth naming: press-and-hold **L** while
working the joystick produces a real drag on the host, because press and
release are independent messages rather than a synthesized click. This is
something absolute tap-to-click cannot express at all.

### Data flow

```
JoystickView  --normalized vector-->  CursorDriver  --60Hz ticks-->
    Protocol.pointerMotionRelative(dx, dy)  -->  outbox  -->  writer thread
        -->  Noise encrypt  -->  socket  -->  host input.rs  -->  compositor

L/R button tap  -->  Protocol.pointerButton(btn, true/false)  -->  outbox  -->  ...

videoClip touch (UNCHANGED)  -->  Protocol.pointerMotionAbsolute(x, y)  -->  ...
```

Both new paths funnel into the existing `enqueue()` / `outbox` machinery. They
inherit its behaviour unchanged, including the writer thread and the Noise
session, and they add no new threads beyond the driver's `Handler` tick, which
runs on the existing UI looper.

### Error handling

- **Not connected.** `enqueue()` is already safe to call with no live session;
  messages are dropped. The joystick stays interactive (the nub still moves) so
  it does not read as frozen — consistent with how the other controls behave
  while disconnected.
- **Session torn down mid-drag.** If L is held when the connection drops, the
  host's compositor receives no release. `teardown()` will send a release for
  any button the client believes is held, so a dropped connection cannot leave
  the laptop with a stuck mouse button. This mirrors the plan's §9
  "modifier stuck" edge case, applied to pointer buttons.
- **View detached while ticking.** `CursorDriver` stops its `Handler` callbacks
  in the activity's teardown path, so no tick outlives the view it drives.

## Testing

JVM unit tests (`./gradlew testDebugUnitTest`), following the existing
`NetworkCheckTest` pattern:

**`CursorDriverTest`**
- zero vector produces zero delta (no drift at rest)
- raw magnitude inside the deadzone (< 0.12) produces zero delta
- full deflection for 1000ms produces ~1200 host px (max speed honoured)
- speed is continuous across the deadzone edge: raw magnitude just above 0.12
  produces a near-zero, not a jumped, delta
- the `m²` curve holds: raw deflection 0.5 rescales to m ≈ 0.432 and yields
  ≈ 0.187 of max speed — markedly less than half, which is the point
- delta scales with elapsed time — a 32ms tick moves twice a 16ms tick
- diagonal full deflection does not exceed max speed (magnitude clamped, so the
  classic "diagonals are faster" bug cannot occur)

**`JoystickViewTest`** (pure geometry, no Android view instantiation)
- touch at center → zero vector
- touch beyond the radius → magnitude clamped to exactly 1, direction preserved
- touch at the rim on each axis → the expected unit vectors
- vertical sign convention matches screen coordinates (down is +y), so the
  cursor does not travel the wrong way

**Manual, on device** (cannot be verified in this environment — no emulator or
device is attached, and this must be stated plainly rather than implied):
- the video is visibly the same width and shape as v0.3.0
- top corners are comfortably tappable
- joystick moves the host cursor smoothly; L/R click; hold-L + joystick drags
- tapping the video still clicks exactly where tapped

## Risks

| Risk | Mitigation |
|---|---|
| Column height overflows on a short landscape phone | Already inside a `ScrollView` built for exactly this; verified in the vertical budget above |
| Tuning constants (deadzone, curve, max speed) feel wrong in the hand | They are named constants in one place, trivially adjustable after real use; the numbers are a starting point, not a measurement |
| Joystick relative motion diverges from where the user thinks the cursor is | The host cursor is embedded in the video, so the stream itself is the feedback — no client-side position tracking to drift |
| Adding to an already-large `MainActivity` (1824 lines) | The two genuinely separable units with real logic (`JoystickView`, `CursorDriver`) become their own files; the settings sheet stays inline, matching the existing overlay pattern and adding little net code since the buttons already exist |

## Open questions

None blocking. Tuning constants are expected to need one round of adjustment
after real-device use — that is a follow-up, not a gate on shipping.
