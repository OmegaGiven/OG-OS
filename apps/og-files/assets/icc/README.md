# Colour profiles bundled with og-files

| File | Used for | Source | License |
|---|---|---|---|
| `CGATS001Compat-v2-micro.icc` (8.4 KB) | `/DefaultCMYK` colour space when PDF/A conversion meets DeviceCMYK content under the sRGB output intent (`viewers/pdf/tools/pdfa.rs`) | Compact-ICC-Profiles by Clinton Ingram, <https://github.com/saucecontrol/Compact-ICC-Profiles> (same file hayro ships) | CC0-1.0, see `CGATS001Compat-v2-micro.LICENSE.txt` |

The sRGB output-intent profile is not a file: `pdfa.rs` generates a compact
ICC v2 display profile (sRGB primaries adapted to D50, 1024-entry sRGB tone
curve) from the IEC 61966-2-1 parameters at run time.

The PDF/A converter embeds the 14 standard fonts from the Foxit/PDFium Type 1
set that the `hayro` renderer already compiles in (BSD-3-Clause, Copyright
2014 PDFium Authors / Foxit Software Inc.; see hayro-interpret's
`assets/LICENSE_FOXIT`). No extra font files are bundled.
