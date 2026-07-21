# Local configuration

Everything in here describes **your machine and your devices**, not the project. None of it
is committed — anyone cloning this repo has different hardware, a different network, and
possibly several phones. Code must never hardcode these values; it reads them from here.

```
config/
  host.example.toml      committed — template for the machine running palmtopd
  host.toml              YOURS, gitignored
  devices/
    example.toml         committed — template for a phone
    <name>.toml          YOURS, gitignored — one file per device
  active                 YOURS, gitignored — name of the device to use by default
```

## Adding a device

```sh
cp config/devices/example.toml config/devices/my-phone.toml
$EDITOR config/devices/my-phone.toml
echo my-phone > config/active          # make it the default
```

Multiple devices coexist as separate files; nothing needs editing in the code to switch
between them. Select one per-invocation with `PALMTOP_DEVICE`:

```sh
PALMTOP_DEVICE=other-phone cargo run -p spike-h264-server
PALMTOP_DEVICE=other-phone ./scripts/run-decode-spike.sh
```

Resolution order for which device is used: `PALMTOP_DEVICE` env var → `config/active` →
error listing what's available.

## Populating a device profile

Most fields can be read straight off a connected phone:

```sh
./scripts/probe-device.sh <adb-serial>    # prints a ready-to-paste profile
```

## Why host settings are separate

`host.toml` covers things that vary by *machine* rather than by phone — the VA-API render
node (which differs on hybrid-GPU laptops), the stream port, and the host's IP on the
current network. One host, many devices.
