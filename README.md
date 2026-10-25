# cosmic-bandwidth

[![CI](https://github.com/andyraitra/cosmic-bandwidth/actions/workflows/ci.yml/badge.svg)](https://github.com/andyraitra/cosmic-bandwidth/actions/workflows/ci.yml)

A [COSMIC](https://system76.com/cosmic) panel applet in the spirit of Bandwidth+ on macOS: live
network rate and this month's data usage against an optional cap.

Panel: `↓1.2 Mo/s ↑40.0 Ko/s · 12.4/50 Go`. Click it for received / sent totals, a gauge, the
next reset date, the cap setting and a manual reset. Notifications fire at 80 % and 100 % of the
cap. The interface is in French.

## Install (no root needed)

Build dependencies (once):

    sudo apt install pkg-config libxkbcommon-dev libwayland-dev libudev-dev libinput-dev libfontconfig-dev just

A Rust toolchain is required ([rustup](https://rustup.rs)). Then:

    just install

The binary goes to `~/.local/bin` and the `.desktop` entry to `~/.local/share/applications`. Add
the applet from Settings → Desktop → Panel → Applets → Bandwidth. Remove it with
`just uninstall`.

## How it works

- Every 2 s, reads `/sys/class/net/<interface>/statistics/{rx,tx}_bytes` and adds the deltas, so
  kernel counter resets (reboot, interface down/up) never inflate the total.
- The month's usage is saved in `~/.local/state/cosmic-bandwidth/usage.json`: atomic writes, at
  most every 30 s, immediately on rollover / alert / cap change / reset, and on exit. A file that
  cannot be read or parsed is moved aside to `usage.json.corrupt-<time>` and never overwritten;
  if it cannot be moved aside, saving is disabled instead of risking the data.
- COSMIC runs one applet process per panel / dock and per screen. Exactly one of them counts,
  alerts and saves (lock `usage.json.lock`); the others show the same usage. A reset done from
  any of them applies everywhere.
- Usage resets on the 1st of the month at 00:00 local time. If the clock goes back more than a
  month (broken hardware clock), the usage is kept and moved to the current month.
- Decimal units (1 Go = 10⁹ bytes), as macOS and carriers use.

Known limitations:

- traffic while no applet runs (logged out, powered off) is not counted;
- when another applet process takes over the counting (panel restart), up to one 2 s tick of
  traffic may be missed — never counted twice;
- after a state file that could not be read, saving stays off until the applet restarts
  (counting and alerts go on);
- a reset requested from a secondary instance is lost if the counting instance exits within the
  few milliseconds before it receives it.

## Settings

`~/.config/cosmic/io.github.andyraitra.CosmicBandwidth/v1/`:

- `interface`: counted interface, `"wlp2s0f0"` by default;
- `cap_bytes`: cap in bytes, `0` = none (also set from the applet, in Go);
- `reset_requests`: internal counter of reset requests.

Example, to count Ethernet:

    echo '"enp59s0"' > ~/.config/cosmic/io.github.andyraitra.CosmicBandwidth/v1/interface

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: [SECURITY.md](SECURITY.md).

## License

[GPL-3.0-only](LICENSE), like libcosmic.
