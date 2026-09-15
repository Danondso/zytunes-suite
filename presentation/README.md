# zytunes presentation

A terminal slide deck about this repo, written for
[presenterm](https://mfontanini.github.io/presenterm/introduction.html).

## Install presenterm

```bash
brew install presenterm
```

## Run it

```bash
presenterm -x zytunes.md
```

`-x` enables snippet execution (needed for the MVP slide's
live `zytunes-tui` demo). Press `Ctrl+E` on that slide to
hand the terminal to the TUI; `q` in zytunes-tui returns to
the deck.

Navigate with arrow keys / `j`/`k`, `q` to quit.

Font sizes assume [Kitty 0.40+](https://sw.kovidgoyal.net/kitty/text-sizing-protocol/):
intro title and slide titles are 3×, body text is 2×. Other terminals
ignore this and stay at the emulator's font size — zoom with
`cmd+plus` / `ctrl+shift+plus` if you need the whole window larger.

## Export

```bash
presenterm --export-html zytunes.md      # no extra deps
presenterm --export-pdf zytunes.md       # needs weasyprint
```

## Speaker notes

Run two instances — one for the audience, one for you:

```bash
presenterm -x zytunes.md --publish-speaker-notes
presenterm zytunes.md --listen-speaker-notes
```
