# Building and deploying

Three parts: building and testing on your computer, cross-compiling for the
Pi, and the phone app.

What has actually been done so far, so you know what is proven:

| Step | Status |
|---|---|
| Native build, tests, clippy on x86_64 Linux | done, passing |
| Cross-compile to ARMv6 | done; the binary was inspected but **never run** |
| Deploying to the Pi | **not done** (no access to the Pi) |
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
- Bluetooth should already be running: `systemctl status bluetooth`.

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
