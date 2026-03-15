//! Bracket highlights, also known as "rainbow brackets".
//! Uses tree-sitter queries from brackets.scm to capture bracket pairs,
//! and theme accents to colorize those.

use std::cmp::Ordering;
use std::ops::Range;
use std::sync::Arc;

use crate::{Editor, HighlightKey};
use collections::{HashMap, HashSet};
use gpui::{AppContext as _, Context, HighlightStyle, Hsla, Rgba};
use itertools::Itertools;
use language::language_settings::BracketColorizationMode;
use language::{BufferRow, BufferSnapshot, language_settings};
use multi_buffer::{Anchor, ExcerptId};
use palette::{
    FromColor, Oklab, Oklch,
    rgb::{LinSrgba, Srgba},
};
use theme::Appearance;
use ui::{ActiveTheme, utils::apca_contrast};

impl Editor {
    pub(crate) fn colorize_brackets(&mut self, invalidate: bool, cx: &mut Context<Editor>) {
        if !self.mode.is_full() {
            return;
        }

        if invalidate {
            self.bracket_fetched_tree_sitter_chunks.clear();
        }

        let (theme_accents, auto_accents) = self
            .accent_data
            .as_ref()
            .map(|accent_data| {
                (
                    accent_data.colors.0.clone(),
                    accent_data.auto_colors.0.clone(),
                )
            })
            .unwrap_or_else(|| {
                let theme_accents = Arc::from(cx.theme().accents().0.to_vec());
                let auto_accents = bracket_colorization_accents(
                    &theme_accents,
                    cx.theme().appearance,
                    cx.theme().colors().editor_background,
                    BracketColorizationMode::Auto,
                );
                (theme_accents, auto_accents)
            });
        let multi_buffer_snapshot = self.buffer().read(cx).snapshot(cx);

        let visible_excerpts = self.visible_excerpts(false, cx);
        let excerpt_data: Vec<(
            ExcerptId,
            BufferSnapshot,
            Range<usize>,
            BracketColorizationMode,
        )> = visible_excerpts
            .into_iter()
            .filter_map(|(excerpt_id, (buffer, _, buffer_range))| {
                let buffer_snapshot = buffer.read(cx).snapshot();
                let file = buffer_snapshot.file().cloned();
                let language_settings = language_settings::language_settings(
                    buffer_snapshot.language().map(|language| language.name()),
                    file.as_ref(),
                    cx,
                );
                if language_settings.colorize_brackets {
                    Some((
                        excerpt_id,
                        buffer_snapshot,
                        buffer_range,
                        language_settings.bracket_colorization_mode,
                    ))
                } else {
                    None
                }
            })
            .collect();

        let mut fetched_tree_sitter_chunks = excerpt_data
            .iter()
            .filter_map(|(excerpt_id, ..)| {
                Some((
                    *excerpt_id,
                    self.bracket_fetched_tree_sitter_chunks
                        .get(excerpt_id)
                        .cloned()?,
                ))
            })
            .collect::<HashMap<ExcerptId, HashSet<Range<BufferRow>>>>();

        let theme_accents_for_ranges = theme_accents.clone();
        let auto_accents_for_ranges = auto_accents.clone();
        let bracket_matches_by_accent = cx.background_spawn(async move {
            let anchors_in_multi_buffer = |current_excerpt: ExcerptId,
                                           text_anchors: [text::Anchor; 4]|
             -> Option<[Option<_>; 4]> {
                multi_buffer_snapshot
                    .anchors_in_excerpt(current_excerpt, text_anchors)?
                    .collect_array()
            };

            let bracket_matches_by_accent: HashMap<usize, Vec<Range<Anchor>>> =
                excerpt_data.into_iter().fold(
                    HashMap::default(),
                    |mut acc, (excerpt_id, buffer_snapshot, buffer_range, mode)| {
                        let fetched_chunks =
                            fetched_tree_sitter_chunks.entry(excerpt_id).or_default();
                        let accent_count = accent_count_for_mode(
                            mode,
                            theme_accents_for_ranges.len(),
                            auto_accents_for_ranges.len(),
                        );

                        if accent_count == 0 {
                            return acc;
                        }

                        let brackets_by_accent = compute_bracket_ranges(
                            &buffer_snapshot,
                            buffer_range,
                            fetched_chunks,
                            excerpt_id,
                            accent_count,
                            &anchors_in_multi_buffer,
                        );

                        for (accent_number, new_ranges) in brackets_by_accent {
                            let ranges = acc
                                .entry(highlight_key_for_mode(mode, accent_number))
                                .or_insert_with(Vec::<Range<Anchor>>::new);

                            for new_range in new_ranges {
                                let i = ranges
                                    .binary_search_by(|probe| {
                                        probe.start.cmp(&new_range.start, &multi_buffer_snapshot)
                                    })
                                    .unwrap_or_else(|i| i);
                                ranges.insert(i, new_range);
                            }
                        }

                        acc
                    },
                );

            (bracket_matches_by_accent, fetched_tree_sitter_chunks)
        });

        self.colorize_brackets_task = cx.spawn(async move |editor, cx| {
            if invalidate {
                editor
                    .update(cx, |editor, cx| {
                        editor.clear_highlights_with(
                            &mut |key| matches!(key, HighlightKey::ColorizeBracket(_)),
                            cx,
                        );
                    })
                    .ok();
            }

            let (bracket_matches_by_accent, updated_chunks) = bracket_matches_by_accent.await;

            editor
                .update(cx, |editor, cx| {
                    editor
                        .bracket_fetched_tree_sitter_chunks
                        .extend(updated_chunks);
                    for (highlight_key, bracket_highlights) in bracket_matches_by_accent {
                        let bracket_color = bracket_color_for_highlight_key(
                            highlight_key,
                            &theme_accents,
                            &auto_accents,
                        );
                        let style = HighlightStyle {
                            color: Some(bracket_color),
                            ..HighlightStyle::default()
                        };

                        editor.highlight_text_key(
                            HighlightKey::ColorizeBracket(highlight_key),
                            bracket_highlights,
                            style,
                            true,
                            cx,
                        );
                    }
                })
                .ok();
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PaletteScore {
    pub min_adjacent_distance: f32,
    pub average_adjacent_distance: f32,
    pub min_background_contrast: f32,
    pub average_background_contrast: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct AutoBracketColorizationConfig {
    pub minimum_background_apca_light: f32,
    pub minimum_background_apca_dark: f32,
    pub minimum_adjacent_oklab_light: f32,
    pub minimum_adjacent_oklab_dark: f32,
    pub lightness_clamp_min: f32,
    pub lightness_clamp_max: f32,
}

impl Default for AutoBracketColorizationConfig {
    fn default() -> Self {
        Self {
            minimum_background_apca_light: 35.0,
            minimum_background_apca_dark: 30.0,
            minimum_adjacent_oklab_light: 0.10,
            minimum_adjacent_oklab_dark: 0.08,
            lightness_clamp_min: 0.18,
            lightness_clamp_max: 0.92,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(test)]
enum BracketColorizationPaletteStrategy {
    ThemeOrder,
    PreservedSmallPalette,
    PreservedStrongPalette,
    PreservedWeakPalette,
    AdjustedBackgroundPalette,
    ReorderedWeakPalette,
    AdjustedBackgroundAndReorderedPalette,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg(test)]
struct BracketColorizationPaletteAnalysis {
    pub mode: BracketColorizationMode,
    pub appearance: Appearance,
    pub original_palette: Arc<[Hsla]>,
    pub final_palette: Arc<[Hsla]>,
    pub original_score: PaletteScore,
    pub final_score: PaletteScore,
    pub adjacent_threshold: Option<f32>,
    pub background_threshold: Option<f32>,
    pub changed: bool,
    pub strategy: BracketColorizationPaletteStrategy,
}

pub(crate) fn bracket_colorization_accents(
    accents: &[Hsla],
    appearance: Appearance,
    background: Hsla,
    mode: BracketColorizationMode,
) -> Arc<[Hsla]> {
    match mode {
        BracketColorizationMode::Theme => Arc::from(accents.to_vec()),
        BracketColorizationMode::Auto => compute_auto_bracket_palette(
            accents,
            appearance,
            background,
            AutoBracketColorizationConfig::default(),
        ),
    }
}

fn compute_auto_bracket_palette(
    accents: &[Hsla],
    appearance: Appearance,
    background: Hsla,
    config: AutoBracketColorizationConfig,
) -> Arc<[Hsla]> {
    let original_palette: Arc<[Hsla]> = Arc::from(accents.to_vec());
    let minimum_adjacent_distance = minimum_adjacent_distance(appearance, config);
    let minimum_adjacent_intervention_distance =
        minimum_adjacent_intervention_distance(appearance, config);
    let minimum_background_contrast = minimum_background_contrast(appearance, config);
    let background_adjusted =
        adjust_background_failures(accents, background, minimum_background_contrast, config);
    let background_adjusted_score = palette_score(&background_adjusted, background);
    let background_adjusted_changed = background_adjusted.as_slice() != accents;

    if accents.len() < 3 {
        return if background_adjusted_changed {
            Arc::from(background_adjusted)
        } else {
            original_palette
        };
    }

    if background_adjusted_score.min_adjacent_distance >= minimum_adjacent_distance {
        return if background_adjusted_changed {
            Arc::from(background_adjusted)
        } else {
            original_palette
        };
    }

    if background_adjusted_score.min_adjacent_distance >= minimum_adjacent_intervention_distance {
        return if background_adjusted_changed {
            Arc::from(background_adjusted)
        } else {
            original_palette
        };
    }

    let reordered = maximize_adjacent_separation(&background_adjusted, background);
    let reordered_score = palette_score(&reordered, background);

    if reordered_score.min_adjacent_distance >= minimum_adjacent_intervention_distance {
        Arc::from(reordered)
    } else if background_adjusted_changed {
        Arc::from(background_adjusted)
    } else {
        original_palette
    }
}

#[cfg(test)]
fn analyze_bracket_colorization_palette(
    accents: &[Hsla],
    appearance: Appearance,
    background: Hsla,
    mode: BracketColorizationMode,
) -> BracketColorizationPaletteAnalysis {
    analyze_bracket_colorization_palette_with_config(
        accents,
        appearance,
        background,
        mode,
        AutoBracketColorizationConfig::default(),
    )
}

#[cfg(test)]
fn analyze_bracket_colorization_palette_with_config(
    accents: &[Hsla],
    appearance: Appearance,
    background: Hsla,
    mode: BracketColorizationMode,
    config: AutoBracketColorizationConfig,
) -> BracketColorizationPaletteAnalysis {
    match mode {
        BracketColorizationMode::Theme => {
            let palette: Arc<[Hsla]> = Arc::from(accents.to_vec());
            let score = palette_score(accents, background);
            BracketColorizationPaletteAnalysis {
                mode,
                appearance,
                original_palette: palette.clone(),
                final_palette: palette,
                original_score: score,
                final_score: score,
                adjacent_threshold: None,
                background_threshold: None,
                changed: false,
                strategy: BracketColorizationPaletteStrategy::ThemeOrder,
            }
        }
        BracketColorizationMode::Auto => {
            auto_bracket_colorization_analysis(accents, appearance, background, config)
        }
    }
}

#[cfg(test)]
fn auto_bracket_colorization_analysis(
    accents: &[Hsla],
    appearance: Appearance,
    background: Hsla,
    config: AutoBracketColorizationConfig,
) -> BracketColorizationPaletteAnalysis {
    let original_palette: Arc<[Hsla]> = Arc::from(accents.to_vec());
    let original_score = palette_score(accents, background);
    let minimum_adjacent_distance = minimum_adjacent_distance(appearance, config);
    let minimum_adjacent_intervention_distance =
        minimum_adjacent_intervention_distance(appearance, config);
    let minimum_background_contrast = minimum_background_contrast(appearance, config);
    let background_adjusted =
        adjust_background_failures(accents, background, minimum_background_contrast, config);
    let background_adjusted_score = palette_score(&background_adjusted, background);
    let background_adjusted_changed = background_adjusted.as_slice() != accents;

    let strategy_for_unchanged = if accents.len() < 3 {
        Some(BracketColorizationPaletteStrategy::PreservedSmallPalette)
    } else if background_adjusted_score.min_adjacent_distance >= minimum_adjacent_distance {
        return build_analysis(
            appearance,
            original_palette,
            original_score,
            background_adjusted,
            background_adjusted_score,
            background_adjusted_changed,
            minimum_adjacent_distance,
            minimum_background_contrast,
            BracketColorizationPaletteStrategy::PreservedStrongPalette,
        );
    } else if background_adjusted_score.min_adjacent_distance
        >= minimum_adjacent_intervention_distance
    {
        Some(BracketColorizationPaletteStrategy::PreservedWeakPalette)
    } else {
        None
    };

    if let Some(strategy_for_unchanged) = strategy_for_unchanged {
        return build_analysis(
            appearance,
            original_palette,
            original_score,
            background_adjusted,
            background_adjusted_score,
            background_adjusted_changed,
            minimum_adjacent_distance,
            minimum_background_contrast,
            strategy_for_unchanged,
        );
    }

    let reordered = maximize_adjacent_separation(&background_adjusted, background);
    let reordered_score = palette_score(&reordered, background);

    if reordered_score.min_adjacent_distance >= minimum_adjacent_intervention_distance {
        BracketColorizationPaletteAnalysis {
            mode: BracketColorizationMode::Auto,
            appearance,
            original_palette,
            final_palette: Arc::from(reordered),
            original_score,
            final_score: reordered_score,
            adjacent_threshold: Some(minimum_adjacent_distance),
            background_threshold: Some(minimum_background_contrast),
            changed: true,
            strategy: if background_adjusted_changed {
                BracketColorizationPaletteStrategy::AdjustedBackgroundAndReorderedPalette
            } else {
                BracketColorizationPaletteStrategy::ReorderedWeakPalette
            },
        }
    } else {
        build_analysis(
            appearance,
            original_palette,
            original_score,
            background_adjusted,
            background_adjusted_score,
            background_adjusted_changed,
            minimum_adjacent_distance,
            minimum_background_contrast,
            BracketColorizationPaletteStrategy::PreservedWeakPalette,
        )
    }
}

#[cfg(test)]
fn build_analysis(
    appearance: Appearance,
    original_palette: Arc<[Hsla]>,
    original_score: PaletteScore,
    background_adjusted: Vec<Hsla>,
    background_adjusted_score: PaletteScore,
    background_adjusted_changed: bool,
    minimum_adjacent_distance: f32,
    minimum_background_contrast: f32,
    strategy: BracketColorizationPaletteStrategy,
) -> BracketColorizationPaletteAnalysis {
    let final_palette = if background_adjusted_changed {
        Arc::from(background_adjusted)
    } else {
        original_palette.clone()
    };
    let final_score = if background_adjusted_changed {
        background_adjusted_score
    } else {
        original_score
    };
    let strategy = if background_adjusted_changed {
        BracketColorizationPaletteStrategy::AdjustedBackgroundPalette
    } else {
        strategy
    };
    BracketColorizationPaletteAnalysis {
        mode: BracketColorizationMode::Auto,
        appearance,
        original_palette,
        final_palette,
        original_score,
        final_score,
        adjacent_threshold: Some(minimum_adjacent_distance),
        background_threshold: Some(minimum_background_contrast),
        changed: background_adjusted_changed,
        strategy,
    }
}

fn minimum_adjacent_distance(appearance: Appearance, config: AutoBracketColorizationConfig) -> f32 {
    match appearance {
        Appearance::Light => config.minimum_adjacent_oklab_light,
        Appearance::Dark => config.minimum_adjacent_oklab_dark,
    }
}

fn minimum_adjacent_intervention_distance(
    appearance: Appearance,
    config: AutoBracketColorizationConfig,
) -> f32 {
    match appearance {
        Appearance::Light => (config.minimum_adjacent_oklab_light - 0.005).max(0.0),
        Appearance::Dark => config.minimum_adjacent_oklab_dark,
    }
}

fn minimum_background_contrast(
    appearance: Appearance,
    config: AutoBracketColorizationConfig,
) -> f32 {
    match appearance {
        Appearance::Light => config.minimum_background_apca_light,
        Appearance::Dark => config.minimum_background_apca_dark,
    }
}

fn accent_count_for_mode(
    mode: BracketColorizationMode,
    theme_count: usize,
    auto_count: usize,
) -> usize {
    match mode {
        BracketColorizationMode::Theme => theme_count,
        BracketColorizationMode::Auto => auto_count,
    }
}

fn highlight_key_for_mode(mode: BracketColorizationMode, accent_number: usize) -> usize {
    accent_number * 2
        + match mode {
            BracketColorizationMode::Theme => 0,
            BracketColorizationMode::Auto => 1,
        }
}

fn bracket_color_for_highlight_key(
    highlight_key: usize,
    theme_accents: &[Hsla],
    auto_accents: &[Hsla],
) -> Hsla {
    let accent_number = highlight_key / 2;
    match highlight_key % 2 {
        0 => theme_accents[accent_number],
        _ => auto_accents[accent_number],
    }
}

fn maximize_adjacent_separation(accents: &[Hsla], background: Hsla) -> Vec<Hsla> {
    let mut best_order = accents.to_vec();
    let best_score = palette_score(&best_order, background);

    let mut used = vec![false; accents.len()];
    let mut order = Vec::with_capacity(accents.len());
    order.push(0);
    used[0] = true;

    while order.len() < accents.len() {
        let last = *order
            .last()
            .expect("adjacent-separation order is initialized with a starting accent");
        let first = order[0];
        let next = (0..accents.len())
            .filter(|&index| !used[index])
            .max_by(|&left, &right| {
                compare_candidates(accents, background, last, first, left, right)
            })
            .expect(
                "adjacent-separation selection must find an unused accent while building the order",
            );
        used[next] = true;
        order.push(next);
    }

    let reordered = order
        .into_iter()
        .map(|index| accents[index])
        .collect::<Vec<_>>();
    let score = palette_score(&reordered, background);
    if palette_score_is_better(score, best_score) {
        best_order = reordered;
    }

    best_order
}

fn compare_candidates(
    accents: &[Hsla],
    background: Hsla,
    last: usize,
    first: usize,
    left: usize,
    right: usize,
) -> Ordering {
    let left_score = (
        adjacent_distance(accents[last], accents[left], background),
        adjacent_distance(accents[first], accents[left], background),
    );
    let right_score = (
        adjacent_distance(accents[last], accents[right], background),
        adjacent_distance(accents[first], accents[right], background),
    );
    compare_float_tuple(left_score, right_score)
}

fn palette_score(accents: &[Hsla], background: Hsla) -> PaletteScore {
    if accents.is_empty() {
        return PaletteScore {
            min_adjacent_distance: f32::MAX,
            average_adjacent_distance: f32::MAX,
            min_background_contrast: f32::MAX,
            average_background_contrast: f32::MAX,
        };
    }

    let (min_adjacent_distance, average_adjacent_distance) = if accents.len() < 2 {
        (f32::MAX, f32::MAX)
    } else {
        let distances = accents
            .iter()
            .copied()
            .zip(accents.iter().copied().cycle().skip(1))
            .take(accents.len())
            .map(|(left, right)| adjacent_distance(left, right, background))
            .collect::<Vec<_>>();
        (
            distances.iter().copied().fold(f32::MAX, f32::min),
            distances.iter().sum::<f32>() / distances.len() as f32,
        )
    };

    let background_contrasts = accents
        .iter()
        .copied()
        .map(|accent| background_contrast(accent, background))
        .collect::<Vec<_>>();
    let min_background_contrast = background_contrasts
        .iter()
        .copied()
        .fold(f32::MAX, f32::min);
    let average_background_contrast =
        background_contrasts.iter().sum::<f32>() / background_contrasts.len() as f32;

    PaletteScore {
        min_adjacent_distance,
        average_adjacent_distance,
        min_background_contrast,
        average_background_contrast,
    }
}

fn palette_score_is_better(left: PaletteScore, right: PaletteScore) -> bool {
    left.min_adjacent_distance > right.min_adjacent_distance + 0.0001
        || ((left.min_adjacent_distance - right.min_adjacent_distance).abs() <= 0.0001
            && left.average_adjacent_distance > right.average_adjacent_distance + 0.0001)
}

fn compare_float_tuple(left: (f32, f32), right: (f32, f32)) -> Ordering {
    match left.0.partial_cmp(&right.0).unwrap_or(Ordering::Equal) {
        Ordering::Equal => left.1.partial_cmp(&right.1).unwrap_or(Ordering::Equal),
        ordering => ordering,
    }
}

fn adjacent_distance(left: Hsla, right: Hsla, background: Hsla) -> f32 {
    let left = color_to_oklab(composited_foreground(left, background));
    let right = color_to_oklab(composited_foreground(right, background));
    let lightness_delta = left.l - right.l;
    let a_delta = left.a - right.a;
    let b_delta = left.b - right.b;
    (lightness_delta * lightness_delta + a_delta * a_delta + b_delta * b_delta).sqrt()
}

fn adjust_background_failures(
    accents: &[Hsla],
    background: Hsla,
    minimum_background_contrast: f32,
    config: AutoBracketColorizationConfig,
) -> Vec<Hsla> {
    accents
        .iter()
        .copied()
        .map(|accent| {
            adjust_color_for_background(accent, background, minimum_background_contrast, config)
        })
        .collect()
}

fn adjust_color_for_background(
    color: Hsla,
    background: Hsla,
    minimum_background_contrast: f32,
    config: AutoBracketColorizationConfig,
) -> Hsla {
    if background_contrast(color, background) >= minimum_background_contrast {
        return color;
    }

    let original = color_to_oklab(color);
    let darker_candidate = adjusted_lightness_candidate(
        color,
        background,
        minimum_background_contrast,
        config.lightness_clamp_min,
    );
    let lighter_candidate = adjusted_lightness_candidate(
        color,
        background,
        minimum_background_contrast,
        config.lightness_clamp_max,
    );

    match (darker_candidate, lighter_candidate) {
        (Some(darker_candidate), Some(lighter_candidate)) => {
            let darker_distance = perceptual_distance(original, color_to_oklab(darker_candidate));
            let lighter_distance = perceptual_distance(original, color_to_oklab(lighter_candidate));
            if darker_distance <= lighter_distance {
                darker_candidate
            } else {
                lighter_candidate
            }
        }
        (Some(darker_candidate), None) => darker_candidate,
        (None, Some(lighter_candidate)) => lighter_candidate,
        (None, None) => color,
    }
}

fn adjusted_lightness_candidate(
    color: Hsla,
    background: Hsla,
    minimum_background_contrast: f32,
    target_lightness: f32,
) -> Option<Hsla> {
    let original = color_to_oklch(color);
    let lightness_delta = target_lightness - original.l;

    if lightness_delta.abs() <= f32::EPSILON {
        return None;
    }

    (1..=128).find_map(|step| {
        let amount = step as f32 / 128.0;
        let candidate = oklch_to_hsla(
            Oklch {
                l: (original.l + lightness_delta * amount).clamp(0.0, 1.0),
                chroma: original.chroma,
                hue: original.hue,
            },
            color.a,
        );
        (background_contrast(candidate, background) >= minimum_background_contrast)
            .then_some(candidate)
    })
}

fn background_contrast(foreground: Hsla, background: Hsla) -> f32 {
    apca_contrast(composited_foreground(foreground, background), background).abs()
}

fn composited_foreground(foreground: Hsla, background: Hsla) -> Hsla {
    let background_rgba = Rgba::from(background);
    let foreground_rgba = background_rgba.blend(Rgba::from(foreground));
    Hsla::from(foreground_rgba)
}

fn color_to_oklab(color: Hsla) -> Oklab {
    let rgba: Srgba = color_to_palette_rgba(color);
    let linear: LinSrgba = rgba.into_linear();
    Oklab::from_color(linear)
}

fn color_to_oklch(color: Hsla) -> Oklch {
    let rgba: Srgba = color_to_palette_rgba(color);
    let linear: LinSrgba = rgba.into_linear();
    Oklch::from_color(linear)
}

fn oklch_to_hsla(color: Oklch, alpha: f32) -> Hsla {
    let linear_rgba = LinSrgba::from_color(color);
    let rgba: Srgba = Srgba::from_linear(linear_rgba);
    let (red, green, blue, _): (f32, f32, f32, f32) = rgba.into_components();
    Hsla::from(Rgba {
        r: red.clamp(0.0, 1.0),
        g: green.clamp(0.0, 1.0),
        b: blue.clamp(0.0, 1.0),
        a: alpha,
    })
}

fn color_to_palette_rgba(color: Hsla) -> Srgba {
    let rgba = Rgba::from(color);
    Srgba::new(rgba.r, rgba.g, rgba.b, rgba.a)
}

fn perceptual_distance(left: Oklab, right: Oklab) -> f32 {
    let lightness_delta = left.l - right.l;
    let a_delta = left.a - right.a;
    let b_delta = left.b - right.b;
    (lightness_delta * lightness_delta + a_delta * a_delta + b_delta * b_delta).sqrt()
}

fn compute_bracket_ranges(
    buffer_snapshot: &BufferSnapshot,
    buffer_range: Range<usize>,
    fetched_chunks: &mut HashSet<Range<BufferRow>>,
    excerpt_id: ExcerptId,
    accents_count: usize,
    anchors_in_multi_buffer: &impl Fn(ExcerptId, [text::Anchor; 4]) -> Option<[Option<Anchor>; 4]>,
) -> Vec<(usize, Vec<Range<Anchor>>)> {
    buffer_snapshot
        .fetch_bracket_ranges(buffer_range.start..buffer_range.end, Some(fetched_chunks))
        .into_iter()
        .flat_map(|(chunk_range, pairs)| {
            if fetched_chunks.insert(chunk_range) {
                pairs
            } else {
                Vec::new()
            }
        })
        .filter_map(|pair| {
            let color_index = pair.color_index?;

            let buffer_open_range = buffer_snapshot.anchor_range_around(pair.open_range);
            let buffer_close_range = buffer_snapshot.anchor_range_around(pair.close_range);
            let [
                buffer_open_range_start,
                buffer_open_range_end,
                buffer_close_range_start,
                buffer_close_range_end,
            ] = anchors_in_multi_buffer(
                excerpt_id,
                [
                    buffer_open_range.start,
                    buffer_open_range.end,
                    buffer_close_range.start,
                    buffer_close_range.end,
                ],
            )?;
            let multi_buffer_open_range = buffer_open_range_start.zip(buffer_open_range_end);
            let multi_buffer_close_range = buffer_close_range_start.zip(buffer_close_range_end);

            let mut ranges = Vec::with_capacity(2);
            if let Some((open_start, open_end)) = multi_buffer_open_range {
                ranges.push(open_start..open_end);
            }
            if let Some((close_start, close_end)) = multi_buffer_close_range {
                ranges.push(close_start..close_end);
            }
            if ranges.is_empty() {
                None
            } else {
                Some((color_index % accents_count, ranges))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{cmp, sync::Arc, time::Duration};

    use super::*;
    use crate::{
        DisplayPoint, EditorMode, EditorSnapshot, MoveToBeginning, MoveToEnd, MoveUp,
        display_map::{DisplayRow, ToDisplayPoint},
        editor_tests::init_test,
        test::{
            editor_lsp_test_context::EditorLspTestContext, editor_test_context::EditorTestContext,
        },
    };
    use collections::HashSet;
    use fs::FakeFs;
    use gpui::{UpdateGlobal as _, hsla};
    use indoc::indoc;
    use itertools::Itertools;
    use language::language_settings::BracketColorizationMode;
    use language::{Capability, markdown_lang};
    use languages::rust_lang;
    use multi_buffer::{MultiBuffer, PathKey};
    use pretty_assertions::assert_eq;
    use project::Project;
    use rope::Point;
    use serde_json::json;
    use settings::{AccentContent, SettingsStore};
    use text::{Bias, OffsetRangeExt, ToOffset};
    use theme::{Appearance, ThemeStyleContent};

    use util::{path, post_inc};

    fn light_editor_background() -> Hsla {
        hsla(0.0, 0.0, 0.98, 1.0)
    }

    fn dark_editor_background() -> Hsla {
        hsla(0.0, 0.0, 0.12, 1.0)
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum BackgroundFailureSeverity {
        NearMiss,
        ModerateFail,
        SevereFail,
    }

    fn background_failure_severity(
        current_contrast: f32,
        minimum_background_contrast: f32,
        has_passing_candidate: bool,
    ) -> Option<BackgroundFailureSeverity> {
        if current_contrast >= minimum_background_contrast {
            return None;
        }

        if !has_passing_candidate {
            return Some(BackgroundFailureSeverity::SevereFail);
        }

        let contrast_deficit = minimum_background_contrast - current_contrast;
        if contrast_deficit <= 5.0 {
            Some(BackgroundFailureSeverity::NearMiss)
        } else if contrast_deficit <= 15.0 {
            Some(BackgroundFailureSeverity::ModerateFail)
        } else {
            Some(BackgroundFailureSeverity::SevereFail)
        }
    }

    #[test]
    fn test_theme_bracket_colorization_mode_preserves_theme_order() {
        let accents = vec![
            hsla(0.0, 1.0, 0.5, 1.0),
            hsla(0.08, 1.0, 0.5, 1.0),
            hsla(0.66, 1.0, 0.5, 1.0),
            hsla(0.40, 1.0, 0.5, 1.0),
        ];

        let analysis = analyze_bracket_colorization_palette(
            &accents,
            Appearance::Light,
            light_editor_background(),
            BracketColorizationMode::Theme,
        );
        let palette = bracket_colorization_accents(
            &accents,
            Appearance::Light,
            light_editor_background(),
            BracketColorizationMode::Theme,
        );

        assert_eq!(
            analysis.strategy,
            BracketColorizationPaletteStrategy::ThemeOrder
        );
        assert!(!analysis.changed);
        assert_eq!(palette.as_ref(), accents.as_slice());
    }

    #[test]
    fn test_auto_bracket_colorization_mode_reorders_weak_palette() {
        let accents = vec![
            hsla(0.0, 1.0, 0.68, 1.0),
            hsla(0.02, 1.0, 0.68, 1.0),
            hsla(0.34, 1.0, 0.68, 1.0),
            hsla(0.36, 1.0, 0.68, 1.0),
        ];

        let original_score = palette_score(&accents, dark_editor_background());
        let reordered = maximize_adjacent_separation(&accents, dark_editor_background());
        let reordered_score = palette_score(&reordered, dark_editor_background());

        assert_ne!(reordered.as_slice(), accents.as_slice());
        assert!(reordered_score.min_adjacent_distance > original_score.min_adjacent_distance);
    }

    #[test]
    fn test_auto_bracket_colorization_mode_does_not_reorder_without_crossing_threshold() {
        let accents = vec![
            hsla(0.0, 1.0, 0.68, 1.0),
            hsla(0.02, 1.0, 0.68, 1.0),
            hsla(0.34, 1.0, 0.68, 1.0),
            hsla(0.36, 1.0, 0.68, 1.0),
        ];
        let reordered = maximize_adjacent_separation(&accents, dark_editor_background());
        let reordered_score = palette_score(&reordered, dark_editor_background());
        let config = AutoBracketColorizationConfig {
            minimum_background_apca_light: 0.0,
            minimum_background_apca_dark: 0.0,
            minimum_adjacent_oklab_light: reordered_score.min_adjacent_distance + 0.001,
            minimum_adjacent_oklab_dark: reordered_score.min_adjacent_distance + 0.001,
            ..AutoBracketColorizationConfig::default()
        };

        let analysis = analyze_bracket_colorization_palette_with_config(
            &accents,
            Appearance::Dark,
            dark_editor_background(),
            BracketColorizationMode::Auto,
            config,
        );

        assert_eq!(
            analysis.strategy,
            BracketColorizationPaletteStrategy::PreservedWeakPalette
        );
        assert!(!analysis.changed);
        assert_eq!(analysis.final_palette.as_ref(), accents.as_slice());
        assert!(
            analysis.final_score.min_adjacent_distance
                < analysis.adjacent_threshold.unwrap_or_default()
        );
    }

    #[test]
    fn test_auto_bracket_colorization_mode_reorders_when_threshold_is_crossed() {
        let accents = vec![
            hsla(0.0, 1.0, 0.68, 1.0),
            hsla(0.02, 1.0, 0.68, 1.0),
            hsla(0.34, 1.0, 0.68, 1.0),
            hsla(0.36, 1.0, 0.68, 1.0),
        ];
        let original_score = palette_score(&accents, dark_editor_background());
        let reordered = maximize_adjacent_separation(&accents, dark_editor_background());
        let reordered_score = palette_score(&reordered, dark_editor_background());
        let config = AutoBracketColorizationConfig {
            minimum_background_apca_light: 0.0,
            minimum_background_apca_dark: 0.0,
            minimum_adjacent_oklab_light: reordered_score.min_adjacent_distance - 0.001,
            minimum_adjacent_oklab_dark: reordered_score.min_adjacent_distance - 0.001,
            ..AutoBracketColorizationConfig::default()
        };

        let analysis = analyze_bracket_colorization_palette_with_config(
            &accents,
            Appearance::Dark,
            dark_editor_background(),
            BracketColorizationMode::Auto,
            config,
        );

        assert!(original_score.min_adjacent_distance < config.minimum_adjacent_oklab_dark);
        assert!(reordered_score.min_adjacent_distance >= config.minimum_adjacent_oklab_dark);
        assert_eq!(
            analysis.strategy,
            BracketColorizationPaletteStrategy::ReorderedWeakPalette
        );
        assert!(analysis.changed);
        assert_eq!(analysis.final_palette.as_ref(), reordered.as_slice());
    }

    #[test]
    fn test_auto_bracket_colorization_mode_keeps_reorder_that_crosses_intervention_band() {
        let accents = vec![
            hsla(0.0, 1.0, 0.68, 1.0),
            hsla(0.02, 1.0, 0.68, 1.0),
            hsla(0.34, 1.0, 0.68, 1.0),
            hsla(0.36, 1.0, 0.68, 1.0),
        ];
        let original_score = palette_score(&accents, light_editor_background());
        let reordered = maximize_adjacent_separation(&accents, light_editor_background());
        let reordered_score = palette_score(&reordered, light_editor_background());
        let config = AutoBracketColorizationConfig {
            minimum_background_apca_light: 0.0,
            minimum_background_apca_dark: 0.0,
            minimum_adjacent_oklab_light: reordered_score.min_adjacent_distance + 0.003,
            minimum_adjacent_oklab_dark: reordered_score.min_adjacent_distance + 0.003,
            ..AutoBracketColorizationConfig::default()
        };

        assert!(
            original_score.min_adjacent_distance
                < minimum_adjacent_intervention_distance(Appearance::Light, config,)
        );
        assert!(
            reordered_score.min_adjacent_distance
                < minimum_adjacent_distance(Appearance::Light, config,)
        );
        assert!(
            reordered_score.min_adjacent_distance
                >= minimum_adjacent_intervention_distance(Appearance::Light, config,)
        );

        let analysis = analyze_bracket_colorization_palette_with_config(
            &accents,
            Appearance::Light,
            light_editor_background(),
            BracketColorizationMode::Auto,
            config,
        );

        assert_eq!(
            analysis.strategy,
            BracketColorizationPaletteStrategy::ReorderedWeakPalette
        );
        assert!(analysis.changed);
        assert_eq!(analysis.final_palette.as_ref(), reordered.as_slice());
        assert!(
            analysis.final_score.min_adjacent_distance
                < analysis.adjacent_threshold.unwrap_or_default()
        );
    }

    #[test]
    fn test_compute_auto_bracket_palette_reorders_after_background_adjustment() {
        let candidate_palettes = [
            vec![
                hsla(0.0, 1.0, 0.24, 1.0),
                hsla(0.02, 1.0, 0.24, 1.0),
                hsla(0.34, 1.0, 0.24, 1.0),
                hsla(0.36, 1.0, 0.24, 1.0),
            ],
            vec![
                hsla(0.0, 0.9, 0.28, 1.0),
                hsla(0.02, 0.9, 0.28, 1.0),
                hsla(0.34, 0.9, 0.28, 1.0),
                hsla(0.36, 0.9, 0.28, 1.0),
            ],
            vec![
                hsla(0.58, 1.0, 0.28, 1.0),
                hsla(0.60, 1.0, 0.28, 1.0),
                hsla(0.12, 1.0, 0.28, 1.0),
                hsla(0.14, 1.0, 0.28, 1.0),
            ],
        ];
        let appearances_and_backgrounds = [
            (Appearance::Dark, dark_editor_background()),
            (Appearance::Light, light_editor_background()),
        ];
        let background_thresholds = [30.0, 35.0, 45.0, 55.0, 70.0, 80.0];
        let adjacency_thresholds = [0.06, 0.08, 0.10, 0.12];

        for accents in candidate_palettes {
            for (appearance, background) in appearances_and_backgrounds {
                for minimum_background_contrast in background_thresholds {
                    for minimum_adjacent_distance in adjacency_thresholds {
                        let config = AutoBracketColorizationConfig {
                            minimum_background_apca_light: minimum_background_contrast,
                            minimum_background_apca_dark: minimum_background_contrast,
                            minimum_adjacent_oklab_light: minimum_adjacent_distance,
                            minimum_adjacent_oklab_dark: minimum_adjacent_distance,
                            ..AutoBracketColorizationConfig::default()
                        };

                        let analysis = analyze_bracket_colorization_palette_with_config(
                            &accents,
                            appearance,
                            background,
                            BracketColorizationMode::Auto,
                            config,
                        );

                        if analysis.strategy
                            != BracketColorizationPaletteStrategy::AdjustedBackgroundAndReorderedPalette
                        {
                            continue;
                        }

                        let palette =
                            compute_auto_bracket_palette(&accents, appearance, background, config);

                        assert_eq!(palette.as_ref(), analysis.final_palette.as_ref());
                        return;
                    }
                }
            }
        }

        panic!("expected to find a palette where auto mode reorders after background adjustment");
    }

    #[test]
    fn test_auto_bracket_colorization_mode_preserves_strong_palette() {
        let accents = vec![
            hsla(0.0, 1.0, 0.78, 1.0),
            hsla(0.16, 1.0, 0.78, 1.0),
            hsla(0.33, 1.0, 0.78, 1.0),
            hsla(0.66, 1.0, 0.78, 1.0),
        ];
        let config = AutoBracketColorizationConfig {
            minimum_background_apca_light: 0.0,
            minimum_background_apca_dark: 0.0,
            ..AutoBracketColorizationConfig::default()
        };

        let analysis = analyze_bracket_colorization_palette_with_config(
            &accents,
            Appearance::Dark,
            dark_editor_background(),
            BracketColorizationMode::Auto,
            config,
        );
        let palette = analysis.final_palette.clone();

        assert_eq!(
            analysis.strategy,
            BracketColorizationPaletteStrategy::PreservedStrongPalette
        );
        assert!(!analysis.changed);
        assert_eq!(palette.as_ref(), accents.as_slice());
    }

    #[test]
    fn test_auto_bracket_colorization_mode_adjusts_only_lightness_for_background_failures() {
        let accents = vec![
            hsla(0.58, 1.0, 0.28, 1.0),
            hsla(0.12, 1.0, 0.28, 1.0),
            hsla(0.22, 0.9, 0.76, 1.0),
        ];

        let analysis = analyze_bracket_colorization_palette(
            &accents,
            Appearance::Light,
            light_editor_background(),
            BracketColorizationMode::Auto,
        );
        let original = color_to_oklch(accents[2]);
        let adjusted = color_to_oklch(analysis.final_palette[2]);

        assert!(analysis.changed);
        assert!((original.chroma - adjusted.chroma).abs() < 0.0001);
        assert!(
            (original.hue.into_positive_degrees() - adjusted.hue.into_positive_degrees()).abs()
                < 0.001
        );
        assert_ne!(original.l, adjusted.l);
    }

    #[test]
    fn test_auto_bracket_colorization_scores_use_appearance_specific_thresholds() {
        let config = AutoBracketColorizationConfig::default();

        assert_eq!(
            minimum_background_contrast(Appearance::Light, config),
            config.minimum_background_apca_light
        );
        assert_eq!(
            minimum_background_contrast(Appearance::Dark, config),
            config.minimum_background_apca_dark
        );
        assert_eq!(
            minimum_adjacent_distance(Appearance::Light, config),
            config.minimum_adjacent_oklab_light
        );
        assert_eq!(
            minimum_adjacent_distance(Appearance::Dark, config),
            config.minimum_adjacent_oklab_dark
        );
        assert_eq!(
            minimum_adjacent_intervention_distance(Appearance::Light, config),
            config.minimum_adjacent_oklab_light - 0.005
        );
        assert_eq!(
            minimum_adjacent_intervention_distance(Appearance::Dark, config),
            config.minimum_adjacent_oklab_dark
        );
    }

    #[test]
    fn test_auto_bracket_colorization_mode_adjusts_background_failures_without_reordering() {
        let accents = vec![
            hsla(0.58, 1.0, 0.28, 1.0),
            hsla(0.12, 1.0, 0.28, 1.0),
            hsla(0.22, 0.9, 0.76, 1.0),
        ];

        let analysis = analyze_bracket_colorization_palette(
            &accents,
            Appearance::Light,
            light_editor_background(),
            BracketColorizationMode::Auto,
        );

        assert_eq!(
            analysis.strategy,
            BracketColorizationPaletteStrategy::AdjustedBackgroundPalette
        );
        assert!(analysis.changed);
        assert_eq!(analysis.final_palette.len(), accents.len());
        assert_eq!(analysis.final_palette[0], accents[0]);
        assert_eq!(analysis.final_palette[1], accents[1]);
        assert_ne!(analysis.final_palette[2], accents[2]);
        assert!(
            analysis.final_score.min_background_contrast
                >= analysis.background_threshold.unwrap_or_default()
        );
    }

    #[test]
    fn test_auto_bracket_colorization_mode_preserves_light_near_miss_palette() {
        let accents = vec![
            Hsla::from(Rgba::try_from("#CC241D").expect("valid color")),
            Hsla::from(Rgba::try_from("#98971A").expect("valid color")),
            Hsla::from(Rgba::try_from("#D79921").expect("valid color")),
            Hsla::from(Rgba::try_from("#458588").expect("valid color")),
            Hsla::from(Rgba::try_from("#B16286").expect("valid color")),
            Hsla::from(Rgba::try_from("#689D6A").expect("valid color")),
            Hsla::from(Rgba::try_from("#D65D0E").expect("valid color")),
        ];
        let background = Hsla::from(Rgba::try_from("#FBF1C7").expect("valid color"));

        let analysis = analyze_bracket_colorization_palette(
            &accents,
            Appearance::Light,
            background,
            BracketColorizationMode::Auto,
        );

        assert_eq!(
            analysis.strategy,
            BracketColorizationPaletteStrategy::PreservedWeakPalette
        );
        assert!(!analysis.changed);
        assert_eq!(analysis.final_palette.as_ref(), accents.as_slice());
        assert!(analysis.original_score.min_adjacent_distance < 0.10);
        assert!(analysis.original_score.min_adjacent_distance >= 0.095);
    }

    #[test]
    fn test_adjust_color_for_background_prefers_closest_passing_candidate() {
        let config = AutoBracketColorizationConfig::default();
        let backgrounds = [
            hsla(0.0, 0.0, 0.15, 1.0),
            hsla(0.0, 0.0, 0.30, 1.0),
            hsla(0.0, 0.0, 0.50, 1.0),
            hsla(0.0, 0.0, 0.70, 1.0),
            hsla(0.0, 0.0, 0.85, 1.0),
        ];
        let minimum_background_contrasts = [10.0, 15.0, 20.0, 25.0, 30.0, 35.0];

        for background in backgrounds {
            let away_from_background_is_lighter = background.l <= 0.5;
            for minimum_background_contrast in minimum_background_contrasts {
                for hue in [0.0, 0.08, 0.16, 0.33, 0.58, 0.75] {
                    for saturation in [0.4, 0.7, 1.0] {
                        for lightness in [0.22, 0.30, 0.38, 0.46, 0.54, 0.62, 0.70] {
                            let color = hsla(hue, saturation, lightness, 1.0);
                            if background_contrast(color, background) >= minimum_background_contrast
                            {
                                continue;
                            }

                            let original = color_to_oklab(color);
                            let darker_candidate = adjusted_lightness_candidate(
                                color,
                                background,
                                minimum_background_contrast,
                                config.lightness_clamp_min,
                            );
                            let lighter_candidate = adjusted_lightness_candidate(
                                color,
                                background,
                                minimum_background_contrast,
                                config.lightness_clamp_max,
                            );
                            let (Some(darker_candidate), Some(lighter_candidate)) =
                                (darker_candidate, lighter_candidate)
                            else {
                                continue;
                            };

                            let darker_distance =
                                perceptual_distance(original, color_to_oklab(darker_candidate));
                            let lighter_distance =
                                perceptual_distance(original, color_to_oklab(lighter_candidate));
                            let closest_candidate = if darker_distance <= lighter_distance {
                                darker_candidate
                            } else {
                                lighter_candidate
                            };
                            let away_from_background_candidate = if away_from_background_is_lighter
                            {
                                lighter_candidate
                            } else {
                                darker_candidate
                            };

                            if closest_candidate == away_from_background_candidate {
                                continue;
                            }

                            assert_eq!(
                                adjust_color_for_background(
                                    color,
                                    background,
                                    minimum_background_contrast,
                                    config,
                                ),
                                closest_candidate
                            );
                            return;
                        }
                    }
                }
            }
        }

        panic!(
            "expected to find a color where the closest passing candidate differs from the away-from-background candidate"
        );
    }

    #[test]
    fn test_auto_bracket_colorization_rescues_severe_background_failure() {
        let color = hsla(0.22, 0.9, 0.76, 1.0);
        let minimum_background_contrast = 35.0;
        let darker_candidate = adjusted_lightness_candidate(
            color,
            light_editor_background(),
            minimum_background_contrast,
            AutoBracketColorizationConfig::default().lightness_clamp_min,
        );
        let lighter_candidate = adjusted_lightness_candidate(
            color,
            light_editor_background(),
            minimum_background_contrast,
            AutoBracketColorizationConfig::default().lightness_clamp_max,
        );
        let analysis = analyze_bracket_colorization_palette(
            &[color],
            Appearance::Light,
            light_editor_background(),
            BracketColorizationMode::Auto,
        );
        let original_contrast = background_contrast(color, light_editor_background());

        assert!(original_contrast < 20.0);
        assert_eq!(
            background_failure_severity(
                original_contrast,
                minimum_background_contrast,
                darker_candidate.is_some() || lighter_candidate.is_some(),
            ),
            Some(BackgroundFailureSeverity::SevereFail)
        );
        assert!(analysis.changed);
        assert!(
            analysis.final_score.min_background_contrast
                >= analysis.background_threshold.unwrap_or_default()
        );
    }

    #[test]
    fn test_adjust_color_for_background_returns_original_when_clamped_out() {
        let color = hsla(0.58, 1.0, 0.47, 1.0);
        let background = hsla(0.0, 0.0, 0.50, 1.0);
        let minimum_background_contrast = 200.0;
        let config = AutoBracketColorizationConfig::default();
        let darker_candidate = adjusted_lightness_candidate(
            color,
            background,
            minimum_background_contrast,
            config.lightness_clamp_min,
        );
        let lighter_candidate = adjusted_lightness_candidate(
            color,
            background,
            minimum_background_contrast,
            config.lightness_clamp_max,
        );

        assert_eq!(
            background_failure_severity(
                background_contrast(color, background),
                minimum_background_contrast,
                darker_candidate.is_some() || lighter_candidate.is_some(),
            ),
            Some(BackgroundFailureSeverity::SevereFail)
        );
        assert_eq!(
            adjust_color_for_background(color, background, minimum_background_contrast, config),
            color
        );
    }

    #[gpui::test]
    async fn test_basic_bracket_colorization(cx: &mut gpui::TestAppContext) {
        init_test(cx, |language_settings| {
            language_settings.defaults.colorize_brackets = Some(true);
        });
        let mut cx = EditorLspTestContext::new(
            Arc::into_inner(rust_lang()).unwrap(),
            lsp::ServerCapabilities::default(),
            cx,
        )
        .await;

        cx.set_state(indoc! {r#"ˇuse std::{collections::HashMap, future::Future};

fn main() {
    let a = one((), { () }, ());
    println!("{a}");
    println!("{a}");
    for i in 0..a {
        println!("{i}");
    }

    let b = {
        {
            {
                [([([([([([([([([([((), ())])])])])])])])])])]
            }
        }
    };
}

#[rustfmt::skip]
fn one(a: (), (): (), c: ()) -> usize { 1 }

fn two<T>(a: HashMap<String, Vec<Option<T>>>) -> usize
where
    T: Future<Output = HashMap<String, Vec<Option<Box<()>>>>>,
{
    2
}
"#});
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();

        assert_eq!(
            r#"use std::«1{collections::HashMap, future::Future}1»;

fn main«1()1» «1{
    let a = one«2(«3()3», «3{ «4()4» }3», «3()3»)2»;
    println!«2("{a}")2»;
    println!«2("{a}")2»;
    for i in 0..a «2{
        println!«3("{i}")3»;
    }2»

    let b = «2{
        «3{
            «4{
                «5[«6(«7[«1(«2[«3(«4[«5(«6[«7(«1[«2(«3[«4(«5[«6(«7[«1(«2[«3(«4()4», «4()4»)3»]2»)1»]7»)6»]5»)4»]3»)2»]1»)7»]6»)5»]4»)3»]2»)1»]7»)6»]5»
            }4»
        }3»
    }2»;
}1»

#«1[rustfmt::skip]1»
fn one«1(a: «2()2», «2()2»: «2()2», c: «2()2»)1» -> usize «1{ 1 }1»

fn two«1<T>1»«1(a: HashMap«2<String, Vec«3<Option«4<T>4»>3»>2»)1» -> usize
where
    T: Future«1<Output = HashMap«2<String, Vec«3<Option«4<Box«5<«6()6»>5»>4»>3»>2»>1»,
«1{
    2
}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
3 hsla(286.00, 51.00%, 64.00%, 1.00)
4 hsla(187.00, 47.00%, 55.00%, 1.00)
5 hsla(355.00, 65.00%, 65.00%, 1.00)
6 hsla(95.00, 38.00%, 62.00%, 1.00)
7 hsla(39.00, 67.00%, 69.00%, 1.00)
"#,
            &bracket_colors_markup(&mut cx),
            "All brackets should be colored based on their depth"
        );
    }

    #[gpui::test]
    async fn test_file_less_file_colorization(cx: &mut gpui::TestAppContext) {
        init_test(cx, |language_settings| {
            language_settings.defaults.colorize_brackets = Some(true);
        });
        let editor = cx.add_window(|window, cx| {
            let multi_buffer = MultiBuffer::build_simple("fn main() {}", cx);
            multi_buffer.update(cx, |multi_buffer, cx| {
                multi_buffer
                    .as_singleton()
                    .unwrap()
                    .update(cx, |buffer, cx| {
                        buffer.set_language(Some(rust_lang()), cx);
                    });
            });
            Editor::new(EditorMode::full(), multi_buffer, None, window, cx)
        });

        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();

        assert_eq!(
            "fn main«1()1» «1{}1»
1 hsla(207.80, 81.00%, 66.00%, 1.00)
",
            editor
                .update(cx, |editor, window, cx| {
                    editor_bracket_colors_markup(&editor.snapshot(window, cx))
                })
                .unwrap(),
            "File-less buffer should still have its brackets colorized"
        );
    }

    #[gpui::test]
    async fn test_markdown_bracket_colorization(cx: &mut gpui::TestAppContext) {
        init_test(cx, |language_settings| {
            language_settings.defaults.colorize_brackets = Some(true);
        });
        let mut cx = EditorLspTestContext::new(
            Arc::into_inner(markdown_lang()).unwrap(),
            lsp::ServerCapabilities::default(),
            cx,
        )
        .await;

        cx.set_state(indoc! {r#"ˇ[LLM-powered features](./ai/overview.md), [bring and configure your own API keys](./ai/llm-providers.md#use-your-own-keys)"#});
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();

        assert_eq!(
            r#"«1[LLM-powered features]1»«1(./ai/overview.md)1», «1[bring and configure your own API keys]1»«1(./ai/llm-providers.md#use-your-own-keys)1»
1 hsla(207.80, 81.00%, 66.00%, 1.00)
"#,
            &bracket_colors_markup(&mut cx),
            "All markdown brackets should be colored based on their depth"
        );

        cx.set_state(indoc! {r#"ˇ{{}}"#});
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();

        assert_eq!(
            r#"«1{«2{}2»}1»
1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
"#,
            &bracket_colors_markup(&mut cx),
            "All markdown brackets should be colored based on their depth, again"
        );
    }

    #[gpui::test]
    async fn test_markdown_brackets_in_multiple_hunks(cx: &mut gpui::TestAppContext) {
        init_test(cx, |language_settings| {
            language_settings.defaults.colorize_brackets = Some(true);
        });
        let mut cx = EditorLspTestContext::new(
            Arc::into_inner(markdown_lang()).unwrap(),
            lsp::ServerCapabilities::default(),
            cx,
        )
        .await;

        let rows = 100;
        let footer = "1 hsla(207.80, 81.00%, 66.00%, 1.00)\n";

        let simple_brackets = (0..rows).map(|_| "ˇ[]\n").collect::<String>();
        let simple_brackets_highlights = (0..rows).map(|_| "«1[]1»\n").collect::<String>();
        cx.set_state(&simple_brackets);
        cx.update_editor(|editor, window, cx| {
            editor.move_to_end(&MoveToEnd, window, cx);
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();
        assert_eq!(
            format!("{simple_brackets_highlights}\n{footer}"),
            bracket_colors_markup(&mut cx),
            "Simple bracket pairs should be colored"
        );

        let paired_brackets = (0..rows).map(|_| "ˇ[]()\n").collect::<String>();
        let paired_brackets_highlights = (0..rows).map(|_| "«1[]1»«1()1»\n").collect::<String>();
        cx.set_state(&paired_brackets);
        // Wait for reparse to complete after content change
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();
        cx.update_editor(|editor, _, cx| {
            // Force invalidation of bracket cache after reparse
            editor.colorize_brackets(true, cx);
        });
        // Scroll to beginning to fetch first chunks
        cx.update_editor(|editor, window, cx| {
            editor.move_to_beginning(&MoveToBeginning, window, cx);
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();
        // Scroll to end to fetch remaining chunks
        cx.update_editor(|editor, window, cx| {
            editor.move_to_end(&MoveToEnd, window, cx);
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();
        assert_eq!(
            format!("{paired_brackets_highlights}\n{footer}"),
            bracket_colors_markup(&mut cx),
            "Paired bracket pairs should be colored"
        );
    }

    #[gpui::test]
    async fn test_bracket_colorization_after_language_swap(cx: &mut gpui::TestAppContext) {
        init_test(cx, |language_settings| {
            language_settings.defaults.colorize_brackets = Some(true);
        });

        let language_registry = Arc::new(language::LanguageRegistry::test(cx.executor()));
        language_registry.add(markdown_lang());
        language_registry.add(rust_lang());

        let mut cx = EditorTestContext::new(cx).await;
        cx.update_buffer(|buffer, cx| {
            buffer.set_language_registry(language_registry.clone());
            buffer.set_language(Some(markdown_lang()), cx);
        });

        cx.set_state(indoc! {r#"
            fn main() {
                let v: Vec<Stringˇ> = vec![];
            }
        "#});
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();

        assert_eq!(
            r#"fn main«1()1» «1{
    let v: Vec<String> = vec!«2[]2»;
}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
"#,
            &bracket_colors_markup(&mut cx),
            "Markdown does not colorize <> brackets"
        );

        cx.update_buffer(|buffer, cx| {
            buffer.set_language(Some(rust_lang()), cx);
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();

        assert_eq!(
            r#"fn main«1()1» «1{
    let v: Vec«2<String>2» = vec!«2[]2»;
}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
"#,
            &bracket_colors_markup(&mut cx),
            "After switching to Rust, <> brackets are now colorized"
        );
    }

    #[gpui::test]
    async fn test_bracket_colorization_when_editing(cx: &mut gpui::TestAppContext) {
        init_test(cx, |language_settings| {
            language_settings.defaults.colorize_brackets = Some(true);
        });
        let mut cx = EditorLspTestContext::new(
            Arc::into_inner(rust_lang()).unwrap(),
            lsp::ServerCapabilities::default(),
            cx,
        )
        .await;

        cx.set_state(indoc! {r#"
struct Foo<'a, T> {
    data: Vec<Option<&'a T>>,
}

fn process_data() {
    let map:ˇ
}
"#});

        cx.update_editor(|editor, window, cx| {
            editor.handle_input(" Result<", window, cx);
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();
        assert_eq!(
            indoc! {r#"
struct Foo«1<'a, T>1» «1{
    data: Vec«2<Option«3<&'a T>3»>2»,
}1»

fn process_data«1()1» «1{
    let map: Result<
}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
3 hsla(286.00, 51.00%, 64.00%, 1.00)
"#},
            &bracket_colors_markup(&mut cx),
            "Brackets without pairs should be ignored and not colored"
        );

        cx.update_editor(|editor, window, cx| {
            editor.handle_input("Option<Foo<'_, ()", window, cx);
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();
        assert_eq!(
            indoc! {r#"
struct Foo«1<'a, T>1» «1{
    data: Vec«2<Option«3<&'a T>3»>2»,
}1»

fn process_data«1()1» «1{
    let map: Result<Option<Foo<'_, «2()2»
}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
3 hsla(286.00, 51.00%, 64.00%, 1.00)
"#},
            &bracket_colors_markup(&mut cx),
        );

        cx.update_editor(|editor, window, cx| {
            editor.handle_input(">", window, cx);
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();
        assert_eq!(
            indoc! {r#"
struct Foo«1<'a, T>1» «1{
    data: Vec«2<Option«3<&'a T>3»>2»,
}1»

fn process_data«1()1» «1{
    let map: Result<Option<Foo«2<'_, «3()3»>2»
}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
3 hsla(286.00, 51.00%, 64.00%, 1.00)
"#},
            &bracket_colors_markup(&mut cx),
            "When brackets start to get closed, inner brackets are re-colored based on their depth"
        );

        cx.update_editor(|editor, window, cx| {
            editor.handle_input(">", window, cx);
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();
        assert_eq!(
            indoc! {r#"
struct Foo«1<'a, T>1» «1{
    data: Vec«2<Option«3<&'a T>3»>2»,
}1»

fn process_data«1()1» «1{
    let map: Result<Option«2<Foo«3<'_, «4()4»>3»>2»
}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
3 hsla(286.00, 51.00%, 64.00%, 1.00)
4 hsla(187.00, 47.00%, 55.00%, 1.00)
"#},
            &bracket_colors_markup(&mut cx),
        );

        cx.update_editor(|editor, window, cx| {
            editor.handle_input(", ()> = unimplemented!();", window, cx);
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();
        assert_eq!(
            indoc! {r#"
struct Foo«1<'a, T>1» «1{
    data: Vec«2<Option«3<&'a T>3»>2»,
}1»

fn process_data«1()1» «1{
    let map: Result«2<Option«3<Foo«4<'_, «5()5»>4»>3», «3()3»>2» = unimplemented!«2()2»;
}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
3 hsla(286.00, 51.00%, 64.00%, 1.00)
4 hsla(187.00, 47.00%, 55.00%, 1.00)
5 hsla(355.00, 65.00%, 65.00%, 1.00)
"#},
            &bracket_colors_markup(&mut cx),
        );
    }

    #[gpui::test]
    async fn test_bracket_colorization_chunks(cx: &mut gpui::TestAppContext) {
        let comment_lines = 100;

        init_test(cx, |language_settings| {
            language_settings.defaults.colorize_brackets = Some(true);
        });
        let mut cx = EditorLspTestContext::new(
            Arc::into_inner(rust_lang()).unwrap(),
            lsp::ServerCapabilities::default(),
            cx,
        )
        .await;

        cx.set_state(&separate_with_comment_lines(
            indoc! {r#"
mod foo {
    ˇfn process_data_1() {
        let map: Option<Vec<()>> = None;
    }
"#},
            indoc! {r#"
    fn process_data_2() {
        let map: Option<Vec<()>> = None;
    }
}
"#},
            comment_lines,
        ));

        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();
        assert_eq!(
            &separate_with_comment_lines(
                indoc! {r#"
mod foo «1{
    fn process_data_1«2()2» «2{
        let map: Option«3<Vec«4<«5()5»>4»>3» = None;
    }2»
"#},
                indoc! {r#"
    fn process_data_2() {
        let map: Option<Vec<()>> = None;
    }
}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
3 hsla(286.00, 51.00%, 64.00%, 1.00)
4 hsla(187.00, 47.00%, 55.00%, 1.00)
5 hsla(355.00, 65.00%, 65.00%, 1.00)
"#},
                comment_lines,
            ),
            &bracket_colors_markup(&mut cx),
            "First, the only visible chunk is getting the bracket highlights"
        );

        cx.update_editor(|editor, window, cx| {
            editor.move_to_end(&MoveToEnd, window, cx);
            editor.move_up(&MoveUp, window, cx);
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();
        assert_eq!(
            &separate_with_comment_lines(
                indoc! {r#"
mod foo «1{
    fn process_data_1«2()2» «2{
        let map: Option«3<Vec«4<«5()5»>4»>3» = None;
    }2»
"#},
                indoc! {r#"
    fn process_data_2«2()2» «2{
        let map: Option«3<Vec«4<«5()5»>4»>3» = None;
    }2»
}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
3 hsla(286.00, 51.00%, 64.00%, 1.00)
4 hsla(187.00, 47.00%, 55.00%, 1.00)
5 hsla(355.00, 65.00%, 65.00%, 1.00)
"#},
                comment_lines,
            ),
            &bracket_colors_markup(&mut cx),
            "After scrolling to the bottom, both chunks should have the highlights"
        );

        cx.update_editor(|editor, window, cx| {
            editor.handle_input("{{}}}", window, cx);
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();
        assert_eq!(
            &separate_with_comment_lines(
                indoc! {r#"
mod foo «1{
    fn process_data_1() {
        let map: Option<Vec<()>> = None;
    }
"#},
                indoc! {r#"
    fn process_data_2«2()2» «2{
        let map: Option«3<Vec«4<«5()5»>4»>3» = None;
    }
    «3{«4{}4»}3»}2»}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
3 hsla(286.00, 51.00%, 64.00%, 1.00)
4 hsla(187.00, 47.00%, 55.00%, 1.00)
5 hsla(355.00, 65.00%, 65.00%, 1.00)
"#},
                comment_lines,
            ),
            &bracket_colors_markup(&mut cx),
            "First chunk's brackets are invalidated after an edit, and only 2nd (visible) chunk is re-colorized"
        );

        cx.update_editor(|editor, window, cx| {
            editor.move_to_beginning(&MoveToBeginning, window, cx);
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();
        assert_eq!(
            &separate_with_comment_lines(
                indoc! {r#"
mod foo «1{
    fn process_data_1«2()2» «2{
        let map: Option«3<Vec«4<«5()5»>4»>3» = None;
    }2»
"#},
                indoc! {r#"
    fn process_data_2«2()2» «2{
        let map: Option«3<Vec«4<«5()5»>4»>3» = None;
    }
    «3{«4{}4»}3»}2»}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
3 hsla(286.00, 51.00%, 64.00%, 1.00)
4 hsla(187.00, 47.00%, 55.00%, 1.00)
5 hsla(355.00, 65.00%, 65.00%, 1.00)
"#},
                comment_lines,
            ),
            &bracket_colors_markup(&mut cx),
            "Scrolling back to top should re-colorize all chunks' brackets"
        );

        cx.update(|_, cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.project.all_languages.defaults.colorize_brackets = Some(false);
                });
            });
        });
        cx.executor().run_until_parked();
        assert_eq!(
            &separate_with_comment_lines(
                indoc! {r#"
mod foo {
    fn process_data_1() {
        let map: Option<Vec<()>> = None;
    }
"#},
                r#"    fn process_data_2() {
        let map: Option<Vec<()>> = None;
    }
    {{}}}}

"#,
                comment_lines,
            ),
            &bracket_colors_markup(&mut cx),
            "Turning bracket colorization off should remove all bracket colors"
        );

        cx.update(|_, cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.project.all_languages.defaults.colorize_brackets = Some(true);
                });
            });
        });
        cx.executor().run_until_parked();
        assert_eq!(
            &separate_with_comment_lines(
                indoc! {r#"
mod foo «1{
    fn process_data_1«2()2» «2{
        let map: Option«3<Vec«4<«5()5»>4»>3» = None;
    }2»
"#},
                r#"    fn process_data_2() {
        let map: Option<Vec<()>> = None;
    }
    {{}}}}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
3 hsla(286.00, 51.00%, 64.00%, 1.00)
4 hsla(187.00, 47.00%, 55.00%, 1.00)
5 hsla(355.00, 65.00%, 65.00%, 1.00)
"#,
                comment_lines,
            ),
            &bracket_colors_markup(&mut cx),
            "Turning bracket colorization back on refreshes the visible excerpts' bracket colors"
        );
    }

    #[gpui::test]
    async fn test_rainbow_bracket_highlights(cx: &mut gpui::TestAppContext) {
        init_test(cx, |language_settings| {
            language_settings.defaults.colorize_brackets = Some(true);
        });
        let mut cx = EditorLspTestContext::new(
            Arc::into_inner(rust_lang()).unwrap(),
            lsp::ServerCapabilities::default(),
            cx,
        )
        .await;

        // taken from r-a https://github.com/rust-lang/rust-analyzer/blob/d733c07552a2dc0ec0cc8f4df3f0ca969a93fd90/crates/ide/src/inlay_hints.rs#L81-L297
        cx.set_state(indoc! {r#"ˇ
            pub(crate) fn inlay_hints(
                db: &RootDatabase,
                file_id: FileId,
                range_limit: Option<TextRange>,
                config: &InlayHintsConfig,
            ) -> Vec<InlayHint> {
                let _p = tracing::info_span!("inlay_hints").entered();
                let sema = Semantics::new(db);
                let file_id = sema
                    .attach_first_edition(file_id)
                    .unwrap_or_else(|| EditionedFileId::current_edition(db, file_id));
                let file = sema.parse(file_id);
                let file = file.syntax();

                let mut acc = Vec::new();

                let Some(scope) = sema.scope(file) else {
                    return acc;
                };
                let famous_defs = FamousDefs(&sema, scope.krate());
                let display_target = famous_defs.1.to_display_target(sema.db);

                let ctx = &mut InlayHintCtx::default();
                let mut hints = |event| {
                    if let Some(node) = handle_event(ctx, event) {
                        hints(&mut acc, ctx, &famous_defs, config, file_id, display_target, node);
                    }
                };
                let mut preorder = file.preorder();
                salsa::attach(sema.db, || {
                    while let Some(event) = preorder.next() {
                        if matches!((&event, range_limit), (WalkEvent::Enter(node), Some(range)) if range.intersect(node.text_range()).is_none())
                        {
                            preorder.skip_subtree();
                            continue;
                        }
                        hints(event);
                    }
                });
                if let Some(range_limit) = range_limit {
                    acc.retain(|hint| range_limit.contains_range(hint.range));
                }
                acc
            }

            #[derive(Default)]
            struct InlayHintCtx {
                lifetime_stacks: Vec<Vec<SmolStr>>,
                extern_block_parent: Option<ast::ExternBlock>,
            }

            pub(crate) fn inlay_hints_resolve(
                db: &RootDatabase,
                file_id: FileId,
                resolve_range: TextRange,
                hash: u64,
                config: &InlayHintsConfig,
                hasher: impl Fn(&InlayHint) -> u64,
            ) -> Option<InlayHint> {
                let _p = tracing::info_span!("inlay_hints_resolve").entered();
                let sema = Semantics::new(db);
                let file_id = sema
                    .attach_first_edition(file_id)
                    .unwrap_or_else(|| EditionedFileId::current_edition(db, file_id));
                let file = sema.parse(file_id);
                let file = file.syntax();

                let scope = sema.scope(file)?;
                let famous_defs = FamousDefs(&sema, scope.krate());
                let mut acc = Vec::new();

                let display_target = famous_defs.1.to_display_target(sema.db);

                let ctx = &mut InlayHintCtx::default();
                let mut hints = |event| {
                    if let Some(node) = handle_event(ctx, event) {
                        hints(&mut acc, ctx, &famous_defs, config, file_id, display_target, node);
                    }
                };

                let mut preorder = file.preorder();
                while let Some(event) = preorder.next() {
                    // This can miss some hints that require the parent of the range to calculate
                    if matches!(&event, WalkEvent::Enter(node) if resolve_range.intersect(node.text_range()).is_none())
                    {
                        preorder.skip_subtree();
                        continue;
                    }
                    hints(event);
                }
                acc.into_iter().find(|hint| hasher(hint) == hash)
            }

            fn handle_event(ctx: &mut InlayHintCtx, node: WalkEvent<SyntaxNode>) -> Option<SyntaxNode> {
                match node {
                    WalkEvent::Enter(node) => {
                        if let Some(node) = ast::AnyHasGenericParams::cast(node.clone()) {
                            let params = node
                                .generic_param_list()
                                .map(|it| {
                                    it.lifetime_params()
                                        .filter_map(|it| {
                                            it.lifetime().map(|it| format_smolstr!("{}", &it.text()[1..]))
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            ctx.lifetime_stacks.push(params);
                        }
                        if let Some(node) = ast::ExternBlock::cast(node.clone()) {
                            ctx.extern_block_parent = Some(node);
                        }
                        Some(node)
                    }
                    WalkEvent::Leave(n) => {
                        if ast::AnyHasGenericParams::can_cast(n.kind()) {
                            ctx.lifetime_stacks.pop();
                        }
                        if ast::ExternBlock::can_cast(n.kind()) {
                            ctx.extern_block_parent = None;
                        }
                        None
                    }
                }
            }

            // At some point when our hir infra is fleshed out enough we should flip this and traverse the
            // HIR instead of the syntax tree.
            fn hints(
                hints: &mut Vec<InlayHint>,
                ctx: &mut InlayHintCtx,
                famous_defs @ FamousDefs(sema, _krate): &FamousDefs<'_, '_>,
                config: &InlayHintsConfig,
                file_id: EditionedFileId,
                display_target: DisplayTarget,
                node: SyntaxNode,
            ) {
                closing_brace::hints(
                    hints,
                    sema,
                    config,
                    display_target,
                    InRealFile { file_id, value: node.clone() },
                );
                if let Some(any_has_generic_args) = ast::AnyHasGenericArgs::cast(node.clone()) {
                    generic_param::hints(hints, famous_defs, config, any_has_generic_args);
                }

                match_ast! {
                    match node {
                        ast::Expr(expr) => {
                            chaining::hints(hints, famous_defs, config, display_target, &expr);
                            adjustment::hints(hints, famous_defs, config, display_target, &expr);
                            match expr {
                                ast::Expr::CallExpr(it) => param_name::hints(hints, famous_defs, config, file_id, ast::Expr::from(it)),
                                ast::Expr::MethodCallExpr(it) => {
                                    param_name::hints(hints, famous_defs, config, file_id, ast::Expr::from(it))
                                }
                                ast::Expr::ClosureExpr(it) => {
                                    closure_captures::hints(hints, famous_defs, config, it.clone());
                                    closure_ret::hints(hints, famous_defs, config, display_target, it)
                                },
                                ast::Expr::RangeExpr(it) => range_exclusive::hints(hints, famous_defs, config, it),
                                _ => Some(()),
                            }
                        },
                        ast::Pat(it) => {
                            binding_mode::hints(hints, famous_defs, config, &it);
                            match it {
                                ast::Pat::IdentPat(it) => {
                                    bind_pat::hints(hints, famous_defs, config, display_target, &it);
                                }
                                ast::Pat::RangePat(it) => {
                                    range_exclusive::hints(hints, famous_defs, config, it);
                                }
                                _ => {}
                            }
                            Some(())
                        },
                        ast::Item(it) => match it {
                            ast::Item::Fn(it) => {
                                implicit_drop::hints(hints, famous_defs, config, display_target, &it);
                                if let Some(extern_block) = &ctx.extern_block_parent {
                                    extern_block::fn_hints(hints, famous_defs, config, &it, extern_block);
                                }
                                lifetime::fn_hints(hints, ctx, famous_defs, config,  it)
                            },
                            ast::Item::Static(it) => {
                                if let Some(extern_block) = &ctx.extern_block_parent {
                                    extern_block::static_hints(hints, famous_defs, config, &it, extern_block);
                                }
                                implicit_static::hints(hints, famous_defs, config,  Either::Left(it))
                            },
                            ast::Item::Const(it) => implicit_static::hints(hints, famous_defs, config, Either::Right(it)),
                            ast::Item::Enum(it) => discriminant::enum_hints(hints, famous_defs, config, it),
                            ast::Item::ExternBlock(it) => extern_block::extern_block_hints(hints, famous_defs, config, it),
                            _ => None,
                        },
                        // trait object type elisions
                        ast::Type(ty) => match ty {
                            ast::Type::FnPtrType(ptr) => lifetime::fn_ptr_hints(hints, ctx, famous_defs, config,  ptr),
                            ast::Type::PathType(path) => {
                                lifetime::fn_path_hints(hints, ctx, famous_defs, config, &path);
                                implied_dyn_trait::hints(hints, famous_defs, config, Either::Left(path));
                                Some(())
                            },
                            ast::Type::DynTraitType(dyn_) => {
                                implied_dyn_trait::hints(hints, famous_defs, config, Either::Right(dyn_));
                                Some(())
                            },
                            _ => Some(()),
                        },
                        ast::GenericParamList(it) => bounds::hints(hints, famous_defs, config,  it),
                        _ => Some(()),
                    }
                };
            }
        "#});
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();

        let actual_ranges = cx.update_editor(|editor, window, cx| {
            editor
                .snapshot(window, cx)
                .all_text_highlight_ranges(&|key| matches!(key, HighlightKey::ColorizeBracket(_)))
        });

        let mut highlighted_brackets = HashMap::default();
        for (color, range) in actual_ranges.iter().cloned() {
            highlighted_brackets.insert(range, color);
        }

        let last_bracket = actual_ranges
            .iter()
            .max_by_key(|(_, p)| p.end.row)
            .unwrap()
            .clone();

        cx.update_editor(|editor, window, cx| {
            let was_scrolled = editor.set_scroll_position(
                gpui::Point::new(0.0, last_bracket.1.end.row as f64 * 2.0),
                window,
                cx,
            );
            assert!(was_scrolled.0);
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();

        let ranges_after_scrolling = cx.update_editor(|editor, window, cx| {
            editor
                .snapshot(window, cx)
                .all_text_highlight_ranges(&|key| matches!(key, HighlightKey::ColorizeBracket(_)))
        });
        let new_last_bracket = ranges_after_scrolling
            .iter()
            .max_by_key(|(_, p)| p.end.row)
            .unwrap()
            .clone();

        assert_ne!(
            last_bracket, new_last_bracket,
            "After scrolling down, we should have highlighted more brackets"
        );

        cx.update_editor(|editor, window, cx| {
            let was_scrolled = editor.set_scroll_position(gpui::Point::default(), window, cx);
            assert!(was_scrolled.0);
        });

        for _ in 0..200 {
            cx.update_editor(|editor, window, cx| {
                editor.apply_scroll_delta(gpui::Point::new(0.0, 0.25), window, cx);
            });
            cx.executor().advance_clock(Duration::from_millis(100));
            cx.executor().run_until_parked();

            let colored_brackets = cx.update_editor(|editor, window, cx| {
                editor
                    .snapshot(window, cx)
                    .all_text_highlight_ranges(&|key| {
                        matches!(key, HighlightKey::ColorizeBracket(_))
                    })
            });
            for (color, range) in colored_brackets.clone() {
                assert!(
                    highlighted_brackets.entry(range).or_insert(color) == &color,
                    "Colors should stay consistent while scrolling!"
                );
            }

            let snapshot = cx.update_editor(|editor, window, cx| editor.snapshot(window, cx));
            let scroll_position = snapshot.scroll_position();
            let visible_lines =
                cx.update_editor(|editor, _, _| editor.visible_line_count().unwrap());
            let visible_range = DisplayRow(scroll_position.y as u32)
                ..DisplayRow((scroll_position.y + visible_lines) as u32);

            let current_highlighted_bracket_set: HashSet<Point> = HashSet::from_iter(
                colored_brackets
                    .iter()
                    .flat_map(|(_, range)| [range.start, range.end]),
            );

            for highlight_range in highlighted_brackets.keys().filter(|bracket_range| {
                visible_range.contains(&bracket_range.start.to_display_point(&snapshot).row())
                    || visible_range.contains(&bracket_range.end.to_display_point(&snapshot).row())
            }) {
                assert!(
                    current_highlighted_bracket_set.contains(&highlight_range.start)
                        || current_highlighted_bracket_set.contains(&highlight_range.end),
                    "Should not lose highlights while scrolling in the visible range!"
                );
            }

            let buffer_snapshot = snapshot.buffer().as_singleton().unwrap().2;
            for bracket_match in buffer_snapshot
                .fetch_bracket_ranges(
                    snapshot
                        .display_point_to_point(
                            DisplayPoint::new(visible_range.start, 0),
                            Bias::Left,
                        )
                        .to_offset(&buffer_snapshot)
                        ..snapshot
                            .display_point_to_point(
                                DisplayPoint::new(
                                    visible_range.end,
                                    snapshot.line_len(visible_range.end),
                                ),
                                Bias::Right,
                            )
                            .to_offset(&buffer_snapshot),
                    None,
                )
                .iter()
                .flat_map(|entry| entry.1)
                .filter(|bracket_match| bracket_match.color_index.is_some())
            {
                let start = bracket_match.open_range.to_point(buffer_snapshot);
                let end = bracket_match.close_range.to_point(buffer_snapshot);
                let start_bracket = colored_brackets.iter().find(|(_, range)| *range == start);
                assert!(
                    start_bracket.is_some(),
                    "Existing bracket start in the visible range should be highlighted. Missing color for match: \"{}\" at position {:?}",
                    buffer_snapshot
                        .text_for_range(start.start..end.end)
                        .collect::<String>(),
                    start
                );

                let end_bracket = colored_brackets.iter().find(|(_, range)| *range == end);
                assert!(
                    end_bracket.is_some(),
                    "Existing bracket end in the visible range should be highlighted. Missing color for match: \"{}\" at position {:?}",
                    buffer_snapshot
                        .text_for_range(start.start..end.end)
                        .collect::<String>(),
                    start
                );

                assert_eq!(
                    start_bracket.unwrap().0,
                    end_bracket.unwrap().0,
                    "Bracket pair should be highlighted the same color!"
                )
            }
        }
    }

    #[gpui::test]
    async fn test_multi_buffer(cx: &mut gpui::TestAppContext) {
        let comment_lines = 100;

        init_test(cx, |language_settings| {
            language_settings.defaults.colorize_brackets = Some(true);
        });
        let fs = FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(
            path!("/a"),
            json!({
                "main.rs": "fn main() {{()}}",
                "lib.rs": separate_with_comment_lines(
                    indoc! {r#"
    mod foo {
        fn process_data_1() {
            let map: Option<Vec<()>> = None;
            // a
            // b
            // c
        }
    "#},
                    indoc! {r#"
        fn process_data_2() {
            let other_map: Option<Vec<()>> = None;
        }
    }
    "#},
                    comment_lines,
                )
            }),
        )
        .await;

        let project = Project::test(fs, [path!("/a").as_ref()], cx).await;
        let language_registry = project.read_with(cx, |project, _| project.languages().clone());
        language_registry.add(rust_lang());

        let buffer_1 = project
            .update(cx, |project, cx| {
                project.open_local_buffer(path!("/a/lib.rs"), cx)
            })
            .await
            .unwrap();
        let buffer_2 = project
            .update(cx, |project, cx| {
                project.open_local_buffer(path!("/a/main.rs"), cx)
            })
            .await
            .unwrap();

        let multi_buffer = cx.new(|cx| {
            let mut multi_buffer = MultiBuffer::new(Capability::ReadWrite);
            multi_buffer.set_excerpts_for_path(
                PathKey::sorted(0),
                buffer_2.clone(),
                [Point::new(0, 0)..Point::new(1, 0)],
                0,
                cx,
            );

            let excerpt_rows = 5;
            let rest_of_first_except_rows = 3;
            multi_buffer.set_excerpts_for_path(
                PathKey::sorted(1),
                buffer_1.clone(),
                [
                    Point::new(0, 0)..Point::new(excerpt_rows, 0),
                    Point::new(
                        comment_lines as u32 + excerpt_rows + rest_of_first_except_rows,
                        0,
                    )
                        ..Point::new(
                            comment_lines as u32
                                + excerpt_rows
                                + rest_of_first_except_rows
                                + excerpt_rows,
                            0,
                        ),
                ],
                0,
                cx,
            );
            multi_buffer
        });

        let editor = cx.add_window(|window, cx| {
            Editor::for_multibuffer(multi_buffer, Some(project.clone()), window, cx)
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();

        let editor_snapshot = editor
            .update(cx, |editor, window, cx| editor.snapshot(window, cx))
            .unwrap();
        assert_eq!(
            indoc! {r#"


fn main«1()1» «1{«2{«3()3»}2»}1»


mod foo «1{
    fn process_data_1«2()2» «2{
        let map: Option«3<Vec«4<«5()5»>4»>3» = None;
        // a
        // b
        // c

    fn process_data_2«2()2» «2{
        let other_map: Option«3<Vec«4<«5()5»>4»>3» = None;
    }2»
}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
3 hsla(286.00, 51.00%, 64.00%, 1.00)
4 hsla(187.00, 47.00%, 55.00%, 1.00)
5 hsla(355.00, 65.00%, 65.00%, 1.00)
"#,},
            &editor_bracket_colors_markup(&editor_snapshot),
            "Multi buffers should have their brackets colored even if no excerpts contain the bracket counterpart (after fn `process_data_2()`) \
or if the buffer pair spans across multiple excerpts (the one after `mod foo`)"
        );

        editor
            .update(cx, |editor, window, cx| {
                editor.handle_input("{[]", window, cx);
            })
            .unwrap();
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();
        let editor_snapshot = editor
            .update(cx, |editor, window, cx| editor.snapshot(window, cx))
            .unwrap();
        assert_eq!(
            indoc! {r#"


{«1[]1»fn main«1()1» «1{«2{«3()3»}2»}1»


mod foo «1{
    fn process_data_1«2()2» «2{
        let map: Option«3<Vec«4<«5()5»>4»>3» = None;
        // a
        // b
        // c

    fn process_data_2«2()2» «2{
        let other_map: Option«3<Vec«4<«5()5»>4»>3» = None;
    }2»
}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
3 hsla(286.00, 51.00%, 64.00%, 1.00)
4 hsla(187.00, 47.00%, 55.00%, 1.00)
5 hsla(355.00, 65.00%, 65.00%, 1.00)
"#,},
            &editor_bracket_colors_markup(&editor_snapshot),
        );

        cx.update(|cx| {
            let theme = cx.theme().name.clone();
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.theme.theme_overrides = HashMap::from_iter([(
                        theme.to_string(),
                        ThemeStyleContent {
                            accents: vec![
                                AccentContent(Some("#ff0000".to_string())),
                                AccentContent(Some("#0000ff".to_string())),
                            ],
                            ..ThemeStyleContent::default()
                        },
                    )]);
                });
            });
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();
        let editor_snapshot = editor
            .update(cx, |editor, window, cx| editor.snapshot(window, cx))
            .unwrap();
        let adjusted_palette = cx.update(|cx| {
            analyze_bracket_colorization_palette(
                &[
                    Hsla::from(Rgba::try_from("#ff0000").expect("valid override accent")),
                    Hsla::from(Rgba::try_from("#0000ff").expect("valid override accent")),
                ],
                cx.theme().appearance,
                cx.theme().colors().editor_background,
                BracketColorizationMode::Auto,
            )
            .final_palette
        });
        let expected_markup = format!(
            "{}\n1 {}\n2 {}\n",
            indoc! {r#"


{«1[]1»fn main«1()1» «1{«2{«1()1»}2»}1»


mod foo «1{
    fn process_data_1«2()2» «2{
        let map: Option«1<Vec«2<«1()1»>2»>1» = None;
        // a
        // b
        // c

    fn process_data_2«2()2» «2{
        let other_map: Option«1<Vec«2<«1()1»>2»>1» = None;
    }2»
}1»
"#,},
            adjusted_palette[0],
            adjusted_palette[1],
        );
        assert_eq!(
            expected_markup,
            editor_bracket_colors_markup(&editor_snapshot),
            "After updating theme accents, the editor should update the bracket coloring"
        );
    }

    #[gpui::test]
    // reproduction of #47846
    async fn test_bracket_colorization_with_folds(cx: &mut gpui::TestAppContext) {
        init_test(cx, |language_settings| {
            language_settings.defaults.colorize_brackets = Some(true);
        });
        let mut cx = EditorLspTestContext::new(
            Arc::into_inner(rust_lang()).unwrap(),
            lsp::ServerCapabilities::default(),
            cx,
        )
        .await;

        // Generate a large function body. When folded, this collapses
        // to a single display line, making small_function visible on screen.
        let mut big_body = String::new();
        for i in 0..700 {
            big_body.push_str(&format!("    let var_{i:04} = ({i});\n"));
        }
        let source = format!(
            "ˇfn big_function() {{\n{big_body}}}\n\nfn small_function() {{\n    let x = (1, (2, 3));\n}}\n"
        );

        cx.set_state(&source);
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();

        cx.update_editor(|editor, window, cx| {
            editor.fold_ranges(
                vec![Point::new(0, 0)..Point::new(701, 1)],
                false,
                window,
                cx,
            );
        });
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.executor().run_until_parked();

        assert_eq!(
            indoc! {r#"
⋯1»

fn small_function«1()1» «1{
    let x = «2(1, «3(2, 3)3»)2»;
}1»

1 hsla(207.80, 81.00%, 66.00%, 1.00)
2 hsla(29.00, 54.00%, 61.00%, 1.00)
3 hsla(286.00, 51.00%, 64.00%, 1.00)
"#,},
            bracket_colors_markup(&mut cx),
        );
    }

    fn separate_with_comment_lines(head: &str, tail: &str, comment_lines: usize) -> String {
        let mut result = head.to_string();
        result.push_str("\n");
        result.push_str(&"//\n".repeat(comment_lines));
        result.push_str(tail);
        result
    }

    fn bracket_colors_markup(cx: &mut EditorTestContext) -> String {
        cx.update_editor(|editor, window, cx| {
            editor_bracket_colors_markup(&editor.snapshot(window, cx))
        })
    }

    fn editor_bracket_colors_markup(snapshot: &EditorSnapshot) -> String {
        fn display_point_to_offset(text: &str, point: DisplayPoint) -> usize {
            let mut offset = 0;
            for (row_idx, line) in text.lines().enumerate() {
                if row_idx < point.row().0 as usize {
                    offset += line.len() + 1; // +1 for newline
                } else {
                    offset += point.column() as usize;
                    break;
                }
            }
            offset
        }

        let actual_ranges = snapshot
            .all_text_highlight_ranges(&|key| matches!(key, HighlightKey::ColorizeBracket(_)));
        let editor_text = snapshot.text();

        let mut next_index = 1;
        let mut color_to_index = HashMap::default();
        let mut annotations = Vec::new();
        for (color, range) in &actual_ranges {
            let color_index = *color_to_index
                .entry(*color)
                .or_insert_with(|| post_inc(&mut next_index));
            let start = snapshot.point_to_display_point(range.start, Bias::Left);
            let end = snapshot.point_to_display_point(range.end, Bias::Right);
            let start_offset = display_point_to_offset(&editor_text, start);
            let end_offset = display_point_to_offset(&editor_text, end);
            let bracket_text = &editor_text[start_offset..end_offset];
            let bracket_char = bracket_text.chars().next().unwrap();

            if matches!(bracket_char, '{' | '[' | '(' | '<') {
                annotations.push((start_offset, format!("«{color_index}")));
            } else {
                annotations.push((end_offset, format!("{color_index}»")));
            }
        }

        annotations.sort_by(|(pos_a, text_a), (pos_b, text_b)| {
            pos_a.cmp(pos_b).reverse().then_with(|| {
                let a_is_opening = text_a.starts_with('«');
                let b_is_opening = text_b.starts_with('«');
                match (a_is_opening, b_is_opening) {
                    (true, false) => cmp::Ordering::Less,
                    (false, true) => cmp::Ordering::Greater,
                    _ => cmp::Ordering::Equal,
                }
            })
        });
        annotations.dedup();

        let mut markup = editor_text;
        for (offset, text) in annotations {
            markup.insert_str(offset, &text);
        }

        markup.push_str("\n");
        for (index, color) in color_to_index
            .iter()
            .map(|(color, index)| (*index, *color))
            .sorted_by_key(|(index, _)| *index)
        {
            markup.push_str(&format!("{index} {color}\n"));
        }

        markup
    }
}
