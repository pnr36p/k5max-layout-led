# K5 Max Layout Indicator

[English](README.md) · [Русский](README.ru.md)

A tiny Windows tray app that lights the Keychron K5 Max keys differently for each keyboard layout (US, RU, ...), so you can see the active layout on the keyboard itself. One 450 KB exe, no runtime, about 2 MB of RAM and no CPU use at idle.

Colors are described in a plain `config.toml`: which keys get which color, per layout. The keyboard itself is described there too, so other RGB keyboards with the same firmware protocol can be supported without touching the code, see [Other keyboards](#other-keyboards).

> Not affiliated with Keychron, SignalRGB or QMK. Use at your own risk, flashing included.

## How it works

The app talks to the keyboard over raw HID (usage page `0xFF60`, usage `0x61`) with the SignalRGB QMK protocol. It does not need the SignalRGB application, but it needs a firmware that implements that protocol. The stock Keychron firmware does not.

Every 100 ms it asks Windows for the keyboard layout of the foreground window and, when it changes, streams a new color frame to the keyboard. It also repaints every 30 s and after the system resumes from sleep or hibernation, because the firmware drops its LED buffer on suspend. A connection counts as established only after the keyboard answers a query. Any I/O error closes the handle and retries with backoff, so a keyboard that vanished during hibernation cannot leave the app in a half-connected state.

## Requirements

- Windows 10 or 11
- Keychron K5 Max, ANSI, connected **with the USB cable** (USB ID `3434:0A50`, 108 LEDs). This is what the default `config.toml` describes.
- Firmware with the SignalRGB protocol, see below

Anything else needs a different `[device]` section, see [Other keyboards](#other-keyboards). Wireless modes are not supported.

## Step 1. Flash the firmware

The firmware is built by the SignalRGB moderators and published in
[SRGBmods/QMK-Binaries](https://github.com/SRGBmods/QMK-Binaries/tree/main/QMK%2BVIA-Firmware/0.14.29-keychron).
This repository does not redistribute it.

The file I run is `keychron_k5_max_ansi_rgb_via.bin` (QMK 0.14.29, SignalRGB protocol 1.0.6, 100 140 bytes):

```
SHA-256  991C60E8A75225ED4B99F8081AFC079819DC6CC83A580948879B2E7F59A535F2
```

A file of the same name and size is in the `wireless_playground` folder there. That match is by name and size only, so compare the hash of whatever you download. The `2025q3` folder has `keychron_k5_max_ansi_rgb_keychron.bin` instead, a different build that I have not tried. Their ReadMe suggests `bluetooth_playground` if wireless misbehaves after flashing.

Flashing, following their ReadMe:

1. Install [QMK Toolbox](https://github.com/qmk/qmk_toolbox/releases).
2. Run it as Administrator and install the drivers from the Tools menu (Ctrl+N).
3. Put the keyboard into bootloader mode as described in Keychron's firmware instructions for your model.
4. Select the `.bin` file in QMK Toolbox and flash it.

This replaces the factory firmware. To go back, flash the official firmware for your model from Keychron's site. Close SignalRGB and VIA while you use this app, they use the same HID interface.

## Step 2. Install the app

Build it yourself (a recent stable Rust with the MSVC toolchain, developed with 1.99):

```powershell
cargo build --release
```

The exe is `target\release\k5max-indicator.exe`. Put it in any folder you can write to and run it. On first start it creates `config.toml` next to itself, and it writes `k5max-indicator.log` there too.

Two helpers print to the console. The exe has no console of its own, so add `| more` to make the shell wait for the output:

```powershell
.\k5max-indicator.exe --list | more
.\k5max-indicator.exe --probe | more
```

`--list` shows the raw HID interfaces that are connected, with the `vid`/`pid` to put into `config.toml`. Any QMK keyboard with VIA shows up, so this only tells you the USB ID.

`--probe` opens the keyboard named in `config.toml` and asks it for the protocol details. That is the real check that the firmware speaks the protocol. For the K5 Max firmware above:

```
device             3434:0A50
QMK version        [00, 0E, 1D]
protocol version   [01, 00, 06]
total LEDs         108 (matches the 108 keys in config.toml)
firmware type      [02, 00, 06]
```

`00 0E 1D` is QMK 0.14.29.

## Tray menu

The icon is a green K when the keyboard is connected and grey when it is not.

| Item | Action |
|---|---|
| status line | connection state and current layout |
| Start with Windows | per-user autostart through the `Run` registry key |
| Reload config | re-reads `config.toml`, keeps the old colors if it has an error |
| Exit | stops the app, returns the keyboard to its own effect when `restore_on_exit = true` |

Only one instance runs at a time.

## Configuration

[config.toml](config.toml) is the whole configuration and is commented. In short:

- `[device]` is the keyboard: USB ID and the name of every key in LED order. Restart the app after changing it, Reload config refuses to apply it.
- `[groups]` gives names to sets of keys.
- `[common]` applies to every layout; `[layouts.<NAME>]` is applied on top of it. A line can be a group or a single key, with `[r, g, b]` or `"#RRGGBB"`. Later lines win. Keys not mentioned stay dark.
- `heartbeat_seconds` is how often the app checks the link and repaints. Changing the keyboard effect with Fn is overridden after at most this long.
- `log = false` turns the log file off. Reload config applies it right away and leaves a last `log disabled` line. The log is on by default, and it also stays on when `config.toml` cannot be parsed, because that is when you need it. It rotates to `k5max-indicator.log.old` at 1 MiB, so it stays small either way.

## Languages

Any language works, not only US and RU. `lang` in a `[layouts.<NAME>]` section is the Windows language ID of the keyboard layout (the low 16 bits of its handle), and every section gets its own colors. Adding German, for example:

```toml
[layouts.DE]
lang = 0x0407
letters = [255, 200, 0]
numbers = [64, 255, 64]
gaming = [255, 128, 128]
```

then choose **Reload config** in the tray menu.

| ID | Language | ID | Language |
|---|---|---|---|
| `0x0409` | English (US) | `0x0422` | Ukrainian |
| `0x0809` | English (UK) | `0x0423` | Belarusian |
| `0x0407` | German | `0x0415` | Polish |
| `0x040C` | French | `0x0405` | Czech |
| `0x0410` | Italian | `0x041F` | Turkish |
| `0x0419` | Russian | `0x0411` | Japanese |

The ID is per language, not per layout: US-International, Dvorak and Colemak are all `0x0409` and look the same. To find the ID of anything else, switch to it and read the `input language XXXX` line in `k5max-indicator.log`. A language without a section uses `unknown_layout`.

## Other keyboards

The code knows nothing about the K5 Max any more: the USB ID and the key list are in `[device]`. It should work with any keyboard that has per-key RGB, a wired USB connection and firmware with the SignalRGB QMK protocol. I have only tested the K5 Max ANSI.

1. **Flash a SignalRGB build** for your model, see [Step 1](#step-1-flash-the-firmware). White-backlit models have no per-key RGB and are not suitable.
2. **Find the USB ID** with `--list`, and put it into `vid` and `pid`.
3. **Run `--probe`**. It prints the LED count of your keyboard. Note it down.
4. **List the keys in LED order.** Open `rgb.c` of your model in [Keychron's qmk_firmware](https://github.com/Keychron/qmk_firmware) (branch `wireless_playground`, `keyboards/keychron/<model>/<variant>/rgb/rgb.c`) and find `g_led_config`:
   - The first table maps the key matrix to LED indexes, `__` means no LED. On Keychron boards the numbers grow left to right, top to bottom, so the order is simply the keys as you read them on the keyboard.
   - The second table gives each LED's physical position `{x, y}`, which is handy to check the order.
   - Write one name per index into `keys`. The names are yours (any text without spaces, case does not matter, each used once). Write `_` for an LED that is not a key, such as underglow.
5. **Fix `[groups]`, `[common]` and `[layouts.*]`** so they only use names that exist in your list. Reloading or starting names the offending key.
6. **Start the app** and look at the log. `device 3434:xxxx, N LEDs` then `keyboard connected` means it works. If the count differs from what the keyboard reports, the log says `keyboard reports N LEDs, config.toml lists M`.

The K5 Max list in [config.toml](config.toml) is a complete example. Keychron's `rgb.c` for the K5 Max ANSI is `keyboards/keychron/k5_max/ansi/rgb/rgb.c`.

### Models that may fit

USB IDs from the `info.json` files in Keychron's source, vendor ID `0x3434`. I have not checked them on hardware (only `0x0A50`), so confirm with `--list`. LED counts differ per model, get yours from `--probe`.

| Model | ANSI | ISO | JIS |
|---|---|---|---|
| K1 Max | `0x0A10` | `0x0A11` | `0x0A12` |
| K2 Max | `0x0A20` | `0x0A21` | `0x0A22` |
| K3 Max | `0x0A30` | `0x0A31` | `0x0A32` |
| **K5 Max** (default) | `0x0A50` | `0x0A51` | – |
| K7 Max | `0x0A70` | `0x0A71` | `0x0A72` |
| K8 Max | `0x0A80` | `0x0A81` | `0x0A82` |
| K10 Max | `0x0AA0` | `0x0AA1` | – |
| K11 Max (encoder) | `0x0AB3` | `0x0AB4` | `0x0AB5` |
| K13 Max | `0x0AD0` | `0x0AD1` | – |
| K15 Max (encoder) | `0x0AF0` | `0x0AF1` | – |
| K17 Max (encoder) | `0x0A00` | `0x0A01` | `0x0A02` |

SignalRGB builds for most of these are in the `2025q3` folder of SRGBmods/QMK-Binaries. Other Keychron series (Q Max and others) and other makers' QMK boards with SignalRGB firmware work the same way, only the numbers differ. If you get one working, a pull request with your `[device]` section is welcome.

The protocol limits a keyboard to 255 LEDs and 32 byte raw HID packets, which is what QMK uses by default.

## Development

```powershell
cargo test
```

The tests cover the connection logic against a fake keyboard (stale handle after hibernation, silent device, resume, repeated heartbeats, wrong LED count), the config parsing, and compare the generated frames with reference dumps in `testdata/`.

### Source layout

The code that does not depend on the operating system is shared: `config.rs` (parsing), `device.rs` (protocol and reconnecting) and `controller.rs` (the layout on display, config reload, the engine thread). Everything OS specific is in `src/platform/<os>/`, and only `windows` exists so far.

On other systems the crate builds with a stub (`platform/unsupported.rs`), so `cargo test` runs the portable tests there, but the app itself reports that the system is not supported. A new platform provides the functions listed at the top of `src/platform/mod.rs`: raw HID access, a tray or menu bar shell that tells the controller which layout is active, and autostart. In `config.toml` a layout's `lang` is the Windows language ID, and `input_source` is the macOS input source ID (for example `com.apple.keylayout.US`). Each platform reads its own field and ignores the other.

## License

[MIT](LICENSE). The firmware is a separate project with its own terms.
