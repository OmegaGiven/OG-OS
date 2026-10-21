# Bundled fonts

Compiled into the binary (`include_bytes!` in
`src/viewers/pdf/tools/topdf/engine.rs`) as the typefaces of every
"Convert to PDF" document, so output is identical on Linux, macOS and
Windows without relying on installed fonts. The system's own fonts are only
consulted as a fallback for scripts these don't cover (e.g. CJK, Indic,
Arabic); set `OG_FILES_NO_SYSTEM_FONTS=1` to disable that.

## Liberation Fonts 2.1.5 (Sans, Serif, Mono — 4 styles each)

- Upstream: https://github.com/liberationfonts/liberation-fonts (files taken
  unmodified from the Arch Linux `ttf-liberation 2.1.5` package)
- Copyright (c) 2010 Google Corporation, (c) 2012 Red Hat, Inc.
- License: **SIL Open Font License 1.1** — see `Liberation-LICENSE.txt`.
  Bundling in software (including app stores) and embedding subsets in
  generated PDFs are both allowed.
- Metric-compatible with Arial, Times New Roman and Courier New, so
  converted Office documents keep their line breaks.
- Coverage: Latin (incl. extended), Greek, Cyrillic, Hebrew, plus common
  symbols.

Total 4.2 MiB.

| file | sha256 |
|---|---|
| LiberationMono-Bold.ttf | 1d8d631105d4cb6c562a0d90f591d6393007d71350c089aec81c37dc8da293ed |
| LiberationMono-BoldItalic.ttf | bac30bbb26308b1a925d233a520fa764c80526f632b75defbf2b37e402eb6b1a |
| LiberationMono-Italic.ttf | 02312ecd02a6b7a96db60deb20468430abb7d3a91974ef9a90bd46ff78ea338d |
| LiberationMono-Regular.ttf | 47ed5b5fcfe6b3c9228937b05de9c769f5fa55b777d539af0f65172f0a24c90b |
| LiberationSans-Bold.ttf | 769673c4355020b1e28a14c366a152da410ab6b16239fe883ebc35b73624835b |
| LiberationSans-BoldItalic.ttf | 4b3d18cbd1b8c068115be4072f43418b194994313f47dbfd3588808903a55d34 |
| LiberationSans-Italic.ttf | 8649cf5f2cbe4727549d78b714a3a7ee540718a13557769c4612fea105294c7e |
| LiberationSans-Regular.ttf | baccc64becc3eb7d104b7c84d99f5314a0a1f896e2b3ea6c2f22fc08d2003bee |
| LiberationSerif-Bold.ttf | 28f2d4300ee366d1ff9ca95df967a27e77987c87857fad0d9c85034405aae39d |
| LiberationSerif-BoldItalic.ttf | 8de6363e9ca6c1e6f539ab032bd9ad6abfe138638f4fc53f3c26ea788b7724c5 |
| LiberationSerif-Italic.ttf | c7681827cba3bddb54f4673d0052cb944bdeac0b83c29c238240617e699311da |
| LiberationSerif-Regular.ttf | 86b9ea1c2f41bed9d7c09ccad4abc2894b33df5de60e5bbbece5d48610911870 |
