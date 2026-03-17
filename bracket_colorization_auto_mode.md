# Bracket Colorization Auto Mode

This worktree adds a configurable bracket colorization mode intended to preserve theme intent while avoiding weak bracket contrast in some palettes, especially in lighter themes.

## Summary

- Added a new language setting: `bracket_colorization_mode`
- Supported values:
  - `auto`
  - `theme`
- Default value: `auto`

This setting works alongside the existing `colorize_brackets` boolean:

- `colorize_brackets = false`: rainbow brackets remain disabled
- `colorize_brackets = true` and `bracket_colorization_mode = theme`: use theme accents exactly as provided
- `colorize_brackets = true` and `bracket_colorization_mode = auto`: preserve theme accents unless bracket-vs-background or adjacent bracket separation appears too weak

## Files Changed

- `assets/settings/default.json`
- `crates/settings_content/src/language.rs`
- `crates/language/src/language_settings.rs`
- `crates/settings_ui/src/page_data.rs`
- `crates/settings/src/vscode_import.rs`
- `crates/editor/src/bracket_colorization.rs`
- `crates/editor/src/editor.rs`
- `crates/editor/Cargo.toml`
- `crates/editor/examples/test_bracket_contrast.rs`

Demo files used for visual comparison:

- `bracket_demo_old.rs`
- `bracket_demo_new.rs`

## Setting Surface

The new setting is part of language settings:

```json
{
  "languages": {
    "Rust": {
      "colorize_brackets": true,
      "bracket_colorization_mode": "auto"
    }
  }
}
```

### Modes

#### `theme`

`theme` preserves the theme-provided accent order exactly.

This is the escape hatch for users who want rainbow brackets to follow the packaged theme colors with no automatic adjustment.

#### `auto`

`auto` acts as a safety net for weak bracket palettes without replacing the theme palette outright.

It does this by:

1. Starting from the active theme's accent list
2. Measuring both:
   - adjacent accent separation
   - contrast against the editor background
3. Adjusting only the specific accents that fail the background contrast check
4. Reordering the resulting palette only if adjacent separation is still materially weak and reordering can actually make it pass
5. Falling back to the original order when no treatment is needed

## How Auto Mode Works

The implementation lives in `crates/editor/src/bracket_colorization.rs`.

### Inputs

Auto mode uses:

- the active theme accent colors
- the active theme appearance (`Light` or `Dark`)
- the active theme editor background
- APCA for bracket-vs-background scoring
- OKLab distance for adjacent bracket scoring
- OKLCH lightness-only adjustment for failing accents

It does not replace the palette wholesale. It starts from the theme accents and only adjusts the accents that fail the background check.

`auto` is intentionally not a full theme-repair system. It makes a best effort to rescue weak bracket colors while preserving hue, chroma, and the original accent order whenever possible.

### Scoring

The current implementation computes a palette score based on:

- `min_adjacent_distance`
- `average_adjacent_distance`
- `min_background_contrast`
- `average_background_contrast`

These now mean:

- adjacent distance: Euclidean distance in `OKLab`
- background contrast: absolute `APCA Lc`

The minimum adjacent distance is treated as the main adjacency signal, since a single weak neighboring pair is enough to make rainbow brackets hard to read.

The minimum background contrast is tracked separately so the analysis can tell the difference between:

- a palette that is too similar to itself
- a palette that is too washed out against the editor background

### Appearance-Aware Thresholds

Auto mode uses different minimum thresholds for light and dark themes:

- light themes:
  - minimum background APCA = `35.0`
  - minimum adjacent OKLab distance = `0.10`
- dark themes:
  - minimum background APCA = `30.0`
  - minimum adjacent OKLab distance = `0.08`

These values were chosen from capture sweeps across bundled `One`, `Ayu`, and `Gruvbox` themes. Higher APCA floors preserved less of the original palette than necessary, especially in light themes.

Auto mode does not use an APCA "abandon ceiling". If a bracket color starts far below the floor but can still be rescued with a lightness-only adjustment inside the clamp range, auto mode still rescues it.

### Reordering

Auto mode now applies treatments in this order:

1. adjust only the accents that fail the background contrast check
2. rescore adjacent separation on that adjusted palette
3. reorder only if the adjusted palette is still weak

This ordering matters. If reordering happened first and individual color adjustment happened second, the second pass could erase the benefit of the reorder step.

If adjacent separation is still too weak after background-safe colors are derived, the code attempts to reorder the resulting palette to maximize separation between neighbors.

The current approach:

- preserves the original first accent in slot 0 so the reordered palette still starts from the theme's leading accent
- greedily picks the next unused accent that best separates from the previous one
- compares candidate orders by minimum adjacent distance first, then average adjacent distance

The reordered palette is only used if it turns a materially failing adjacent-separation result into a passing one. In light themes, the auto mode includes a narrow tolerance band below the `0.10` target so a `0.099` near miss does not force a full palette reorder.

At the palette level, the implementation currently distinguishes these auto strategies:

- unchanged
- background-adjusted only
- reordered only
- background-adjusted and reordered

### Analysis Helper

The auto-mode logic is also exposed through a pure analysis helper so it can be run without opening the editor UI.

The helper returns:

- the original palette
- the final palette used for bracket rendering
- the original and final palette scores
- the adjacent and background thresholds used
- whether the palette changed
- the strategy that was chosen

This keeps the CLI/example output aligned with the actual runtime logic instead of duplicating the heuristic elsewhere.

The helper also has an overrideable config surface for developer tooling, so the capture/example paths can sweep thresholds without changing production defaults.

## Rendering Model

Bracket colorization now effectively supports two highlight families:

- theme-order bracket highlights
- auto-order bracket highlights

The editor maps bracket depth to one of these families based on the current language setting, then applies the corresponding accent from either:

- the original theme order
- the auto palette after background adjustment and optional reordering

This keeps `theme` and `auto` behavior available simultaneously without changing the underlying bracket-depth logic.

## Caching

The derived `auto` palette is cached in editor accent state rather than being recomputed on every bracket-colorization pass.

Specifically:

- the raw theme accents are stored in `AccentData`
- the derived `auto` accent order is also stored in `AccentData`
- both are refreshed when theme/accent state changes

This matters because `colorize_brackets()` can run on more than just settings/theme changes. It also runs on existing editor refresh paths such as reparses, excerpt changes, and scroll-driven updates. The caching change removes unnecessary repeated palette analysis from that path.

The current implementation now reuses the cached `Arc<[Hsla]>` data directly for both theme and auto mode.

## Theme File Analysis

This worktree also adds a small example program that can run the bracket auto-mode logic directly against a theme JSON file without opening the full editor:

```sh
cargo run -p editor --example test_bracket_contrast -- assets/themes/one/one.json
```

The example:

1. parses the theme JSON
2. refines it through the real theme pipeline
3. runs the same bracket auto-mode analysis used by the editor
4. prints a report for each theme variant in the file

The report includes:

- whether adjacent and background thresholds pass before and after treatment
- whether bracket colors changed
- which strategy was used
- whether the palette was reordered
- whether any accents were background-adjusted
- the original palette
- the final palette
- the original and final palette scores

This is intended as a developer tool for quickly evaluating shipped or custom themes outside the editor UI.

## Real Render Capture Tool

This worktree also adds a macOS-only capture tool that renders a real Zed editor offscreen and saves PNGs:

```sh
cargo run -p zed --features visual-tests --bin zed_bracket_contrast_capture -- assets/themes/one/one.json
```

By default, it writes next to the input theme file and overwrites prior outputs.

The capture tool also supports overriding the upstream APCA threshold so current upstream behavior can be inspected at different settings:

```sh
cargo run -p zed --features visual-tests --bin zed_bracket_contrast_capture -- \
  --upstream-apca-threshold 45 \
  assets/themes/one/one.json
```

It can also generate a threshold-only upstream stack for side-by-side APCA comparisons:

```sh
cargo run -p zed --features visual-tests --bin zed_bracket_contrast_capture -- \
  --stack-upstream-apca-thresholds 35,45,55,65 \
  assets/themes/one/one.json
```

It can also generate threshold stacks for the proposed auto behavior:

```sh
cargo run -p zed --features visual-tests --bin zed_bracket_contrast_capture -- \
  --stack-auto-background-apca-thresholds 30,35,40,45 \
  assets/themes/one/one.json
```

```sh
cargo run -p zed --features visual-tests --bin zed_bracket_contrast_capture -- \
  --auto-background-apca-light-threshold 35 \
  --auto-background-apca-dark-threshold 30 \
  --stack-auto-adjacent-oklab-thresholds 0.08,0.10,0.12 \
  assets/themes/one/one.json
```

For each refined theme variant, it saves:

- a raw theme image
- an upstream-behavior image
- an auto-behavior image
- a stitched comparison image
- optionally, a stitched upstream-threshold stack image
- optionally, a stitched auto background-threshold stack image
- optionally, a stitched auto adjacent-threshold stack image
- a markdown report

The stitched comparison is ordered:

- `theme`
- `upstream`
- `auto`

### Upstream Baseline

The upstream baseline is not raw theme order. It mirrors the upstream recolorization behavior from `UPSTREAM_bracket_colorization.rs`:

- keep theme accent order
- run `ensure_minimum_contrast(..., editor_background, <apca-threshold>)` per accent
- do not reorder

That upstream helper uses APCA and may desaturate colors if a lightness-only adjustment does not meet the threshold.

By contrast, the proposed `auto` behavior in this branch:

- uses APCA as the background-readability metric
- uses OKLab distance for adjacent bracket separation
- only adjusts the specific accents that fail the APCA floor
- preserves hue and chroma, nudging only OKLCH lightness toward the nearest passing candidate
- never desaturates, hue-shifts, or snaps to black/white
- only reorders after the background-safe palette is derived and only if adjacent separation is still weak

This matters because the comparison image is intended to answer:

- what the raw theme looks like
- what current upstream behavior looks like
- what the proposed auto behavior looks like

### Image Labels

The image panes include treatment summaries directly in the editor content:

- `Raw Theme Order - unchanged`
- `Current Upstream Behavior - APCA <threshold> per-accent adjustment`
- `Proposed Auto Behavior - ...`

For auto mode, the label reflects the detected palette-level treatment:

- `unchanged`
- `minimal per-accent background fix only`
- `adjacency reorder only`
- `minimal per-accent background fix + adjacency reorder`

When `--stack-upstream-apca-thresholds` is used, the saved threshold stack image starts with the raw theme pane and then appends one upstream pane per APCA value in the order provided.

When `--stack-auto-background-apca-thresholds` or `--stack-auto-adjacent-oklab-thresholds` is used, the saved threshold stack image starts with the raw theme pane and then appends one auto pane per threshold value in the order provided.

### Demo Rendering

The capture images now use the real editor renderer rather than a synthetic SVG mockup.

That means the saved PNGs include:

- real Zed fonts
- real line numbers and editor layout
- real Rust syntax highlighting from the active theme
- bracket highlights on top of that themed code rendering

The syntax-highlighting fix here was important: the capture tool now explicitly applies the active syntax theme to the standalone `rust_lang()` instance before building the demo buffer. In normal app paths, that happens through the language registry, but this offscreen tool needed to do it itself.

This is important because the capture output is now closer to what a reviewer would actually see in the editor. It avoids the earlier SVG issues around guessed font metrics, guessed wrapping, and bracket-position drift.

## Why This Approach

The goal was to avoid three bad outcomes:

1. Regressing lighter themes by forcing a single accent ordering everywhere
2. Treating adjacent separation as the only problem when some themes also fail against the background
3. "Mangling" theme colors too aggressively for the sake of rainbow brackets

This approach tries to strike a middle ground:

- preserve theme colors
- preserve theme order when it is already good enough
- only adjust the colors that fail the background check
- only reorder when adjacent separation is still weak after that
- always provide an explicit `theme` override

## Tradeoffs

### Pros

- Better UX than a hardcoded global remapping
- Gives users an explicit escape hatch
- Avoids requiring theme authors to immediately change shipped palettes
- Keeps rainbow bracket behavior tied to the actual theme colors
- Distinguishes between adjacency problems and background-contrast problems
- Produces reviewer-facing captures using the same editor text rendering stack as Zed itself

### Cons

- Adds a new setting and more bracket-colorization complexity
- The heuristic is still intentionally simple, so it is not guaranteed to pick the "best" perceptual ordering or the "best" contrast-safe adjustment
- Some themes may still benefit from improving their accent palettes directly
- The current analysis reports final colors well, but it does not yet track per-accent provenance beyond palette-level treatment summaries
- The upstream comparison and the proposed `auto` mode are intentionally using different background-adjustment algorithms, so visual differences between those panes are not attributable to reordering alone

## Current Findings

Using the real-render capture tool changed the evaluation materially.

The main conclusions from the bundled-theme sweep are:

- upstream per-accent APCA adjustment is too aggressive for rainbow brackets in some shipped themes
- lowering the upstream APCA threshold reduces the damage somewhat, but the larger issue is still the adjustment strategy
- desaturating independent accent colors to satisfy a per-accent threshold can make the bracket set less distinguishable even when background contrast numerically improves
- APCA is still a better fit than the previous WCAG-ratio floor for bracket-vs-background scoring, but it works better as a selective floor than as a body-text-style target
- the tuned APCA floors that held up best across bundled `One`, `Ayu`, and `Gruvbox` themes were:
  - light: `35.0`
  - dark: `30.0`
- the tuned adjacent OKLab floors that held up best were:
  - light: `0.10`
  - dark: `0.08`
- the proposed `auto` behavior currently produces more usable results because it preserves hue/chroma, applies minimal per-accent OKLCH lightness correction only when needed, and treats adjacent separation as a separate concern

## Research Notes

The external research direction was still useful even though the implementation is now farther along.

The most relevant ideas were:

- bracket characters are not body text and should not be treated like paragraph content for contrast targeting
- they are closer to APCA's "spot readable" or symbolic/non-text use cases
- there does not appear to be a strong IDE-specific numeric standard for syntax or bracket contrast, so in-context rendered evaluation still matters
- editor themes often rely on selective contrast rather than maximizing every accent against the background

The practical takeaway is now reflected in the implementation:

- use `APCA` for bracket-vs-background measurement
- keep the APCA floor in the softer `30-35` band rather than body-text-style targets
- use a separate perceptual-distance metric for adjacent bracket levels
- perform minimal adjustment in `OKLCH`
- preserve hue and chroma, adjusting only lightness

## Tests

Focused tests were added around the new mode behavior:

- `theme` mode preserves accent order
- the reordering helper improves minimum OKLab adjacent distance on clearly weak palettes
- `auto` mode leaves strong palettes unchanged
- `auto` mode can adjust background-failing palettes without reordering
- `auto` mode adjusts only lightness for background fixes
- the analysis thresholds differ by appearance

Bracket colorization tests were also rerun to ensure the broader editor behavior stayed intact.

Verification command used:

```sh
cargo test -p editor bracket_colorization --lib
```

Additional verification used:

```sh
cargo run -p editor --example test_bracket_contrast -- assets/themes/one/one.json
```

Additional real-render verification used:

```sh
cargo run -p zed --features visual-tests --bin zed_bracket_contrast_capture -- assets/themes/one/one.json
```

Additional threshold-variation verification can be done with:

```sh
cargo run -p zed --features visual-tests --bin zed_bracket_contrast_capture -- \
  --upstream-apca-threshold 45 \
  assets/themes/one/one.json
```

And threshold-only stacks can be generated with:

```sh
cargo run -p zed --features visual-tests --bin zed_bracket_contrast_capture -- \
  --stack-upstream-apca-thresholds 35,45,55,65 \
  assets/themes/one/one.json
```

```sh
cargo run -p zed --features visual-tests --bin zed_bracket_contrast_capture -- \
  --stack-auto-background-apca-thresholds 30,35,40,45 \
  assets/themes/one/one.json assets/themes/ayu/ayu.json assets/themes/gruvbox/gruvbox.json
```

```sh
cargo run -p zed --features visual-tests --bin zed_bracket_contrast_capture -- \
  --auto-background-apca-light-threshold 35 \
  --auto-background-apca-dark-threshold 30 \
  --stack-auto-adjacent-oklab-thresholds 0.08,0.10,0.12 \
  assets/themes/one/one.json assets/themes/ayu/ayu.json assets/themes/gruvbox/gruvbox.json
```

## Possible Follow-Ups

The latest captures make one boundary clear: bracket colorization cannot fully rescue a weak theme.
Even if bracket-vs-background contrast is improved, a theme can still produce muddy results when
bracket colors sit next to syntax-highlighted tokens or when the accent palette itself has weak
internal rhythm. In practice, that means the feature should stay conservative.

Current direction:

- preserve theme intent first
- only intervene when a bracket color is genuinely getting lost
- avoid aggressive palette normalization or desaturation-heavy correction
- treat some failures as theme-quality issues rather than trying to algorithmically "fix" every palette

Practical follow-ups from here:

- Broaden the APCA/OKLab sweep to more shipped themes and community themes
- Re-evaluate whether adjacency reordering should stay as aggressive as it is now, since it can improve separation while still making the palette feel less natural
- Track per-accent provenance through adjustment + reorder if richer reports become useful
- Consider whether the minimum reorder-improvement gate should become configurable in dev tooling
- Revisit shipped theme accent palettes independently of bracket logic
- If stronger guarantees are ever required, consider a bracket-specific palette system instead of trying to infer everything from theme accents
