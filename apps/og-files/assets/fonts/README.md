# Bundled fonts

Compiled into the binary (`include_bytes!` in
`src/viewers/pdf/tools/topdf/engine.rs`) as the typefaces of every
"Convert to PDF" document, so output is identical on Linux, macOS and
Windows without relying on installed fonts. The system's own fonts are only
consulted as a fallback for scripts these don't cover (e.g. CJK); set
`OG_FILES_NO_SYSTEM_FONTS=1` to disable that.

## GNU FreeFont 20120503 (FreeSans, FreeSerif, FreeMono — 4 styles each)

- Upstream: https://www.gnu.org/software/freefont/ —
  `freefont-otf-20120503.tar.gz` from https://ftp.gnu.org/gnu/freefont/
  (files taken unmodified from the Arch Linux `gnu-free-fonts 20120503-9`
  package, which ships that tarball's .otf files)
- Copyright 2002–2012 GNU FreeFont contributors
- License: **GPL-3.0-or-later with the font exception** — the same license
  family as og-files (GPL-3.0-or-later), so bundling is unproblematic:

  > As a special exception, if you create a document which uses this font,
  > and embed this font or unaltered portions of this font into the
  > document, this font does not by itself cause the resulting document to
  > be covered by the GNU General Public License. This exception does not
  > however invalidate any other reasons why the document might be covered
  > by the GNU General Public License.

  i.e. the PDFs og-files produces (which embed subsets of these fonts) are
  not themselves GPL-covered.
- Coverage: Latin (incl. extended), Greek, Cyrillic, Armenian, Georgian,
  Hebrew, Arabic (Sans/Mono partial), Devanagari, Bengali, Gurmukhi,
  Gujarati, Tamil, Thai and others, plus many symbols.

Total 6.7 MiB.

| file | sha256 |
|---|---|
| FreeSans.otf | d3e9138ccf76ef516cc6af867e4e7ed8776331947235824f924c5886186b0c80 |
| FreeSansBold.otf | 860760177d95ed4bd030cd2cd05f29ce23737dbd5ff7080954bb7f5802d0f42d |
| FreeSansOblique.otf | 16034e8e2a3ff4a988f15423bf2252b69c1f750feb9123a39dd65297c39737a9 |
| FreeSansBoldOblique.otf | d7385b41aed42d731cfec5c2b01933fa738931af0f8d8d258eddac9657228663 |
| FreeSerif.otf | aab33b5d0efd450d3ac3153b176f542315e171db79e494aa287cd940ae16231b |
| FreeSerifBold.otf | e5d664c1448e3976fbbc68b1d9ab69d00383efb376e67a3a6b79279233e152cc |
| FreeSerifItalic.otf | 4c815172f18223f5ecfae908d8fae0eed5708c8f12c257cfb59150464f9b8599 |
| FreeSerifBoldItalic.otf | 7d3a5e140353cf13dc2f51471bd33f88bd7464d159c72760a0b44a3a88948916 |
| FreeMono.otf | daf720e115bc462f721cdf3065697b75fea033bf46d4598d3a727fce4b036032 |
| FreeMonoBold.otf | f343654c66abd947ded29abab099f79e55edb93a27ff88e7a67b1d6da7d8017e |
| FreeMonoOblique.otf | 7cf6a9b570ade6a2e4cc88378fec3192be321a63d5657c8e894b108f77754741 |
| FreeMonoBoldOblique.otf | 4eda7c1df91a24ac7b40c721e89c2142d026a0a4526db2996e69eaacf0b75942 |

Why not Liberation / DejaVu / Noto / Typst's own fonts: those are under the
SIL OFL or Bitstream Vera licenses, which the FSF lists as free but
GPL-incompatible; FreeFont is GPL-3.0-or-later itself.
