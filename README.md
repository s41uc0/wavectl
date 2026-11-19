# wavectl

A priority-aware audio manager for PipeWire. It sits next to your sound server and decides what
happens when several things play at once, e.g. a laptop used as an audio hub for a phone and a console.

- **Priorities**: order your sources (programs, phones, consoles). When a higher one plays, lower
  ones **duck**, **pause** (MPRIS apps and phones over Bluetooth/AVRCP), **mute**, or are left alone,
  per source.
- **Per-program tiers**: any program can be given its own tier, so ducking is per program, not just per group.
- **Mixer**: every program/device stream gets a volume slider drawn over a stereo meter with falling peaks.
- **Routing**: send a stream to any output, or send a program's audio into another program that is recording.
- **Now playing**: track and player info plus transport controls for MPRIS apps and Bluetooth phones.

Bluetooth *pairing* is deliberately out of scope (use your desktop's settings or `bluetoothctl`);
connected devices simply show up as sources.

## Install (Nix)

```sh
nix profile add github:s41uc0/wavectl
systemctl --user enable --now wavectld wavectl-tray
```

`wavectld` is the daemon, `wavectl` the CLI, `wavectl-gui` the window, `wavectl-tray` the tray icon.
The tray is a StatusNotifierItem (on GNOME you need the AppIndicator extension).

## CLI

```sh
wavectl status                      # tiers, levels, behaviors
wavectl streams                     # every stream: volume, output, now playing
wavectl behavior "Phone" pause      # duck | pause | mute | nothing
wavectl promote Spotify             # give a program its own tier
wavectl priority up|down <source>
wavectl route <stream> <output>     # or `default`
wavectl send <stream> <program>     # send a stream into a recording program
wavectl player <stream> playpause
```

Config lives in `~/.config/wavectl/config.toml`.

## How it works

The daemon meters each stream with a passive capture stream and ducks via the node's `softVolumes`
property, which is independent of the user's own volume, so your volume is never touched. Routing uses
the PipeWire default metadata (like pavucontrol) and explicit links.

## Develop

```sh
nix develop            # or: nix-shell
cargo test
```

Layout: `crates/core` (pure priority engine), `daemon`, `client` (D-Bus), `cli`, `gui` (GTK4), `tray`.
