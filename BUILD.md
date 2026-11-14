# Building and deploying

Three parts: building and testing on your computer, cross-compiling for the
Pi, and the phone app.

What has actually been done so far, so you know what is proven:

| Step | Status |
|---|---|
| Native build, tests, clippy on x86_64 Linux | done, passing |
| Cross-compile to ARMv6 (`arm-unknown-linux-gnueabihf`) | done; the binary was inspected (ARMv6, hard-float, glibc ≤ 2.28) but **never run**, because this machine has no ARM glibc |
| Static ARMv6 build of the same code (`--static`) | done, and **run under emulation** on an ARM1176 (the Zero's CPU model): all tests pass and the daemon registered its BLE service with BlueZ |
| Anything on a real Pi | **not done** (no access to the Pi) |
| Building the phone app | **not done** (no Flutter SDK installed) |

## 1. On your computer (x86_64 Linux)

Needs: a Rust toolchain (`rustup`), and a C compiler (`gcc` or `clang`),
which is used once to build the bundled D-Bus library.

```sh
cargo build            # debug build
cargo test             # all tests
./scripts/check.sh     # format check + clippy + tests; run before committing
```

Try it without any hardware, using the mock radio and the captures in
`flipper_files/`:

```sh
alias pa='cargo run --quiet -- --config pi-alarm.dev.toml'

pa check                          # config and capture files
pa inspect flipper_files/3_fan.sub
pa send fan_3                     # "transmits" on the mock radio
pa add --time 06:30 --days weekdays --step light_toggle@-10m --step fan_3@0
pa list
pa next
pa request '{"v":1,"op":"status"}'
pa run                            # the daemon; Ctrl-C to stop
```

`pi-alarm.dev.toml` keeps its data in `./local-data/` (git-ignored) and has
BLE switched off. Set `enabled = true` under `[ble]` to also start the BLE
server on your computer's Bluetooth adapter.

About the D-Bus library: the `bluer` crate needs libdbus. By default this
project compiles it from source into the binary (cargo feature
`vendored-dbus`), so no `libdbus-1-dev` package is required. To link the
system library instead: `cargo build --no-default-features` (then you need
`libdbus-1-dev` and `pkg-config`).

## 2. Cross-compiling for the Pi Zero W

### Why the target matters

The Pi Zero W's processor is an ARM1176: **ARMv6** with hardware floating
point. The Rust target for that is:

```
arm-unknown-linux-gnueabihf        <- ARMv6 + hard float. Use this.
armv7-unknown-linux-gnueabihf      <- ARMv7. Do NOT use: builds fine, then
                                      dies on the Zero with "Illegal instruction".
```

There is a second trap. The usual cross C toolchain from distribution
packages (`gcc-arm-linux-gnueabihf` on Debian and Ubuntu) is itself built for
ARMv7, and the startup code it links into every program contains ARMv7
instructions. A binary linked
with it can crash on the Zero even though Rust compiled for ARMv6. This
project therefore links with **Zig**, which targets ARMv6 correctly and
needs no system packages.

### One-time setup

```sh
# The ARMv6 standard library for Rust (installs under ~/.rustup).
rustup target add arm-unknown-linux-gnueabihf

# Zig and cargo-zigbuild, inside the project directory.
python3 -m venv .tools/venv
.tools/venv/bin/pip install ziglang cargo-zigbuild
```

`.tools/` is git-ignored. No sudo and no system packages are involved.

### Build

```sh
./scripts/build-pi.sh
```

The binary is `target/arm-unknown-linux-gnueabihf/release/pi-alarm` (about
3 MB). The script prints the proof that it is right for the Zero:

```
ELF 32-bit LSB pie executable, ARM, EABI5 ..., interpreter /lib/ld-linux-armhf.so.3
  Tag_CPU_arch: v6
  Tag_FP_arch: VFPv2
```

`v6` is the important line. It depends only on glibc 2.28 or newer, so it
suits Raspberry Pi OS (32-bit) from Buster onwards. The Zero W cannot run
64-bit Raspberry Pi OS.

### The static variant

```sh
rustup target add arm-unknown-linux-musleabihf     # once
./scripts/build-pi.sh --static
```

This produces `target/arm-unknown-linux-musleabihf/release/pi-alarm`: the
same program, still ARMv6 hard-float, with the C library (musl) compiled in.
It depends on nothing installed on the Pi.

Use the normal build first, as planned. Reach for the static one if the
normal binary will not start on your Pi (for example an error mentioning
`GLIBC_...` or `ld-linux-armhf`). It is also the only variant that could be
executed here: with `qemu-arm-static -cpu arm1176`, which emulates the
Zero's processor and stops on any instruction that processor lacks.

To repeat that test (needs `qemu-arm-static`, which was already installed on
this machine):

```sh
qemu-arm-static -cpu arm1176 \
    target/arm-unknown-linux-musleabihf/release/pi-alarm inspect flipper_files/3_fan.sub
```

### If Zig does not work for you

- **`cross`** (`cargo install cross`, needs Docker or Podman):
  `cross build --release --target arm-unknown-linux-gnueabihf`. Its build
  image has a genuine ARMv6 toolchain.
- **Build on the Pi itself:** install Rust on the Pi and run
  `cargo build --release` there. It works, since the Pi's own target is the
  ARMv6 one, but expect well over an hour and add swap first.

## 3. Deploying to the Pi

Not done from here. These are the steps, to run yourself. `pi@raspberrypi`
stands for your user and host name.

### Prepare the Pi (once)

- Raspberry Pi OS Lite, 32-bit.
- Enable SPI (WIRING.md).
- Set the time zone, because alarm times are in the Pi's zone:
  `sudo timedatectl set-timezone America/New_York` (use yours).
- Switch Bluetooth on. Raspberry Pi OS keeps the radios off until a Wi-Fi
  country is set, and setting the country alone does not unblock Bluetooth:

  ```sh
  sudo raspi-config nonint do_wifi_country US     # use yours
  for r in /sys/class/rfkill/rfkill*; do
      [ "$(cat $r/type)" = bluetooth ] && echo 0 | sudo tee $r/soft
  done
  bluetoothctl show | grep Powered                # expect "Powered: yes"
  ```

  The setting survives reboots.
- Install the small system files from `deploy/` (each file says in its first
  lines what it is for):

  | File | Goes to (on the Pi) |
  |---|---|
  | `bluetooth-pi-alarm.conf` | `/etc/systemd/system/bluetooth.service.d/pi-alarm.conf` |
  | `pi-alarm-clock-save.service`, `.timer` | `/etc/systemd/system/` |
  | `pi-alarm-journald.conf` | `/etc/systemd/journald.conf.d/99-pi-alarm-persistent.conf` |
  | `pi-alarm-usb-gadget.conf` | `/etc/modprobe.d/` (only if you use the USB link below) |

  Then `sudo systemctl daemon-reload`,
  `sudo systemctl enable --now pi-alarm-clock-save.timer`,
  `sudo systemctl restart bluetooth systemd-journald`.

### Copy the files

From the project directory on your computer:

```sh
./scripts/build-pi.sh
scp target/arm-unknown-linux-gnueabihf/release/pi-alarm \
    deploy/pi-alarm.example.toml deploy/pi-alarm.service \
    flipper_files/*.sub \
    pi@raspberrypi:/tmp/
```

### Install (on the Pi)

```sh
sudo install -m 755 /tmp/pi-alarm /usr/local/bin/pi-alarm
sudo mkdir -p /etc/pi-alarm/signals
sudo cp /tmp/*.sub /etc/pi-alarm/signals/
# Only the first time, so you do not overwrite your edited config:
sudo cp -n /tmp/pi-alarm.example.toml /etc/pi-alarm/pi-alarm.toml
sudo cp /tmp/pi-alarm.service /etc/systemd/system/
sudo systemctl daemon-reload
```

Check before starting the service:

```sh
pi-alarm --version
pi-alarm check              # config and captures
pi-alarm check --radio      # also talks to the CC1101
pi-alarm send light_toggle  # the one-button test (HARDWARE.md, step 3)
```

If `pi-alarm --version` prints "Illegal instruction", the binary was built
for the wrong ARM version; rebuild with `./scripts/build-pi.sh`.

Then:

```sh
sudo systemctl enable --now pi-alarm
journalctl -u pi-alarm -f       # live log
```

A healthy start logs `CC1101 found`, `loaded N alarm(s)` and
`BLE ready on hci0 ... advertising as pi-alarm`.

### Everyday commands on the Pi

```sh
pi-alarm list                   # alarms (the service keeps running)
pi-alarm next                   # what will run in the next 7 days
sudo pi-alarm add --time 07:00 --days weekdays --step fan_3@0
sudo pi-alarm delete <id>
sudo systemctl restart pi-alarm # after editing /etc/pi-alarm/pi-alarm.toml
```

Reading needs no sudo. Changing alarms does, because `/var/lib/pi-alarm`
belongs to root. The running service picks up alarm changes within a second.

To test BLE before the radio is wired, set `transmitter = "mock"` in the
config; the service then logs what it would transmit.

### Updating

Rebuild, `scp` the binary, `sudo install` it again, and
`sudo systemctl restart pi-alarm`. Alarms and config are untouched.

### Working with the Pi over USB

The Pi Zero W can only join 2.4 GHz Wi-Fi with WPA2. Without such a network,
the way in is the USB cable: the Pi appears to the computer as a network
adapter. Use a **data** cable in the Pi's port labelled **USB** (the middle
one); that one cable also powers the Pi.

Everything goes through one script, and there is no order to remember. Plug
the Pi in and run a command; if the Pi is still booting, the script waits.

```sh
scripts/pi ssh            # a shell on the Pi
scripts/pi ssh pi-alarm list
scripts/pi status         # service, radio, Bluetooth, clock, next alarm
scripts/pi logs           # follow the service log
scripts/pi deploy         # build, install the new binary, restart the service
scripts/pi reboot
scripts/pi shutdown       # then unplug when the green light is dark
```

The script also sets the Pi's clock from the computer's each time it
connects, because the Pi has no battery clock.

**Unplugging.** Run `scripts/pi shutdown` first when you can. Pulling the
cable without it has not caused a problem so far (alarms are saved in a way
that survives a power cut), but a clean shutdown is the safe habit.

**The one rule: do not let the computer connect by itself.** If the computer
sends anything in the first moments after the Pi switches its USB network on
(about 80 seconds after power-on), the Pi stops accepting data from the
computer until it is unplugged and plugged back in. Seen 6 times out of 6
with an automatic connection, never with the wait the script does. The
symptom is a link that looks up but never gets an answer. So the computer's
connection is created with `autoconnect no`, and `scripts/pi` starts it at
the right moment. If you connect by hand (network menu) too early and it
hangs: unplug the Pi, plug it back in, run `scripts/pi connect`.

One-time setup:

1. On the Pi: switch on Raspberry Pi OS's USB gadget mode. On this Pi it
   was done at first boot by adding `rpi:` / `enable_usb_gadget: true` to
   the `user-data` file on the SD card's boot partition. Then install
   `deploy/pi-alarm-usb-gadget.conf` (table above) and reboot. The Pi is
   then always `10.12.194.1` on the cable.
2. On the computer (NetworkManager), create the connection the script uses.
   It matches the Pi on any USB port, never starts by itself, and never
   takes over the computer's internet:

   ```sh
   nmcli connection add type ethernet con-name pi-alarm-usb \
       802-3-ethernet.mac-address 02:70:69:61:6C:02 \
       connection.autoconnect no \
       ipv4.method manual ipv4.addresses 10.12.194.2/28 ipv4.never-default yes \
       ipv6.method disabled
   ```

## 4. The phone app

**The app has never been compiled.** It was written by hand against the
published source of `flutter_blue_plus` 2.3.15, without the Flutter SDK.
Expect to fix a few compile errors on the first build. The pure-Dart parts
(framing, models, sync queue) have unit tests that should tell you quickly
whether the logic is sound.

### Setup

1. Install the Flutter SDK: <https://docs.flutter.dev/get-started/install>
   (choose Android). `flutter doctor` should be happy about the Android
   toolchain.
2. Generate the Android project files around the existing code. Commit
   first, so you can see exactly what the command adds:

   ```sh
   cd app
   flutter create --platforms=android --project-name pi_alarm --org com.example .
   git status          # new android/ folder; lib/ and pubspec.yaml unchanged
   ```

   If it added `test/widget_test.dart` (a sample test for a counter app),
   delete that file: there is no counter here and it will not compile.
3. Add the Bluetooth permissions. In
   `android/app/src/main/AndroidManifest.xml`, directly inside `<manifest>`
   and above `<application>`:

   ```xml
   <uses-feature android:name="android.hardware.bluetooth_le" android:required="true" />

   <!-- Android 12 and newer (the S21 FE). -->
   <uses-permission android:name="android.permission.BLUETOOTH_SCAN"
                    android:usesPermissionFlags="neverForLocation" />
   <uses-permission android:name="android.permission.BLUETOOTH_CONNECT" />

   <!-- Android 11 and older. -->
   <uses-permission android:name="android.permission.BLUETOOTH" android:maxSdkVersion="30" />
   <uses-permission android:name="android.permission.BLUETOOTH_ADMIN" android:maxSdkVersion="30" />
   <uses-permission android:name="android.permission.ACCESS_FINE_LOCATION" android:maxSdkVersion="30" />
   ```

   `flutter_blue_plus` asks the user for these at run time by itself.
4. Fetch packages and run the tests:

   ```sh
   flutter pub get
   flutter analyze
   flutter test
   ```

5. Run on the phone (USB debugging on):

   ```sh
   flutter run
   ```

### First use

1. Make sure the Pi service is running and you are near it.
2. Open the app. The status in the top right says **Set up**. Tap it, then
   **Find Pi**. The app scans for the Pi's service, remembers its address
   and connects.
3. From then on, opening the app connects by itself, sends the time, and
   syncs. The status shows **Synced**, **Connecting**, or **N unsent**.

### Licence note

`flutter_blue_plus` 2.x is not under a standard open-source licence. It is
free for personal and non-profit use, which is why the code passes
`License.nonprofit` when connecting. Read
<https://pub.dev/packages/flutter_blue_plus/license> if this ever becomes
more than a personal project.
