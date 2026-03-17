# Bracket Contrast Thresholds: Analysis and Recommendations

## The Core Question

Two decisions need grounding:

1. **When should auto mode intervene?** (What APCA Lc floor means "this bracket is getting lost against the background"?)
2. **How should it adjust?** (What adjustment strategy avoids mangling the theme?)

---

## What Brackets Actually Are (Perceptually)

Brackets in a code editor don't fit neatly into any standard accessibility category. They're:

- **Not body text** — nobody reads brackets linearly; they're structural anchors scanned in peripheral vision
- **Not decorative** — they carry semantic meaning (nesting depth), and rainbow-colored brackets specifically exist to make that depth visually parseable
- **Not icons or pictograms** — they're single characters rendered at the editor's monospace font size (typically 12–16px)
- **Closest analogy: "spot readable" symbolic content** — you need to *identify* them and *distinguish* them from neighbors, but not *read* them fluently

This places brackets in APCA's lower tiers, well below body text requirements.

## APCA Threshold Recommendations

### What the spec says

The APCA ARC Bronze Simple Mode defines these Lc tiers:

| Lc | Use case |
|----|----------|
| **90** | Preferred for body text |
| **75** | Minimum for body text (14px/700, 16px/500, 18px/400) |
| **60** | Minimum for content text you want people to read (24px/400, 16px/700) |
| **45** | Minimum for large/heavy headlines (36px/400, 24px/700); also fine-detail pictograms |
| **30** | Absolute minimum for "spot readable" non-content text (placeholder text, disabled text, large non-text elements) |
| **15** | Absolute minimum for any non-semantic discernible element (dividers, thick outlines) |

### Where brackets land

Brackets at typical editor font sizes (13–16px, regular weight, monospace) are:

- **Functionally spot-readable symbols** — you need to see which bracket is which color, not read a sentence
- **Surrounded by syntax-highlighted context** — they're never isolated, so they get spatial cues from neighboring tokens
- **Cyclic over a small palette** — with 6–8 accent colors, the *relationship between adjacent colors* matters as much as any single color's background contrast

This places the bracket use case **between Lc 30 and Lc 45**:

- **Lc 30** is the APCA-spec floor for "spot readable" — below this, the bracket is effectively invisible to many users against the background
- **Lc 45** is where fine-detail pictograms live — quite conservative for brackets, which are structurally simpler than pictograms

### Proposed thresholds

| | Current auto mode | Recommended | Reasoning |
|---|---|---|---|
| **Light theme APCA floor** | 35.0 | **35.0** | Light themes wash out quickly when accents get too close to the background. `Lc 35` is still well below body-text requirements while remaining conservative enough to catch visibly weak brackets. |
| **Dark theme APCA floor** | 30.0 | **30.0** | Dark themes are theoretically weaker at the same absolute Lc, but the bundled-theme capture sweep showed that raising dark themes to `35` made too many muted accents look washed out. `Lc 30` is the best empirical balance. |

### Why not 45 or 55?

The upstream code uses [ensure_minimum_contrast(bracket_color, editor_background, 55.0)](file:///Users/kai/claude_home/monokai_for_zed/reference/zed-brackets-auto-mode/crates/ui/src/utils/apca_contrast.rs#142-203). This is too high because:

1. **Lc 55 is near the content-text tier** (Lc 60), designed for text people actively read at 24px — way above what brackets need
2. At Lc 55, many theme accents in dark themes fail (they're deliberately muted/saturated), so the adjustment function has to significantly lighten, desaturate, or even snap to white/black
3. The result is that most dark themes with bracket colorization on look washed out or lose their accent palette entirely

**Any threshold much above Lc 40 will start mangling well-designed dark themes.** The "sweet spot" is Lc 30–35, which catches genuinely invisible brackets while leaving intentionally muted-but-visible accents alone.

---

## OKLab Adjacent-Distance Thresholds

### What the science says

The OKLab Just Noticeable Difference (JND) threshold is:

| dEOk | Interpretation |
|------|---------------|
| **< 0.01** | Virtually indistinguishable |
| **0.02** | JND — the threshold at which most people can just barely tell two colors apart |
| **0.04–0.05** | Clearly different but may still be confusable at a glance |
| **0.08–0.10** | Comfortably distinguishable in most viewing conditions |
| **0.15+** | Obviously different |

### What brackets need

Adjacent brackets need to be:

- **Distinguishable at a glance** without careful comparison — you're scanning code structure, not doing a color-matching exercise
- **Distinguishable in the presence of syntax highlighting** — surrounding token colors can mask subtle bracket-to-bracket differences
- **Distinguishable across a cyclic sequence** — the wrap-around pair (last accent → first accent) matters as much as any interior pair

This means the threshold needs to be **well above JND** — at least 3–5× the JND to be "comfortably rapid" rather than "barely noticeable."

### Proposed thresholds

| | Current auto mode | Recommended | Reasoning |
|---|---|---|---|
| **Light theme adjacent OKLab** | 0.10 | **0.08–0.10** | Light themes have brighter accents with more chroma headroom, so a slightly higher floor is achievable without distortion. 0.10 is a solid "4–5× JND" target. |
| **Dark theme adjacent OKLab** | 0.08 | **0.08–0.10** | Dark themes have less lightness headroom for their accents, so achieving high adjacent distance is harder. 0.08 (4× JND) is reasonable as a floor. |

The current values (0.10 light / 0.08 dark) are in the right range. The dark value could potentially be raised to 0.10 to match, since the reordering step (rather than color adjustment) is the main lever for improving adjacency.

---

## Adjustment Strategy Recommendations

### The current approach is sound

The proposed auto mode's adjustment strategy is fundamentally better than upstream because:

1. **Lightness-only OKLCH adjustment** preserves hue and chroma — the two properties that give a theme its visual identity
2. **Per-accent selective adjustment** means untouched accents stay pixel-identical to the theme
3. **Separation of concerns** (background contrast ≠ adjacent separation) avoids the upstream trap of fixing one problem by creating another
4. **Reordering as a separate step** is non-destructive to the colors themselves

### Final adjustment policy

The final policy is:

1. **Keep the APCA floors at `30` dark / `35` light.**
2. **Keep best-effort rescue for background failures.** Auto mode does not use an APCA abandon ceiling. If a color starts far below the floor but can still be rescued with a lightness-only adjustment inside the clamp range, it should still be rescued.
3. **Keep bidirectional `OKLCH` lightness search and choose the passing candidate with the smallest `OKLab` distance from the original.**
4. **Do not add polarity-based direction preferences, desaturation, hue shifting, or black/white fallback.**
5. **Treat severity as diagnostics, not as a public policy knob.** A near miss and a severe fail are useful to label in tests and analysis notes, but not to drive a separate "give up early" branch in auto mode.

This keeps `auto` aligned with its intended contract: a safety net that rescues weak bracket colors while remaining meaningfully less invasive than upstream.

## 4. Empirical Evaluation on Bundled Themes

To resolve the remaining tension between APCA polarity math (suggesting dark themes need Lc 35) and the APCA spec tier definition (spot readable minimum Lc 30), we ran the internal Zed bracket contrast capture tool over all bundled themes (One, Ayu, Gruvbox) testing an APCA threshold stack of `[25, 30, 35, 40]`.

### Findings from the Capture Sweep

1. **APCA 25 is too low for dark themes.** 
   At Lc 25, brackets in themes like *Gruvbox Dark Soft* blend so deeply into the background that they require significant focal effort to distinguish. This confirms that dipping below the APCA spec "spot readable" tier (Lc 30) compromises usability.
2. **APCA 35 feels slightly intrusive in dark themes.**
   While mathematically safer for reverse-polarity legibility, pushing the dark floor up to Lc 35 resulted in more aggressive, highly-luminescent adjustments across multiple theme families. The aesthetic cost to the theme author's intended mood was noticeably higher.
3. **APCA 30 provides the optimal balance.**
   At Lc 30, passing accents remain subtle but definitively identifiable. Failing accents are nudged just enough to break out of the background "mud" without screaming at the user.
4. **Bundled dark and light themes still need best-effort rescue.**
   The text-based sweep showed that *One Dark*, *Ayu Dark*, *Ayu Mirage*, and all three *Gruvbox Dark* variants fail the dark `Lc 30` floor, while bundled light themes such as *One Light* and *Ayu Light* also contain accents far below the light `Lc 35` floor. Auto mode should continue rescuing these cases when a lightness-only adjustment can do so cleanly.

### Conclusion on Thresholds

The empirical results confirm that the **current defaults (30 Dark / 35 Light)** are the correct targets.

While the polarity asymmetry of APCA suggests that dark themes might theoretically need higher absolute thresholds than light themes for equal perceptual weight, the visual density of code editing overrides that math. A subtle bracket on a dark background is acceptable; a washed-out bracket on a light background is painful. 

The 30/35 split serves as a "safety net" rather than a strict body-text target.

## Summary of Final Recommendations

1. **Keep APCA floors at 30 (Dark) / 35 (Light).** The capture sweeps confirm this is the optimal empirical balance between accessibility and theme preservation.
2. **Maintain standard OKLab Adjacent Separation (0.08 Dark / 0.10 Light).**
3. **Retain the "Minimize OKLab Distance" Direction Heuristic.** While pushing away from the background guarantees visibility, it creates an undesirable uniformity in lightness across adjusted palettes. Minimizing OKLab distance preserves the relative lightness mapping within the theme author's original palette design.
4. **Make reordering threshold-crossing only, with a narrow light-theme tolerance band.** Reordering is the most theme-violating intervention in auto mode, so it should only happen when the background-adjusted palette is materially below the adjacency target and the reordered palette actually reaches that target. In practice, light themes get a small tolerance band below `0.10` so a `0.099` near miss does not trigger a reorder by itself.
5. **Do not add an abandon ceiling.** A fixed "too broken to rescue" cutoff would knowingly leave bundled themes with unreadable brackets, which is not acceptable for the default safety-net mode.
