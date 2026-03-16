# Bracket Colorization Auto Mode

This worktree adds a configurable bracket colorization mode intended to preserve theme intent while avoiding weak adjacent bracket contrast in some palettes, especially in lighter themes.

## Summary

- Added a new language setting: `bracket_colorization_mode`
- Supported values:
  - `auto`
  - `theme`
- Default value: `auto`

This setting works alongside the existing `colorize_brackets` boolean:

- `colorize_brackets = false`: rainbow brackets remain disabled
- `colorize_brackets = true` and `bracket_colorization_mode = theme`: use theme accents exactly as provided
- `colorize_brackets = true` and `bracket_colorization_mode = auto`: preserve theme accents unless adjacent bracket contrast appears too weak

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

`auto` tries to improve adjacent bracket contrast without replacing the theme palette.

It does this by:

1. Starting from the active theme's accent list
2. Measuring adjacent accent separation
3. Using an appearance-aware threshold
4. Reordering the existing accents only if the original adjacent sequence is weak
5. Falling back to the original accent order if the reordered sequence is not meaningfully better

## How Auto Mode Works

The implementation lives in `crates/editor/src/bracket_colorization.rs`.

### Inputs

Auto mode uses:

- the active theme accent colors
- the active theme appearance (`Light` or `Dark`)
- a simple adjacent-color distance heuristic

It does not generate a new palette or mutate theme colors. It only decides whether to keep the theme order or use a reordered view of the same accent list.

### Scoring

The current implementation computes a palette score based on adjacent color distance in RGB space:

- `min_adjacent_distance`
- `average_adjacent_distance`

The minimum adjacent distance is treated as the main signal, since a single weak neighboring pair is enough to make rainbow brackets hard to read.

### Appearance-Aware Thresholds

Auto mode uses different minimum thresholds for light and dark themes:

- light themes require a higher minimum adjacent distance
- dark themes tolerate a slightly lower one

This is based on the observed failure mode from the screenshots: weak neighboring accents are much more noticeable on light backgrounds.

### Reordering

If the original palette fails the threshold, the code attempts to reorder the existing theme accents to maximize separation between neighbors.

The current approach:

- tries each accent as a possible starting point
- greedily picks the next unused accent that best separates from the previous one
- compares candidate orders by minimum adjacent distance first, then average adjacent distance

The reordered palette is only used if it meaningfully improves the weakest neighboring pair.

### Analysis Helper

The auto-mode logic is also exposed through a pure analysis helper so it can be run without opening the editor UI.

The helper returns:

- the original palette
- the final palette used for bracket rendering
- the original and final palette scores
- the threshold used for the current appearance
- whether the palette changed
- the strategy that was chosen

This keeps the CLI/example output aligned with the actual runtime logic instead of duplicating the heuristic elsewhere.

## Rendering Model

Bracket colorization now effectively supports two highlight families:

- theme-order bracket highlights
- auto-order bracket highlights

The editor maps bracket depth to one of these families based on the current language setting, then applies the corresponding accent from either:

- the original theme order
- the reordered auto palette

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

- whether adjacent bracket contrast is considered good or bad under the current heuristic
- whether bracket colors changed
- which strategy was used
- the original palette
- the final palette
- the original and final palette scores

This is intended as a developer tool for quickly evaluating shipped or custom themes outside the editor UI.

## Why This Approach

The goal was to avoid two bad outcomes:

1. Regressing lighter themes by forcing a single accent ordering everywhere
2. "Mangling" theme colors too aggressively for the sake of rainbow brackets

This approach tries to strike a middle ground:

- preserve theme colors
- preserve theme order when it is already good enough
- only intervene when the adjacent sequence is weak
- always provide an explicit `theme` override

## Tradeoffs

### Pros

- Better UX than a hardcoded global remapping
- Gives users an explicit escape hatch
- Avoids requiring theme authors to immediately change shipped palettes
- Keeps rainbow bracket behavior tied to the actual theme colors

### Cons

- Adds a new setting and more bracket-colorization complexity
- The heuristic is intentionally simple, so it is not guaranteed to pick the "best" perceptual ordering
- Some themes may still benefit from improving their accent palettes directly

## Tests

Focused tests were added around the new mode behavior:

- `theme` mode preserves accent order
- `auto` mode reorders clearly weak palettes
- `auto` mode leaves strong palettes unchanged

Bracket colorization tests were also rerun to ensure the broader editor behavior stayed intact.

Verification command used:

```sh
cargo test -p editor bracket_colorization --lib
```

Additional verification used:

```sh
cargo run -p editor --example test_bracket_contrast -- assets/themes/one/one.json
```

## Possible Follow-Ups

- Tune the thresholds based on additional theme samples
- Swap the RGB-distance heuristic for a more perceptual metric if needed
- Consider exposing an `enhanced` mode later if users want forced high-contrast ordering even when `auto` would preserve the theme
- Revisit shipped theme accent palettes independently of bracket logic
