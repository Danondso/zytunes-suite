# zytunes presentation

A terminal slide deck about this repo, written for
[presenterm](https://mfontanini.github.io/presenterm/introduction.html).

## Install presenterm

```bash
brew install presenterm
```

## Run it

```bash
presenterm zytunes.md
```

Navigate with arrow keys / `j`/`k`, `q` to quit.

## Export

```bash
presenterm --export-html zytunes.md      # no extra deps
presenterm --export-pdf zytunes.md       # needs weasyprint
```

## Speaker notes

Run two instances — one for the audience, one for you:

```bash
presenterm zytunes.md --publish-speaker-notes
presenterm zytunes.md --listen-speaker-notes
```
