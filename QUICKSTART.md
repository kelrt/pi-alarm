# Quick start

The short version. Each step links to the full instructions.

## 0. Try it on your computer (no hardware)

```sh
cargo test
cargo run -- --config pi-alarm.dev.toml check
cargo run -- --config pi-alarm.dev.toml send fan_3     # mock radio
```

## 1. Confirm the captures work

On the Flipper: Sub-GHz → Saved → `3_fan` → Send, from where the Pi will
sit. The fan should respond. ([HARDWARE.md](HARDWARE.md), step 1)

## 2. Wire the CC1101

**3.3 V only, never 5 V.** Seven wires, then enable SPI
(`sudo raspi-config nonint do_spi 0`, reboot). Replace the module's antenna
with a 23.5 cm straight wire. ([WIRING.md](WIRING.md))

## 3. Build and copy to the Pi

One-time setup on your computer:

```sh
rustup target add arm-unknown-linux-gnueabihf
python3 -m venv .tools/venv
.tools/venv/bin/pip install ziglang cargo-zigbuild
```

Then:

```sh
./scripts/build-pi.sh
scp target/arm-unknown-linux-gnueabihf/release/pi-alarm \
    deploy/pi-alarm.example.toml deploy/pi-alarm.service \
    flipper_files/*.sub  pi@raspberrypi:/tmp/
```

If the binary will not start on the Pi, use `./scripts/build-pi.sh --static`
instead. ([BUILD.md](BUILD.md), sections 2-3)

## 4. Install and test on the Pi

```sh
sudo install -m 755 /tmp/pi-alarm /usr/local/bin/pi-alarm
sudo mkdir -p /etc/pi-alarm/signals
sudo cp /tmp/*.sub /etc/pi-alarm/signals/
sudo cp -n /tmp/pi-alarm.example.toml /etc/pi-alarm/pi-alarm.toml
sudo cp /tmp/pi-alarm.service /etc/systemd/system/
sudo timedatectl set-timezone America/New_York     # your zone

pi-alarm check --radio              # expect "CC1101 found"
sudo pi-alarm send light_toggle     # the light should toggle
```

If the light does not toggle: [HARDWARE.md](HARDWARE.md), "If nothing
happens".

## 5. Start the service

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now pi-alarm
journalctl -u pi-alarm -f
```

Set an alarm without the phone:

```sh
sudo pi-alarm add --time 06:30 --days weekdays \
    --step light_toggle@-10m --step fan_3@0
pi-alarm list
pi-alarm next
```

## 6. Phone app

The app has never been compiled; expect to fix some errors.

```sh
cd app
flutter create --platforms=android --project-name pi_alarm --org com.example .
# add the Bluetooth permissions from BUILD.md section 4 to AndroidManifest.xml
flutter pub get && flutter test && flutter run
```

In the app: tap the status in the top right → **Find Pi**. After that it
connects, sets the Pi's clock and syncs by itself. ([BUILD.md](BUILD.md),
section 4)

## Later

- Add fan speeds 1 and 2: capture with Read RAW, add an `[actions.fan_1]`
  block to the config. ([LEARNING.md](LEARNING.md), exercise 1)
- Status, open questions and what is untested: [HANDOFF.md](HANDOFF.md)
