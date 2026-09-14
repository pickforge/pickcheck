# PickCheck brand assets

Artwork for PickCheck, formerly complexity-gate. It follows the Pickforge brand
kit in `pickforge/branding-visual`. The brand standard puts README assets in
`assets/branding/`; this repo keeps them under `docs/assets/branding/`.

| File | Use |
|---|---|
| `pickcheck-mark-128.svg` | Canonical mark. Source geometry for every other file. |
| `pickcheck-mark-512.png` | Raster mark for surfaces that cannot use SVG. |
| `pickcheck-lockup-horizontal.svg` | README hero, width 560. |
| `pickcheck-lockup-horizontal.png` | Raster lockup, 1440 × 360. |
| `pickcheck-stop-hook-mock.svg` | README product mock, width 900. |
| `pickcheck-og-image.svg` | Social card source, 1200 × 630. |
| `pickcheck-og-image.png` | Social card upload, 1200 × 630. |
| `pickcheck-social-generated.png` | Optional generated social card, 1730 × 909. See provenance. |
| `pickforge-studio-footer.svg` | Shared footer, copied from `branding-visual/assets/pickforge/`. |

## Mark

A 128 × 128 dark square with four off-white corner brackets. Inside, two branch
paths merge into one stem that ends at the single `#FF7A1A` ember dot: the
branches of a function, checked down to one result.

The lockup, social card, and mock reuse the mark's path coordinates. Only stroke
widths and the dot radius are sized for each scale. Keep the ember solid and
alone. Do not add a checkmark or gradient to the mark.

## PNG exports

PNGs are rendered from the SVGs with `rsvg-convert` and the Geist fonts
installed. Text stays vector in every SVG; no raster is embedded.

```sh
rsvg-convert -w 512 pickcheck-mark-128.svg -o pickcheck-mark-512.png
rsvg-convert -w 1440 pickcheck-lockup-horizontal.svg -o pickcheck-lockup-horizontal.png
rsvg-convert pickcheck-og-image.svg -o pickcheck-og-image.png
```

## Generated social card provenance

`pickcheck-social-generated.png` is an unedited copy of a Codex candidate made
with its built-in image generation tool from a text prompt that referenced a
generated hero candidate. The branch motif and product-name-first layout were
chosen from that candidate and redrawn as original SVG geometry in the files
above.

Limitations: it is a bitmap, 1730 × 909 instead of the requested 1200 × 630.
Fonts, colors, and geometry are approximate, with slight texture in the
lettering and dot. It is not a canonical asset. Prefer `pickcheck-og-image.png`.
