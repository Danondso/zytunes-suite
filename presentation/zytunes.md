---
title: Rusty Spaghetti
sub_title: Or - zytunes - building the music player I never thought I've always wanted while selling my soul to Anthropic while some bangin' tunes play oh and Rust is there too 🦀 (and maybe a snake 🐍)
author: by Dublin Anondson
date: 2026-08-29
theme:
  name: terminal-dark
  override:
    intro_slide:
      title:
        font_size: 3
    slide_title:
      font_size: 3
---


New Slide Who Dis?
===
<!-- font_size: 2 -->
<!-- alignment: center -->
Dublin
<!-- pause -->
<!-- speaker_note: SE on a PLG team at CompanyCam -->
- Senior Engineer
<!-- speaker_note: used to be a react dev but Claude writes everything now -->
<!-- pause -->
- BA in Computer Science @uark, December 2015
<!-- column_layout: [3, 2] -->
<!-- column: 0 -->
<!-- pause -->
![image:width:100%](assets/howl-and-aunty.png)
Howl and Aunty Whispers
<!-- column: 1 -->
<!-- pause -->
![image:width:100%](assets/chickens.png)
Big Eye, Jelly Boy, Hennsylvania, Bekahh, etc..
<!-- pause -->
<!-- end_slide -->
How Did I Get Here?
===
<!-- font_size: 2 -->
<!-- alignment: center -->
<!-- speaker_note: why am i giving a talk about ferrous pasta and old stuff -->
<!-- pause -->
<!-- speaker_note: Growing up in NJ I had access to lots of things to mess around with, stripping down old bikes and not putting them back together -->
Tinkering and modding stuff
<!-- pause -->
<!-- column_layout: [3, 2] -->
<!-- column: 0 -->
<!-- speaker_note: cobbled together a PSP that I could softmod from broken ones I bought off of friends, had to take them apart to see what version number the motherboards had and built one back together -->
![image:width:100%](assets/psp.png)
<!-- column: 1 -->
<!-- pause -->
![image:width:100%](assets/strat.png)

<!-- reset_layout -->
<!-- pause -->
It brings me joy to see stuff work longer than intended.
<!-- pause -->
It's a supreme act of ownership to repair and maintain.
<!-- end_slide -->

The Gateway
===
<!-- font_size: 2 -->
<!-- pause -->
<!-- speaker_note: Lovely little place down in Fayetteville called FreeGeek -->
![](assets/fgar.png)
<!-- alignment: center -->
<!-- pause -->
- 521 W Ash St, Fayetteville, AR 72703 (down the road from Fossil Cove)
<!-- pause -->
<!-- speaker_note: they take almost any electronic item, even car batteries, someone dropped off a telephone switchboard one time -->
- e-waste recycling
<!-- pause -->
<!-- speaker_note: reimage machines and put linux on them so they can be re-sold -->
<!-- speaker_note: similar to geeksquad -->
- IT support
<!-- pause -->
<!-- speaker_note: testing and pricing electronic items for sale in the thrift store -->
<!-- speaker_note: depending on skillset you can specialize in certain tech -->
<!-- speaker_note: volunteer hours count towards class credits -->
- volunteering
<!-- pause -->
- enabling my crippling addiction to hoarding outdated technology via their thrift store

![image:width:50%](assets/old-comps.jpg)
<!-- end_slide -->
Reject modernity; return to ~~monke~~ froot
===
<!-- font_size: 2 -->
<!-- alignment: center -->
<!-- pause -->
<!-- speaker_note: this is the first one I picked up, a 5.5th Gen iPod (if it has search then it's 5.5th gen!) -->
<!-- speaker_note: I was so excited, I hadn't had one since 2011, I had a large music collection leftover from highschool that I've been carrying around on HDDs I can explore again -->
<!-- speaker_note: No more using spotify, I was free -->
<!-- column_layout: [3, 2] -->
<!-- column: 0 -->
2024
![image:width:100%](assets/IMG_3924.jpeg)
<!-- column: 1 -->
<!-- pause -->
Walking out the store like:
![image:width:100%](assets/thrifting.gif)
![image:width:100%](assets/happy.gif)
<!-- reset_layout -->
<!-- pause -->
Wait..
<!-- pause -->
is this good enough?
<!-- end_slide -->
Time To Upgrade
===
<!-- font_size: 2 -->
<!-- column_layout: [3, 2] -->
<!-- alignment: center -->
<!-- pause -->

<!-- column: 0 -->
- 30GB stock, small boi
<!-- pause -->
<!-- speaker_note: using ALAC for a lot of my music, blows their estimation of 4min @128Kbps mp3 back in the elder days -->
- 320GB library, many lossless formatted
<!-- pause -->
![image:width:100%](assets/iflash.png)
<!-- pause -->
ZIF compatible storage powered by SD Cards
<!-- speaker_note: show the iPod and demonstrate how you'd open it -->
<!-- column: 1 -->
<!-- speaker_note: the hardware is designed to interface with the iPods, so they have a ZIF connector, just pop out the old ZIF drive and put the new one in -->
<!-- pause -->
- iFlash Solo
<!-- pause -->
- 512GB SD Card
<!-- pause -->
![image:width:100%](assets/evil-ipod-dance.gif)
- whole library fits!
<!-- pause -->
- we're done!
<!-- reset_layout -->
<!-- pause -->
![image:width:100%](assets/so-much-space.gif)
I am enjoying this.
<!-- end_slide -->

The End
===
<!-- font_size: 2 -->
<!-- pause -->
<!-- alignment: center -->
<!-- speaker_note: and... another -->
<!-- end_slide -->

The Spiral Continues
===
<!-- font_size: 2 -->
![image:width:60%](assets/a-few-months-later.png)
<!-- alignment: center -->
<!-- pause -->
![image:width:30%](assets/ohno.png)

<!-- speaker_note: Fast forward a few months, and things have dengenerated, I have this giant pile of devices, I've upgraded / repaired a lot of them, some are just hunks of junk because of activation lock. -->
<!-- end_slide -->

The Ingredients
===
<!-- font_size: 2 -->
<!-- speaker_note: this is where the germ of rusty spaghetti starts to take shape -->
<!-- alignment: center -->
<!-- pause -->
Accumulated a lot of devices
<!-- pause -->
<!-- speaker_note: nothing much to cover in the way of upgrading Zunes other than they can't read greater than 120GB if I remember right, and you can only use a few select SSD type ZIF Drives, iPod Classics and Zunes share similar batteries though! -->
iPods, Zunes, GoGears
<!-- pause -->
Upgrades, repairs, lack of parts
<!-- speaker_note: as far as I can tell, you cannot purchase the screen to repair a Philips GoGear 30GB -->
<!-- pause -->
Managing these in one app is
![image:width:60%](assets/obnoxious.png)

<!-- end_slide -->
Constraints
===
<!-- font_size: 2 -->

<!-- speaker_note: I have a fun set of constraints, also I may have missed an app in the ecosystem that would have made this easier but I was already thinking about the recipe, in my dreams --> 
<!-- alignment: left -->
<!-- pause -->
Windows has the best compatibility
<!-- pause -->
  - I don't use it  
<!-- pause -->
macos is iPod support
<!-- pause -->
  - Apple Music has no Zune support
<!-- pause -->
  - funky behavior on newer OS versions
<!-- pause -->
Rockbox is pretty good
<!-- pause -->
  - USB session timeouts when moving tracks
<!-- pause -->
  - also no Zune support
<!-- pause -->
Alternatives:
<!-- pause -->
  - gtkpod, but no Zune support
<!-- pause -->
  - buy an older mac, bootcamp windows
<!-- pause -->
  - VM
<!-- pause -->
  - Build a cross OS compatible terminal based music player entirely in rust while selling my soul to anthropic while some bangin tunes play
<!-- pause -->

And no snakes. 
<!-- pause -->
![image:width:20%](assets/evil-snake.gif)
<!-- speaker_note: you can probably figure out where the rest of this is going now -->
<!-- end_slide -->


Preamble: 
===
<!-- font_size: 2 -->
<!-- alignment: center -->
<!-- pause -->
- It's vibes all the way down
<!-- speaker_note: latish 2025 I think is when I started using LLMs wholesale at work and around March when I started working on this, I had a  -->
<!-- speaker_note: I cobbled the info together from memory/clauding about in the repo so I might get something wrong about a tech used or its function -->
<!-- pause -->
<!-- speaker_note: I'm going to talk about how great using Claude for this is, but I'm a little fatigued at the pace and I sorta feel my identity as an engineer getting degraded sorta -->
- Complicated feelings about LLMs now than back then
<!-- pause -->
<!-- speaker_note: I've never worked on a music app before, I leaned on claude to give me choices on how to proceed, at some point I stopped reading a lot of the prose it was writing -->
- I made lots of goofy decisions
<!-- pause -->
- If done again, I'd pay more attention
<!-- speaker_note: This talk is more about the bizarre speed at which I could iterate on something while getting inspired to try more as I went, because of Claude -->
<!-- end_slide -->

MVP
===
<!-- font_size: 2 -->
<!-- speaker_note: An overview of the MVP plus what we'll dig into -->
<!-- pause -->
<!-- alignment: center -->
<!-- speaker_note: this comes from Apple Music, I think it's a leftover option from the iTunes era, I don't actually know too much about what it's used for outside of this -->
- macos only to start
- Export and parse Library.xml
<!-- pause -->
- display album/artist/tracks, handle playback, search
<!-- pause -->
- CLI for ease of iterating with Claude
<!-- pause -->
<!-- speaker_note: I wanted to throw claude at this problem and see if it could put together something, at the time I was picking up and upgrading zunes -->
- manage media on Zunes
<!-- pause -->
- Themes!

<!-- pause -->
Kit:
<!-- column_layout: [5, 5] -->
<!-- column: 0 -->
<!-- pause -->
Patrician Crates 📦
* `ratatui` - TUI
<!-- speaker_note: basically you don't have to press enter, keypresses turn into events -->
* `crossterm` - interactive TUI
* `rodio` - playback 
<!-- speaker_note: Why this split? I tried to do the whole USB stack in Rust with rusb. Finding the Zune worked. The MTPZ handshake did not — Mac libusb fails every time you send data TO the device (the certificate), error 0x2002. android-file-transfer already talked IOKit on Darwin and the handshake worked. So the tradeoff became rusb for detection, aft-mtp-cli So I shipped rusb to detect, shell out to aft-mtp-cli for everything real. IOKit FFI in-process is the later zune-mtp chapter. Linux wasn't in the MVP. -->
* `rusb` — USB ops
* `quick-xml` - XML parsing
* `percent-encoding` - 
<!-- column: 1 -->
Plebian Libs 📚:
<!-- speaker_note: aft-mtp-cli is C++ and not rust, but I didn't think it made sense to try to build everything from scratch I wanted it to work first before trying to port libraries over -->
<!-- speaker_note: there's a whole sidequest hiding in here around modifying zune firmware versions I'm not going to get into for time -->
<!-- speaker_note: aft-mtp-cli has its own handshake TrustedApp::Create(session, ~/.mtpz-data), so it works out of the box -->
- `aft-mtp-cli` - C++ library rust shells out to -> MTP & MTPZ handshake
- `ffmpeg` - transcode to MP3 + 200×200 art
<!-- speaker_note: I guess rusb is cheating a little bit but that's okay it's an MVP -->
- `libusb` - what rusb uses under the hood
<!-- reset_layout -->
<!-- pause -->
<!-- speaker_note: I wasn't doing anything special here outside of heavily leveraging plan mode. -->
Claude will do it all.

![image:width:40%](assets/do-it-all.gif)

<!-- end_slide -->

The Spaghetti Factory Opens
===
<!-- alignment: center -->
<!-- font_size: 2 -->
<!-- pause -->
- Prompt for plan (sometimes do research before) -> review plan -> implement:
- Prompt for plan and do research -> review research -> discuss -> implement:
  - -> test -> feedback -> retest ->

<!-- pause -->
Warp Speed:

<!-- speaker_note: Left column is v0.5.0 — the TUI binary, search, themes, Zune sync. Playback is not in that cut. Right column is when rodio / Now Playing land, which is what the kit slide claims. -->
<!-- column_layout: [5, 5] -->
<!-- column: 0 -->
<!-- pause -->
Mar 20–21:
* 8 commits
* 8,074 lines of Rust
* 4,155 is the TUI
* 11 themes, `/` search
* no `rodio`
* ffmpeg + `aft-mtp-cli`
<!-- column: 1 -->
<!-- pause -->
<!-- speaker_note: By this commit zune-mtp is already in Cargo.toml (~2.6k lines, Mar 26). The kit slide still lists aft-mtp-cli as the MTP path — that's the story of the MVP, not the tree at playback. -->
+ Now Playing · Mar 27
* 8 days from commit 1
* 10,489 lines of Rust
* 5,621 TUI
* +2,477 for playback
* `rodio`
* `zune-mtp` - rust port of aft-mtp-cli
<!-- reset_layout -->
<!-- pause -->
aft-mtp-cli adds: 
- caching so we don't walk the zune library on every plugin (more songs -> more time)

CLean up these adds;
- Disk library cache — the big one. Cold zune-init walked the whole artist/album graph and took minutes. You added SaveCache / TryLoadFromCache (AFT-LIBRARY-CACHE-V1) at ~/.aft-library-cache, keyed by device serial. Second connect: load the file, skip the walk. After import, AutoSave kept it fresh.

zune-refresh — delete that cache and rebuild from the device. Upstream zune-init is just new Library(session) every time, no cache, no refresh command.

track-id: on stdout — after a successful import, print track-id:<mtp handle> so Rust can parse a stable object ID. Upstream ZuneImport returns silently.

create-playlist <name> <comma-separated-ids> — build an AbstractAVPlaylist from the IDs zune-import just printed. Not in upstream CLI.

<!-- pause -->

<!-- end_slide -->

zune-mtp
===
<!-- font_size: 2 -->
<!-- alignment: center -->
<!-- pause -->
<!-- speaker_note: libmtp-zune is a fork of libmtp -->
Built referencing aft-mtp-cli and the libmtp-zune
<!-- pause -->
BYOCredentials - mtpz data needed for zune sync
<!-- pause -->
A reverse engineered library written in C does this though [`libmtp-zune`](https://github.com/kbhomes/libmtp-zune)
<!-- pause -->
C!? We can't be having that here. Sorry Dennis Richie :'(
<!-- pause -->
Port it to Rust!
<!-- pause -->
`zune-mtp` ports that protocol knowledge — RSA-1024 signing, a
PSS-like certificate exchange, AES-128-CBC, CMAC key extraction — into a native IOKit transport, bypassing libusb entirely because libusb
can't do the data-out operations the handshake needs.
<!-- end_slide -->

The End cont.
===
<!-- font_size: 2 -->

<!-- alignment: center -->

<!-- pause -->
We did it! We're done!

<!-- pause -->
Talks to a zune, apple music can handle iPods.

<!-- pause -->
Not quite..

<!-- pause -->
C++, stdin/out reads, C libraries

<!-- end_slide -->

Claude build me a hammer
===
<!-- font_size: 2 -->

<!-- alignment: center -->

I wanted to start from scratch and I'm clueless so I asked Claude to take a look.
It suggested probing the device to understand what it support and do research on what's been done already.

<!-- pause -->

- Have Claude scaffold a throwaway probe binary, run it once against the real device,
read the result, delete or keep if it works.

- Honestly surprised I didn't brick it
<!-- end_slide -->

Everything IS a nail when you have a Claude Hammer
===
<!-- font_size: 2 -->

<!-- alignment: center -->

- We poke around and finally get the handshake woring.
<!-- pause -->
- `SendObjectInfo` → `SendObject` → a song is *on the device*.
- Sync, push, remove, device browsing
- aww CRUD, it works

<!-- end_slide -->

iPod
===
<!-- font_size: 2 -->
<!-- alignment: center -->
<!-- pause -->
Much easier than the Zune
<!-- pause -->
- No handshake, mounts as a USB storage device
<!-- pause --> 
- Real challenge: parse and write back the iTunesDB
<!-- pause -->
<!-- speaker_note: fortunately gtkpod did this already and allows us to access -->
- libgpod -> split from gtkpod, written in C
<!-- pause -->

<!-- speaker_note: I remember around this time I was brute force checking it over and over and over, very bad habit I probably wasted a lot of time here -->
<!-- pause -->
ipod-db does this
<!-- end_slide -->

Reject spaghett; return to Rust
===
<!-- font_size: 2 -->

<!-- alignment: center -->

<!-- pause -->
Library.xml has outlived it's usefulness.

<!-- pause -->
Replaced! We read the directory and build a cache. Updates as new files are added.

We swap (most of) ffmpeg for:
<!-- speaker_note: great for a library of mixed files -->
* `symphonia` - audio decoder
<!-- speaker_note: Zune only supports WMA, AAC, and MP3, so we need to transcode the file if it's incompatible -->
* `mp3lame-encoder` - JIT transcoding audio files
<!-- pause -->
* `lofty` - adds metadata parser and writer
<!-- speaker_note: lofty works across files my files -->

* `discid` — CD table-of-contents reads, LGPL, dynamically linked

<!-- end_slide -->

Time to refactor
===
<!-- font_size: 2 -->

<!-- alignment: center -->

<!-- pause -->
<!-- speaker_note: now that it works I better refactor this because I'd hate to have to answer for claude slop -->
<!-- speaker_note: by the time we get to this point we've got a 10k line god file for the UI -->
The unglamorous, load-bearing refactor work:

`app.rs` and `native.rs` had both grown into the kind of file where
"just add one more match arm" stops being a joke.

<!-- pause -->

Split into `tui/app/keys.rs`, `tui/app/events.rs`, decomposed `native.rs`
into focused pieces — same behavior, files a human can actually hold in
their head again.

<!-- end_slide -->

Using TDD correctly
===
<!-- font_size: 2 -->

<!-- alignment: center -->

The `DeviceSession` trait exists specifically so sync/remove/collect
logic can be tested against a fake — no hardware required, no flaky
USB-in-CI nonsense.

<!-- pause -->

Behavior asserted, not mocks — did the right bytes land in the right
file, not "was `write` called." Failing test first, then the code that
makes it pass.

<!-- end_slide -->

Stems
===
<!-- font_size: 2 -->

<!-- alignment: center -->

Press `M` on a playing track, get it split into live-toggleable
vocals/drums/bass/etc.

Feature drift no feature dream!

<!-- font_size: 2 -->

<!-- alignment: center -->
<!-- pause -->
<!-- speaker_note: I didn't want to bundle this with the install as it'd add gigs for the models and the AI part should be opt in -->
- uv - bundles up pytorch & model, and checkpoints for separate install
<!-- pause -->

Nothing installs at startup or during a library scan. First press of
`M`, zytunes asks, then bootstraps [`uv`](https://github.com/astral-sh/uv)
and pulls a pinned engine version into a managed location — only after
you say yes.

<!-- end_slide -->

2. A multi-pass architecture
===
<!-- font_size: 2 -->

<!-- alignment: center -->

One pass gets you six stems. Getting **backing vocals** separated from
the **lead** vocal needs a second, more specialized pass on top of that.

<!-- pause -->

The `hq-harmony` recipe cascades a Mel-Roformer karaoke model over the
already-isolated vocal stem — seven stems out, lead and backing split
cleanly, each pass's intermediate output feeding the next.

<!-- pause -->

We have Rock Band harmony vocals at home now.

<!-- end_slide -->

It's Really a thing I promise
=== 
<!-- speaker_note: Ctrl+E hands the terminal to zytunes-tui. q in the TUI returns to the deck. -->
<!-- pause -->
```bash +exec +acquire_terminal
/// if ! command -v zytunes-tui >/dev/null 2>&1; then
///   exec "$(git rev-parse --show-toplevel)/target/release/zytunes-tui"
/// fi
zytunes-tui
```
<!-- snippet: >

<!-- end_slide -->

In The End
===
<!-- font_size: 2 -->

<!-- alignment: center -->
<!-- pause -->
<!-- speaker_note: I got to focus a lot on tradeoffs and choices instead of getting bogged down into learning a single library -->
Since LLM's did all the heavy lifting I spent more time considering the system itself, and even with odd decisions made earlier, it was very easy to pivot. 
<!-- speaker_note:  -->
<!-- pause -->
I learned so much more when I had to make this talk, I hope y'all did too.

<!-- pause -->

TDD is clutch.

<!-- end_slide -->

Thank you
===
<!-- font_size: 2 -->

<!-- alignment: center -->

<!-- pause -->
`github.com/Danondso/zytunes-suite`
<!-- pause -->
