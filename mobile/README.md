# zytunes_mobile

Flutter LAN client for `zytunes-serve`. Browse artists/albums, search, and
stream tracks over HTTP with a required Bearer token.

Streaming does not bump play count (`Range` seeks would inflate it). After
50% of the track or 4 minutes, whichever first, the app `POST`s
`/tracks/{id}/play` so LAN listens land in the same `local-plays.json`
sidecar as the TUI. Completing a track is a fallback for short clips;
skipping before the threshold does not count. The now-playing bar and
full player show the count; each increment flips the number and sweeps a
speedometer needle.

```bash
# on the computer that has the library
zytunes-serve --bind 0.0.0.0 --port 9847 --token SECRET

# on this machine
cd mobile
flutter test
flutter run
```

Connect with the server's LAN IP (or `.local` name), port `9847`, and the
same token. The app uses cleartext HTTP on the local network by design.
Host and port are remembered after the first attempt; the token is stored
in platform secure storage after a successful connect. A later launch
reconnects automatically. The gear on the library AppBar edits the same
fields without wiping a working session if the new server is unreachable.

`flutter install` uninstalls first and clears that storage. Prefer
`flutter run` / `flutter run --release` so Android upgrades in place.

On Android, playback holds a foreground notification so the OS does not
kill the process when the app is backgrounded. iOS uses the `audio`
background mode.

Themes: Bedfellow Light (default) and Bedfellow Dark, plus every TUI preset
(iTunes 2004, Gruvbox, Everforest, Tokyo Night, IBM Mainframe, Amber CRT,
Windows 95, System 7, BIOS, Red Sands, Newport Lights, NeXTSTEP, WinAmp
Classic, Zune Original). Pick one on the connect screen or in the library
gear menu; the choice is saved in SharedPreferences.

The artist list speed-scrolls like an iPod click wheel. After about 20
names in one flick, further dragging ticks by first letter (A→B→C) with
a haptic click per letter, a large overlay, and the list jumping only on
those ticks. A hard flick keeps ticking after you lift, slowing down
until it stops. Lifting slowly returns to ordinary scrolling.
