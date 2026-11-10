# Wiring the CC1101 to the Pi Zero W

> ## ⚠️ 3.3 V ONLY
>
> **The CC1101 module is a 3.3 V part. Never connect it to a 5 V pin.**
>
> - Power it from Pi **pin 1 or pin 17 (3V3)**.
> - Pi **pins 2 and 4 are 5 V**. They sit right next to pin 1. Putting the
>   module's VCC on one of them destroys the module, usually instantly and
>   without any visible sign.
> - Count the pins twice before you plug in the power. Wire with the Pi
>   unplugged.

## Pin table

Wire by **signal name**. The names are printed on the module or in its
seller's pinout picture. Pin *positions* differ between module makes, so do
not copy a pin-number layout from a different board.

| CC1101 signal | Also labelled | Pi physical pin | Pi GPIO (BCM) | Purpose |
|---|---|---|---|---|
| **VCC** | 3V3, VDD | **17** | 3V3 power | power, **3.3 V only** |
| **GND** | | **20** | ground | ground |
| **SCK** | SCLK, CLK | **23** | GPIO11 (SPI0 SCLK) | SPI clock |
| **MOSI** | SI, SDI | **19** | GPIO10 (SPI0 MOSI) | SPI data, Pi to module |
| **MISO** | SO, SDO, GDO1 | **21** | GPIO9 (SPI0 MISO) | SPI data, module to Pi |
| **CSN** | CS, SS | **24** | GPIO8 (SPI0 CE0) | SPI chip select |
| **GDO0** | GD0 | **22** | GPIO25 | transmit data (carrier on/off) |
| GDO2 | GD2 | not connected | | unused |

Seven wires. All seven Pi pins are in one block at pins 17-24:

```
        (towards the SD card: pin 1 end)
              ...   ...
   3V3   17 | o   o | 18
  MOSI   19 | o   o | 20   GND
  MISO   21 | o   o | 22   GDO0  (GPIO25)
   SCK   23 | o   o | 24   CSN   (CE0)
              ...   ...
```

Odd pin numbers are the row nearer the middle of the board; even numbers are
the row at the board edge. Pin 1 is at the SD-card end and has a square
solder pad.

On the most common 8-pin module (2 x 4 header, coiled antenna) the order is
usually 1 GND, 2 VCC, 3 GDO0, 4 CSN, 5 SCK, 6 MOSI, 7 MISO, 8 GDO2. I have
not seen your module, so treat that as a hint and trust its own labels.

## Notes

- **GDO0 can go on another free GPIO.** Change `gdo0_gpio` in the config to
  match. It is a BCM GPIO number (25), not a physical pin number (22).
- **Optional: a resistor in the GDO0 wire.** Anything from 330 Ω to 1 kΩ.
  Both the Pi and the CC1101 are able to drive that line; the software makes
  sure only one does at a time (DECISIONS.md, D6), and the resistor limits
  the current if a bug or a brown-out ever breaks that rule. Not required.
- **Connect the antenna before transmitting.** See HARDWARE.md for the right
  length at 304.25 MHz. The coiled antenna that ships with most modules is
  cut for 433 MHz.
- **Keep the wires short**, 10-15 cm at most. Long SPI wires cause read
  errors that look like a dead module.
- **Power:** the module draws about 30 mA while transmitting, well within
  what the Pi's 3.3 V pin supplies.

## Enabling SPI

SPI is off by default on Raspberry Pi OS. Either:

```sh
sudo raspi-config        # Interface Options -> SPI -> Yes
```

or, without the menu:

```sh
sudo raspi-config nonint do_spi 0
```

or by hand: add the line `dtparam=spi=on` to `/boot/firmware/config.txt`
(on older releases the file is `/boot/config.txt`).

Then reboot and check:

```sh
ls /dev/spidev0.*
# expected: /dev/spidev0.0  /dev/spidev0.1
```

`spidev0.0` is chip select CE0, the one in the table.

To run `pi-alarm send ...` as your normal user (not through the service),
that user must be in the `spi` and `gpio` groups. The default user on
Raspberry Pi OS already is; check with `groups`.

## First check after wiring

```sh
pi-alarm check --radio
```

This resets the chip over SPI and reads its version register. Expected in
the output:

```
CC1101 found: part 0x00, version 0x14
```

(Other chip revisions and clones are reported to answer with a different
version, such as `0x04` or `0x17`. Any value other than `0x00` and `0xFF`
means the chip is talking.)

If it says **no CC1101 found (VERSION register reads 0x00 or 0xFF)**, the Pi
is not hearing the chip at all. Check, in this order:

1. SPI is enabled: `ls /dev/spidev0.*` lists two devices.
2. The module has power: 3.3 V between its VCC and GND pins, if you have a
   multimeter.
3. Each of SCK, MOSI, MISO and CSN goes to the Pi pin in the table. MOSI and
   MISO swapped is the classic mistake.
4. The wires are short and firmly seated. An answer that changes from run to
   run means a loose contact.

Then continue with HARDWARE.md.
