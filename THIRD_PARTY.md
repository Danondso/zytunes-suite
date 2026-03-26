# Third-Party Notices

## android-file-transfer-linux (aft/)

The `aft/` directory contains a vendored fork of
[android-file-transfer-linux](https://github.com/whoozle/android-file-transfer-linux)
by Vladimir Menshakov, licensed under **LGPL-2.1**. See `aft/LICENSE` for the
full license text.

The fork adds library caching and debug USB tracing for Zune 30 compatibility.
It is invoked as a subprocess (`aft-mtp-cli`) and is not linked into the Rust
binaries.

## MTPZ keys

MTPZ authentication keys (`~/.mtpz-data`) originate from the
[libmtp-zune](https://github.com/kbhomes/libmtp-zune) project.
