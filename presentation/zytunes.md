---
title: zytunes
sub_title: 
author: Danondso
date: 2026-08-29
theme:
  name: tokyonight-storm
---

How Did I Get Here?
===

<!-- alignment: center -->

<!-- pause -->

I love tinkering and modding stuff. 

<!-- Pics of my iPod collection here -->

<!-- pause -->

Started buying way to much shit from FreeGeek down in Fayetteville.

<!-- speaker_note: I'm a third generation hoarder and love collecting stuff -->

<!-- pause -->

RIP my wallet.

<!-- speaker_note: Might work with other devices, I haven't tried lol -->

<!-- pause -->

Started going in around every Friday to see what's new.

Fast forwards a couple months and this is happening

<!-- todo: picture of ipod collection here -->



<!-- pause -->

I want to make my own

<!-- speaker_note: Hold up the actual devices here if you brought them — Zune, iPod Classic. Physical props sell the "I still use these" premise immediately. -->

<!-- end_slide -->

I. The problem
===

<!-- alignment: center -->

<!-- pause -->



<!-- pause -->

I like owning my music. I want to keep using the devices I grew up with.

<!-- pause -->

But they have — or are slowly becoming — **abandonware**.

<!-- end_slide -->

A. The Zune
===

<!-- alignment: center -->

<!-- pause -->

Got

<!-- pause -->

RIP in peace 2006-2012, went the way of the GoGear, Zen, 

<!-- pause -->



<!-- end_slide -->

B. The iPod
===

<!-- alignment: center -->

Apple still "supports" the ecosystem, technically.

<!-- pause -->

But actually managing a **local** library on a **classic** iPod — no
streaming, no cloud, just your own files — gets harder every macOS
release. iTunes is gone. Sync is an afterthought bolted onto Finder.

<!-- pause -->

Nominally alive. Practically neglected.

<!-- end_slide -->

C. Windows sucks
===

<!-- alignment: center -->

I use macOS and Linux. I don't want to boot Windows to manage a music
player.

<!-- pause -->

I tried **Rockbox**. USB sessions kept timing out mid-transfer, which
makes copying a whole album over reliably... not fun.

<!-- pause -->

So: no first-party tooling worth using, no good third-party alternative
for my OS. Fine. I'll write it myself.

<!-- end_slide -->

D. LLMs make this trivial
===

<!-- alignment: center -->

<!-- pause -->

Reverse-engineered protocol docs, decades-old file formats, obscure C
libraries — the kind of project that used to take a determined person a
*year* of nights and weekends.

<!-- pause -->

With an LLM doing the tedious parts alongside me? Suddenly this looks
like a weekend project that turns into a few months, instead of a few
months that turns into never.

<!-- end_slide -->

II. Goals
===

<!-- alignment: center -->

<!-- pause -->

**Terminal-based**, just because — I like living in a terminal.

<!-- pause -->

**Entirely Rust.** Even the C libraries some amazing people wrote —
because without them this project genuinely wouldn't exist — get ported
or wrapped, never vendored as-is.

<!-- end_slide -->

II.B The timeline
===

<!-- alignment: center -->

This felt like a two-year project.

<!-- pause -->

It was three months.

<!-- pause -->

That's the whole pitch for pairing with an LLM on something like this:
not that it writes better code than you, but that it collapses the
"I'll get to that eventually" projects into "I got to that this weekend."

<!-- pause -->

Vibe coding. Woohoo.

<!-- end_slide -->

III. zytunes
===

<!-- alignment: center -->

<!-- pause -->

Where it actually starts: the smallest possible thing that could sync a
song to a device.

<!-- end_slide -->

A. The MVP
===

<!-- alignment: center -->

Step one: get *some* library into the tool.

<!-- pause -->

The fastest path was Apple Music's **Library.xml** export — point at the
file, parse it, get artist/album/track metadata for free.

<!-- pause -->

It worked. It also turned out to be a mistake I'd pay for later. More on
that in a bit.

<!-- end_slide -->

B. Built on giants
===

<!-- alignment: center -->

Rust has quietly assembled a real audio ecosystem. zytunes leans on it
hard:

<!-- pause -->

* [`rodio`](https://crates.io/crates/rodio) — local playback
* [`ratatui`](https://crates.io/crates/ratatui) — the entire TUI
* [`rusb`](https://crates.io/crates/rusb) — USB device detection
* [`lofty`](https://crates.io/crates/lofty) — tag reading across every format I care about
* [`symphonia`](https://crates.io/crates/symphonia) + [`mp3lame-encoder`](https://crates.io/crates/mp3lame-encoder) — pure-Rust decode/transcode, no ffmpeg required for audio
* [`discid`](https://crates.io/crates/discid) — CD table-of-contents reads, LGPL, dynamically linked

<!-- end_slide -->

C. Themes!
===

<!-- alignment: center -->

Because a terminal app with one grey color scheme is a terminal app I
won't open twice.

<!-- pause -->

16 built-in themes — iTunes 2004, Gruvbox, Tokyo Night, Windows 95,
System 7, BIOS, Zune Original — plus user-defined themes via
`~/.config/zytunes/config.toml`. `t` to pick, live preview.

<!-- end_slide -->

D. Album art
===

<!-- alignment: center -->

Terminals aren't known for pictures. Do it anyway.

<!-- pause -->

Two renderers — unicode **halfblock** and a 10-character luminance-ramp
**ascii** fallback for terminals that can't do better — cached per
`(artist, album)` and invalidated on `(mtime, size)` so re-tagging a
file refreshes the art automatically.

<!-- end_slide -->

IV. The Zune
===

<!-- alignment: center -->

<!-- pause -->

This is where "weekend project" stopped being funny.

<!-- end_slide -->

A. Rewriting libmtp-zune in Rust
===

<!-- alignment: center -->

The Zune doesn't speak plain MTP. It speaks **MTPZ** — Microsoft's
encrypted variant — and the only real documentation of that handshake
is [`libmtp-zune`](https://github.com/kbhomes/libmtp-zune), a
reverse-engineering project in C.

<!-- pause -->

`zune-mtp` ports that protocol knowledge — RSA-1024 signing, a
PSS-like certificate exchange, AES-128-CBC, CMAC key extraction — into
a native IOKit transport, bypassing libusb entirely because libusb
can't do the data-out operations the handshake needs.

<!-- end_slide -->

B. The one where Claude makes tools
===

<!-- alignment: center -->

Reverse-engineering a firmware needs experiments, not production code.

<!-- pause -->

So the actual workflow became: describe a hypothesis to Claude, have it
scaffold a throwaway probe binary, run it once against the real device,
read the result, delete or keep the probe.

<!-- pause -->

`tools/mtp-probe` is what survived that process — a diagnostic CLI with
subcommands for exactly the kind of "does the firmware actually support
this" questions that used to mean bricking a device to find out.

<!-- end_slide -->

C. Syncing, finally
===

<!-- alignment: center -->

Handshake works. Session opens. `SendObjectInfo` → `SendObject` → a song
is *on the device*.

<!-- pause -->

Music library sync, push, remove, device browsing — the boring, load-bearing
CRUD that makes this an actual tool instead of a proof of concept.

<!-- end_slide -->

The firmware rabbit hole
===

<!-- alignment: center -->

<!-- pause -->

Music was syncing. Then I looked at the album browser.

<!-- end_slide -->

1. Wait, why are there blocks?
===

<!-- alignment: center -->

Every non-ASCII character in a track or artist name rendered as a solid
block glyph on the device's screen.

<!-- pause -->

Not a crash. Not an error. Just... boxes, where an "é" or an em dash
should be.

<!-- end_slide -->

2. Sorry, kid
===

<!-- alignment: center -->

Asked Claude what was going on. The honest answer, after digging through
what the firmware actually ships:

<!-- pause -->

*"Sorry, kid — there's no font support here."*

<!-- pause -->

The bitmap font table baked into this firmware build simply doesn't have
glyphs past the ASCII range. Nothing to patch at the protocol level. The
problem is baked into the binary on the device.

<!-- end_slide -->

3. Hacks, then probes
===

<!-- alignment: center -->

First instinct: work around it in software. Transliterate, strip
accents, degrade gracefully.

<!-- pause -->

Second instinct, once "gracefully" started meaning "worse": open the
the device and write probes.

<!-- pause -->

Not to patch this build — to understand how firmware versioning and the
older train worked at all, because the build I actually wanted
lived on the *other side* of an update I'd taken.

<!-- end_slide -->

4. – 5. An older firmware train
===

<!-- alignment: center -->

Later Zune firmware trains differ in what they ship, so it wasn't just
"switch trains overnight."

<!-- pause -->

Working through the protocol with small probes — what it advertises, what it
trusts — turned "not possible" into "possible, carefully."

<!-- pause -->

Boom. Cash money. I did the rest of this work on firmware **1.4**.

<!-- end_slide -->

6. – 7. Font hack time
===

<!-- alignment: center -->

1.4 doesn't fix the font table on its own — but it's old enough, and
different enough, that the glyph set it ships actually covers more of
what I needed.

<!-- pause -->

Patched, flashed, synced a track with an accented title.

<!-- pause -->

Oh wow. **Fonts!**

<!-- end_slide -->

8. The tradeoffs
===

<!-- alignment: center -->

Nothing on a reverse-engineered abandonware device is free.

<!-- pause -->

v1.4 firmware has its own scars, which is exactly why `findings.md` in
this repo exists:

* `SetObjectPropValue` on certain album-art JPEGs just... hangs, forever,
  until the 45s timeout fires and desyncs the USB pipes
* Pre-3.0 firmware rejects the sync-progress vendor op outright — has to
  fail silently instead of logging a scary warning every sync
* The album browser reads cover art from a separate prop store, not the
  embedded ID3 tag most tools rely on

<!-- pause -->

Every one of these is a firmware-version tradeoff I made on purpose, in
exchange for actually being able to read a track name.

<!-- end_slide -->

G. But I still like the 3.0 theme
===

<!-- alignment: center -->

All that, and I'll admit it: Zune 3.0's on-device UI just looks nicer.

<!-- pause -->

So zytunes doesn't force a firmware choice — it supports both, and adds
**photo and video sync** (`photo-sync`, `video-sync`) for the newer
firmware's Pictures/Video stores, transcoding video to WMV2/WMAv2 via
ffmpeg along the way.

<!-- end_slide -->

V. The iPod
===

<!-- alignment: center -->

<!-- pause -->

After the Zune, this felt almost relaxing.

<!-- pause -->

No encrypted handshake — a classic iPod just mounts as a USB mass-storage
volume. The hard part isn't talking to it, it's getting **iTunesDB**
bit-exact: mhit/mhbd records, hash58 signing, ArtworkDB thumbnails,
ported from `libgpod` from scratch in the `ipod-db` crate.

<!-- pause -->

"Apple still supports it" turns out to mean about the same thing as
"Microsoft still supports the Zune." In practice, both are on me now.

<!-- end_slide -->

VI. Shifting foundations
===

<!-- alignment: center -->

<!-- pause -->

Somewhere around month two, the "weekend project" architecture started
showing its age.

<!-- end_slide -->

A. Removing Library.xml
===

<!-- alignment: center -->

Remember that convenient MVP shortcut from section III?

<!-- pause -->

Apple's XML export doesn't carry the metadata a real device-sync tool
actually needs, and it's one more moving part tied to one specific app
on one specific OS.

<!-- pause -->

`feat!: remove iTunes Library.xml support` — directory scanning became
the *only* library backend. Slower to set up. Correct forever after.

<!-- end_slide -->

B. I can get the stain out
===

<!-- alignment: center -->

The unglamorous, load-bearing refactor work:

<!-- end_slide -->

1. God file!
===

<!-- alignment: center -->

`app.rs` and `native.rs` had both grown into the kind of file where
"just add one more match arm" stops being a joke.

<!-- pause -->

Split into `tui/app/keys.rs`, `tui/app/events.rs`, decomposed `native.rs`
into focused pieces — same behavior, files a human can actually hold in
their head again.

<!-- end_slide -->

2. Security?
===

<!-- alignment: center -->

Not a pentest — the boring kind of security: `unwrap()` calls that
would panic the whole TUI on a malformed USB response, `String` errors
that made every failure mode a guessing game.

<!-- pause -->

Typed `MtpError`/`DeviceError` enums, `unwrap()` swept out of the hot
paths, CI wired up with dependency + security scanning so this doesn't
regress quietly.

<!-- end_slide -->

3. Using TDD correctly :P
===

<!-- alignment: center -->

The `DeviceSession` trait exists specifically so sync/remove/collect
logic can be tested against a fake — no hardware required, no flaky
USB-in-CI nonsense.

<!-- pause -->

Behavior asserted, not mocks — did the right bytes land in the right
file, not "was `write` called." Failing test first, then the code that
makes it pass.

<!-- end_slide -->

C. There's a snake in my brut
===

<!-- alignment: center -->

Using Python. Because I have to.

<!-- pause -->

Section II said "entirely Rust." Section VII is about to spend its whole
budget on stem separation, and the state of the art for that lives in
PyTorch. Purity lost to pragmatism, and I'm at peace with it.

<!-- end_slide -->

VII. The Candy Store
===

<!-- alignment: center -->

<!-- pause -->

So — I've made a music app I actually like using. Works on both
platforms. Tracks the metadata I care about.

<!-- pause -->

We're done, right?

<!-- pause -->

...right?

<!-- end_slide -->

A. Stems
===

<!-- alignment: center -->

Press `M` on a playing track, get it split into live-toggleable
vocals/drums/bass/etc. Because why *wouldn't* a music sync tool also do
real-time source separation.

<!-- end_slide -->

1. uv
===

<!-- alignment: center -->

Stem separation needs real Python dependencies — PyTorch, model
checkpoints, gigabytes of it.

<!-- pause -->

Nothing installs at startup or during a library scan. First press of
`M`, zytunes asks, then bootstraps [`uv`](https://github.com/astral-sh/uv)
and pulls a pinned engine version into a managed location — only after
you say yes.

<!-- end_slide -->

2. A multi-pass architecture
===

<!-- alignment: center -->

One pass gets you six stems. Getting **backing vocals** separated from
the **lead** vocal needs a second, more specialized pass on top of that.

<!-- pause -->

The `hq-harmony` recipe cascades a Mel-Roformer karaoke model over the
already-isolated vocal stem — seven stems out, lead and backing split
cleanly, each pass's intermediate output feeding the next.

<!-- end_slide -->

3. Nvidia drivers, thanks Linux
===

<!-- alignment: center -->

Roformer inference on CPU is *markedly* slower than demucs. You really
want a GPU for the high-quality recipes.

<!-- pause -->

So naturally, the next few evenings went to Linux's favorite pastime:
fighting Nvidia drivers until CUDA agreed to exist.

<!-- pause -->

It works now. `[stems] gpu = true` in the config, and I no longer think
about it — which is the only metric that matters for a hobby project.

<!-- end_slide -->

B. What's still in the store
===

<!-- alignment: center -->

The candy store doesn't have a bottom. Next up:

<!-- pause -->

* **A streaming client** — point zytunes at the library over the network
  instead of only local sync
* **A mobile app** — on-device stem breakdown, so the split doesn't need
  a laptop in the loop at all

<!-- pause -->

Neither exists yet. Both are why "done" was never really the word.

<!-- end_slide -->

VIII. What have we learned?
===

<!-- alignment: center -->

<!-- pause -->

AI allows me to borrow the power of Demons and summon back from the dead these old devices. 

<!-- pause -->

Rust can absolutely live next to decades of reverse-engineered C
knowledge — you just have to be willing to port the *understanding*,
not the code.

<!-- pause -->

An LLM didn't replace the USB traces or the nights of "why does this
it replaced the eight other things that would've made me give up before
getting to them.

<!-- pause -->

And the refactor in section VI only worked because there were tests to
catch what broke. Three months of vibe coding still needs a seatbelt.

<!-- end_slide -->

zytunes
===

<!-- alignment: center -->

`github.com/Danondso/zytunes`

<!-- pause -->

Thanks, all!
