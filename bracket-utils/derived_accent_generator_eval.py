from __future__ import annotations

import argparse
import json
from collections import Counter, defaultdict
from dataclasses import dataclass
from pathlib import Path
from statistics import mean
from typing import Any, Callable

import json5
from coloraide import Color

from accent_palette_provenance import family_for_key, normalize_rgb_hex, oklab_distance
from analyze_extension_themes import REPOS_DIR
from palette_probe import authored_order
from strategy_sweep import cut_then_move_orders, make_candidate
from theme_pipeline import (
    adjust_color_for_background,
    background_contrast,
    is_valid_color_string,
    target_threshold,
)


TERMINAL_CANONICAL_KEYS = [
    "terminal.ansi.blue",
    "terminal.ansi.green",
    "terminal.ansi.yellow",
    "terminal.ansi.red",
    "terminal.ansi.magenta",
    "terminal.ansi.cyan",
    "terminal.ansi.bright_blue",
    "terminal.ansi.bright_green",
    "terminal.ansi.bright_yellow",
    "terminal.ansi.bright_red",
    "terminal.ansi.bright_magenta",
    "terminal.ansi.bright_cyan",
    "terminal.ansi.white",
]

FAMILY_PRIORITY = {
    "terminal_ansi": 5.0,
    "status": 4.5,
    "players": 4.0,
    "ui_accent": 3.5,
    "syntax": 3.0,
    "vim": 2.5,
    "ui_other": 1.0,
}

PREFERRED_FAMILIES = {
    "terminal_ansi",
    "status",
    "players",
    "syntax",
    "ui_accent",
    "vim",
}

PROVENANCE_KEY_PRIORITY: Counter[str] = Counter()
PROVENANCE_FAMILY_PRIORITY: Counter[str] = Counter()
PROVENANCE_SLOT_KEY_PRIORITY: defaultdict[int, Counter[str]] = defaultdict(Counter)
PROVENANCE_SLOT_FAMILY_PRIORITY: defaultdict[int, Counter[str]] = defaultdict(Counter)
PROVENANCE_SIGNAL_KEYS: set[str] = set()


@dataclass(frozen=True)
class SourceColor:
    family: str
    key: str
    color: Color
    priority: float


@dataclass(frozen=True)
class ThemeCase:
    theme_name: str
    file_path: str
    appearance: str
    explicit_accents: bool
    background: Color
    accents: tuple[Color, ...]
    source_colors: tuple[SourceColor, ...]


@dataclass(frozen=True)
class PaletteCandidate:
    color: Color
    families: tuple[str, ...]
    keys: tuple[str, ...]
    source_count: int
    family_score: float


def iter_theme_json_files() -> list[Path]:
    return sorted(REPOS_DIR.glob("*/themes/*.json"))


def load_theme_file(path: Path) -> object:
    text = path.read_text()
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return json5.loads(text)


def walk_style_colors(value: Any, path: str = "") -> list[SourceColor]:
    source_colors: list[SourceColor] = []
    if isinstance(value, dict):
        for key, nested_value in value.items():
            next_path = f"{path}.{key}" if path else str(key)
            if next_path == "accents":
                continue
            source_colors.extend(walk_style_colors(nested_value, next_path))
        return source_colors

    if isinstance(value, list):
        for index, nested_value in enumerate(value):
            next_path = f"{path}[{index}]"
            source_colors.extend(walk_style_colors(nested_value, next_path))
        return source_colors

    if isinstance(value, str) and value and is_valid_color_string(value):
        family = family_for_key(path)
        source_colors.append(
            SourceColor(
                family=family,
                key=path,
                color=Color(value),
                priority=FAMILY_PRIORITY.get(family, 0.0),
            )
        )
    return source_colors


def build_theme_cases() -> list[ThemeCase]:
    cases: list[ThemeCase] = []
    for path in iter_theme_json_files():
        data = load_theme_file(path)
        if not isinstance(data, dict) or "themes" not in data:
            continue
        for theme in data["themes"]:
            if not isinstance(theme, dict):
                continue
            style = theme.get("style")
            if not isinstance(style, dict):
                continue
            raw_accents = style.get("accents") or []
            accent_values = tuple(
                Color(value)
                for value in raw_accents
                if isinstance(value, str) and value and is_valid_color_string(value)
            )
            if len(accent_values) < 2:
                explicit = False
                accent_values = ()
            else:
                explicit = True
            source_colors = tuple(walk_style_colors(style))
            cases.append(
                ThemeCase(
                    theme_name=str(theme["name"]),
                    file_path=str(path),
                    appearance=str(theme["appearance"]),
                    explicit_accents=explicit,
                    background=Color(style["editor.background"]),
                    accents=accent_values,
                    source_colors=source_colors,
                )
            )
    return cases


def threshold_pick(candidates, threshold):
    passers = [
        candidate
        for candidate in candidates
        if candidate.min_adjacent_distance >= threshold
    ]
    if passers:
        return min(
            passers,
            key=lambda candidate: (
                candidate.displacement_cost,
                candidate.inversion_count,
                -candidate.min_adjacent_distance,
            ),
        )
    return max(
        candidates,
        key=lambda candidate: (
            candidate.min_adjacent_distance,
            -candidate.inversion_count,
            -candidate.displacement_cost,
        ),
    )


def dedupe_source_colors(
    source_colors: list[SourceColor],
    threshold: float = 0.01,
) -> list[SourceColor]:
    exact_seen: set[str] = set()
    deduped: list[SourceColor] = []
    sorted_colors = sorted(
        source_colors,
        key=lambda source_color: (
            -source_color.priority,
            -source_color.color.convert("oklch")["c"],
            -background_contrast(source_color.color, Color("white")),
        ),
    )
    for source_color in sorted_colors:
        rgb = normalize_rgb_hex(source_color.color)
        if rgb in exact_seen:
            continue
        if any(oklab_distance(source_color.color, existing.color) <= threshold for existing in deduped):
            continue
        exact_seen.add(rgb)
        deduped.append(source_color)
    return deduped


def filtered_source_pool(case: ThemeCase) -> list[SourceColor]:
    preferred = [
        source_color
        for source_color in case.source_colors
        if source_color.family in PREFERRED_FAMILIES
        and source_color.color.alpha() >= 0.95
    ]

    vivid = [
        source_color
        for source_color in preferred
        if source_color.color.convert("oklch")["c"] >= 0.03
    ]
    if vivid:
        return dedupe_source_colors(vivid)

    if preferred:
        return dedupe_source_colors(preferred)

    opaque_any = [
        source_color
        for source_color in case.source_colors
        if source_color.color.alpha() >= 0.95
    ]
    return dedupe_source_colors(opaque_any)


def broad_source_pool(case: ThemeCase) -> list[SourceColor]:
    opaque_any = [
        source_color
        for source_color in case.source_colors
        if source_color.color.alpha() >= 0.80
    ]
    return dedupe_source_colors(opaque_any, threshold=0.008)


def provenance_source_pool(case: ThemeCase) -> list[SourceColor]:
    preferred = [
        source_color
        for source_color in case.source_colors
        if source_color.color.alpha() >= 0.95
        and (
            source_color.family in PREFERRED_FAMILIES
            or source_color.key in PROVENANCE_SIGNAL_KEYS
        )
    ]

    vivid = [
        source_color
        for source_color in preferred
        if source_color.color.convert("oklch")["c"] >= 0.03
    ]
    if vivid:
        return dedupe_source_colors(vivid)

    if preferred:
        return dedupe_source_colors(preferred)

    return broad_source_pool(case)


def aggregate_candidates(case: ThemeCase) -> list[PaletteCandidate]:
    return aggregate_candidates_from_pool(filtered_source_pool(case))


def aggregate_candidates_from_pool(pool: list[SourceColor]) -> list[PaletteCandidate]:
    grouped: defaultdict[str, list[SourceColor]] = defaultdict(list)
    for source_color in pool:
        grouped[normalize_rgb_hex(source_color.color)].append(source_color)

    candidates: list[PaletteCandidate] = []
    for _, group in grouped.items():
        families = tuple(sorted({source_color.family for source_color in group}))
        keys = tuple(sorted(source_color.key for source_color in group))
        family_score = sum(FAMILY_PRIORITY.get(family, 0.0) for family in families)
        representative = max(
            group,
            key=lambda source_color: (
                source_color.priority,
                source_color.color.convert("oklch")["c"],
            ),
        )
        candidates.append(
            PaletteCandidate(
                color=representative.color,
                families=families,
                keys=keys,
                source_count=len(group),
                family_score=family_score,
            )
        )
    return candidates


def fill_with_aggregate(
    case: ThemeCase,
    chosen: list[Color],
    target_count: int,
) -> list[Color]:
    if len(chosen) >= target_count:
        return chosen[:target_count]

    existing = {normalize_rgb_hex(color) for color in chosen}
    aggregate = aggregate_candidates_from_pool(broad_source_pool(case))
    for candidate in sorted(
        aggregate,
        key=lambda candidate: (
            -candidate.family_score,
            -candidate.source_count,
            -candidate.color.convert("oklch")["c"],
        ),
    ):
        rgb = normalize_rgb_hex(candidate.color)
        if rgb in existing:
            continue
        chosen.append(candidate.color)
        existing.add(rgb)
        if len(chosen) >= target_count:
            break
    return chosen[:target_count]


def initialize_provenance_priors(cases: list[ThemeCase]) -> None:
    PROVENANCE_KEY_PRIORITY.clear()
    PROVENANCE_FAMILY_PRIORITY.clear()
    PROVENANCE_SLOT_KEY_PRIORITY.clear()
    PROVENANCE_SLOT_FAMILY_PRIORITY.clear()
    PROVENANCE_SIGNAL_KEYS.clear()

    for case in cases:
        if not case.explicit_accents:
            continue

        rgb_index: defaultdict[str, list[SourceColor]] = defaultdict(list)
        for source_color in case.source_colors:
            rgb_index[normalize_rgb_hex(source_color.color)].append(source_color)

        for accent_index, accent in enumerate(case.accents):
            matching_sources = rgb_index.get(normalize_rgb_hex(accent), [])
            if not matching_sources:
                continue

            matching_keys = {source_color.key for source_color in matching_sources}
            matching_families = {source_color.family for source_color in matching_sources}
            for key in matching_keys:
                PROVENANCE_KEY_PRIORITY[key] += 1
                PROVENANCE_SLOT_KEY_PRIORITY[accent_index][key] += 1
            for family in matching_families:
                PROVENANCE_FAMILY_PRIORITY[family] += 1
                PROVENANCE_SLOT_FAMILY_PRIORITY[accent_index][family] += 1

    for key, count in PROVENANCE_KEY_PRIORITY.items():
        if count >= 8:
            PROVENANCE_SIGNAL_KEYS.add(key)


def terminal_canonical_generator(case: ThemeCase, target_count: int) -> list[Color]:
    pool = filtered_source_pool(case)
    keyed = {source_color.key: source_color for source_color in pool}
    chosen: list[SourceColor] = []
    used_rgb: set[str] = set()
    for key in TERMINAL_CANONICAL_KEYS:
        source_color = keyed.get(key)
        if source_color is None:
            continue
        rgb = normalize_rgb_hex(source_color.color)
        if rgb in used_rgb:
            continue
        chosen.append(source_color)
        used_rgb.add(rgb)
        if len(chosen) >= target_count:
            return [source_color.color for source_color in chosen]

    color_list = [source_color.color for source_color in chosen]
    return fill_with_aggregate(case, color_list, target_count)


def ranked_key_canonical_generator(case: ThemeCase, target_count: int) -> list[Color]:
    pool = provenance_source_pool(case)
    chosen: list[Color] = []
    used_rgb: set[str] = set()
    grouped = aggregate_candidates_from_pool(pool)

    for candidate in sorted(
        grouped,
        key=lambda candidate: (
            -sum(PROVENANCE_KEY_PRIORITY[key] for key in candidate.keys),
            -sum(PROVENANCE_FAMILY_PRIORITY[family] for family in candidate.families),
            -candidate.family_score,
            -candidate.source_count,
            -candidate.color.convert("oklch")["c"],
        ),
    ):
        rgb = normalize_rgb_hex(candidate.color)
        if rgb in used_rgb:
            continue
        chosen.append(candidate.color)
        used_rgb.add(rgb)
        if len(chosen) >= target_count:
            return chosen

    return fill_with_aggregate(case, chosen, target_count)


def farthest_selection(
    candidates: list[PaletteCandidate],
    target_count: int,
    background: Color,
) -> list[Color]:
    if not candidates:
        return []

    def seed_score(candidate: PaletteCandidate) -> float:
        oklch = candidate.color.convert("oklch")
        return (
            candidate.family_score * 0.12
            + candidate.source_count * 0.04
            + oklch["c"] * 1.5
            + min(background_contrast(candidate.color, background), 90.0) / 180.0
        )

    chosen: list[PaletteCandidate] = [max(candidates, key=seed_score)]
    remaining = [candidate for candidate in candidates if candidate != chosen[0]]

    while remaining and len(chosen) < target_count:
        def candidate_score(candidate: PaletteCandidate) -> float:
            min_distance = min(
                oklab_distance(candidate.color, chosen_color.color)
                for chosen_color in chosen
            )
            family_penalty = 0.02 * sum(
                len(set(chosen_color.families).intersection(candidate.families))
                for chosen_color in chosen
            )
            return (
                min_distance
                + candidate.family_score * 0.008
                + candidate.source_count * 0.01
                + candidate.color.convert("oklch")["c"] * 0.05
                - family_penalty
            )

        next_candidate = max(remaining, key=candidate_score)
        chosen.append(next_candidate)
        remaining.remove(next_candidate)

    return [candidate.color for candidate in chosen]


def priority_farthest_generator(case: ThemeCase, target_count: int) -> list[Color]:
    chosen = farthest_selection(aggregate_candidates(case), target_count, case.background)
    return fill_with_aggregate(case, chosen, target_count)


def aggregate_priority_generator(case: ThemeCase, target_count: int) -> list[Color]:
    aggregate = sorted(
        aggregate_candidates(case),
        key=lambda candidate: (
            -candidate.family_score,
            -candidate.source_count,
            -candidate.color.convert("oklch")["c"],
        ),
    )
    chosen = [candidate.color for candidate in aggregate[:target_count]]
    return fill_with_aggregate(case, chosen, target_count)


def terminal_seeded_farthest_generator(case: ThemeCase, target_count: int) -> list[Color]:
    pool = filtered_source_pool(case)
    keyed = {source_color.key: source_color for source_color in pool}
    chosen: list[Color] = []
    chosen_families: list[tuple[str, ...]] = []
    used_rgb: set[str] = set()

    for key in TERMINAL_CANONICAL_KEYS[:6]:
        source_color = keyed.get(key)
        if source_color is None:
            continue
        rgb = normalize_rgb_hex(source_color.color)
        if rgb in used_rgb:
            continue
        chosen.append(source_color.color)
        chosen_families.append((source_color.family,))
        used_rgb.add(rgb)
        if len(chosen) >= min(3, target_count):
            break

    aggregate = aggregate_candidates(case)
    remaining = [
        candidate
        for candidate in aggregate
        if normalize_rgb_hex(candidate.color) not in used_rgb
    ]
    if not chosen:
        return farthest_selection(aggregate, target_count, case.background)

    while remaining and len(chosen) < target_count:
        def candidate_score(candidate: PaletteCandidate) -> float:
            min_distance = min(
                oklab_distance(candidate.color, chosen_color)
                for chosen_color in chosen
            )
            family_penalty = 0.02 * sum(
                len(set(existing_families).intersection(candidate.families))
                for existing_families in chosen_families
            )
            return (
                min_distance
                + candidate.family_score * 0.008
                + candidate.source_count * 0.01
                + candidate.color.convert("oklch")["c"] * 0.05
                - family_penalty
            )

        next_candidate = max(remaining, key=candidate_score)
        chosen.append(next_candidate.color)
        chosen_families.append(next_candidate.families)
        remaining.remove(next_candidate)

    return fill_with_aggregate(case, chosen, target_count)


def slotwise_provenance_generator(case: ThemeCase, target_count: int) -> list[Color]:
    aggregate = aggregate_candidates_from_pool(provenance_source_pool(case))
    chosen: list[Color] = []
    used_rgb: set[str] = set()

    for accent_index in range(target_count):
        remaining = [
            candidate
            for candidate in aggregate
            if normalize_rgb_hex(candidate.color) not in used_rgb
        ]
        if not remaining:
            break

        def candidate_score(candidate: PaletteCandidate) -> float:
            key_score = sum(
                PROVENANCE_SLOT_KEY_PRIORITY[accent_index][key]
                for key in candidate.keys
            )
            family_score = sum(
                PROVENANCE_SLOT_FAMILY_PRIORITY[accent_index][family]
                for family in candidate.families
            )
            return (
                key_score * 1.5
                + family_score * 0.75
                + candidate.family_score * 0.2
                + candidate.source_count * 0.15
                + candidate.color.convert("oklch")["c"] * 2.0
            )

        next_candidate = max(remaining, key=candidate_score)
        chosen.append(next_candidate.color)
        used_rgb.add(normalize_rgb_hex(next_candidate.color))

    return fill_with_aggregate(case, chosen, target_count)


def slotwise_provenance_farthest_generator(case: ThemeCase, target_count: int) -> list[Color]:
    aggregate = aggregate_candidates_from_pool(provenance_source_pool(case))
    if not aggregate:
        return []

    chosen: list[PaletteCandidate] = []
    remaining = list(aggregate)

    for accent_index in range(target_count):
        if not remaining:
            break

        def candidate_score(candidate: PaletteCandidate) -> float:
            key_score = sum(
                PROVENANCE_SLOT_KEY_PRIORITY[accent_index][key]
                for key in candidate.keys
            )
            family_score = sum(
                PROVENANCE_SLOT_FAMILY_PRIORITY[accent_index][family]
                for family in candidate.families
            )
            min_distance = (
                min(
                    oklab_distance(candidate.color, existing.color)
                    for existing in chosen
                )
                if chosen
                else 0.0
            )
            return (
                key_score * 1.2
                + family_score * 0.6
                + min_distance * 60.0
                + candidate.family_score * 0.12
                + candidate.source_count * 0.08
                + min(background_contrast(candidate.color, case.background), 90.0) / 40.0
            )

        next_candidate = max(remaining, key=candidate_score)
        chosen.append(next_candidate)
        remaining.remove(next_candidate)

    return fill_with_aggregate(case, [candidate.color for candidate in chosen], target_count)


def slotwise_provenance_strict_generator(case: ThemeCase, target_count: int) -> list[Color]:
    aggregate = aggregate_candidates_from_pool(provenance_source_pool(case))
    chosen: list[Color] = []
    used_rgb: set[str] = set()

    for accent_index in range(target_count):
        remaining = [
            candidate
            for candidate in aggregate
            if normalize_rgb_hex(candidate.color) not in used_rgb
        ]
        if not remaining:
            break

        def candidate_score(candidate: PaletteCandidate) -> float:
            key_score = sum(
                PROVENANCE_SLOT_KEY_PRIORITY[accent_index][key]
                for key in candidate.keys
            )
            family_score = sum(
                PROVENANCE_SLOT_FAMILY_PRIORITY[accent_index][family]
                for family in candidate.families
            )
            global_key_score = sum(PROVENANCE_KEY_PRIORITY[key] for key in candidate.keys)
            global_family_score = sum(
                PROVENANCE_FAMILY_PRIORITY[family] for family in candidate.families
            )
            return (
                key_score * 2.0
                + family_score
                + global_key_score * 0.12
                + global_family_score * 0.06
                + candidate.source_count * 0.05
                + candidate.family_score * 0.08
            )

        next_candidate = max(remaining, key=candidate_score)
        chosen.append(next_candidate.color)
        used_rgb.add(normalize_rgb_hex(next_candidate.color))

    return fill_with_aggregate(case, chosen, target_count)


def slotwise_provenance_balanced_generator(case: ThemeCase, target_count: int) -> list[Color]:
    aggregate = aggregate_candidates_from_pool(provenance_source_pool(case))
    chosen: list[PaletteCandidate] = []
    remaining = list(aggregate)

    for accent_index in range(target_count):
        if not remaining:
            break

        def candidate_score(candidate: PaletteCandidate) -> float:
            key_score = sum(
                PROVENANCE_SLOT_KEY_PRIORITY[accent_index][key]
                for key in candidate.keys
            )
            family_score = sum(
                PROVENANCE_SLOT_FAMILY_PRIORITY[accent_index][family]
                for family in candidate.families
            )
            global_key_score = sum(PROVENANCE_KEY_PRIORITY[key] for key in candidate.keys)
            min_distance = (
                min(
                    oklab_distance(candidate.color, existing.color)
                    for existing in chosen
                )
                if chosen
                else 0.0
            )
            return (
                key_score * 1.6
                + family_score * 0.8
                + global_key_score * 0.08
                + min_distance * 24.0
                + candidate.source_count * 0.06
                + candidate.family_score * 0.08
                + min(background_contrast(candidate.color, case.background), 90.0) / 60.0
            )

        next_candidate = max(remaining, key=candidate_score)
        chosen.append(next_candidate)
        remaining.remove(next_candidate)

    return fill_with_aggregate(case, [candidate.color for candidate in chosen], target_count)


def terminal_status_canonical_generator(case: ThemeCase, target_count: int) -> list[Color]:
    pool = provenance_source_pool(case)
    keyed = {source_color.key: source_color for source_color in pool}
    preferred_keys = [
        "terminal.ansi.blue",
        "renamed",
        "terminal.ansi.green",
        "conflict",
        "terminal.ansi.yellow",
        "terminal.ansi.cyan",
        "terminal.ansi.magenta",
        "created",
        "terminal.ansi.red",
        "modified",
        "terminal.ansi.white",
        "created.background",
    ]
    chosen: list[Color] = []
    used_rgb: set[str] = set()
    for key in preferred_keys:
        source_color = keyed.get(key)
        if source_color is None:
            continue
        rgb = normalize_rgb_hex(source_color.color)
        if rgb in used_rgb:
            continue
        chosen.append(source_color.color)
        used_rgb.add(rgb)
        if len(chosen) >= target_count:
            return chosen
    return fill_with_aggregate(case, chosen, target_count)


GENERATORS: dict[str, Callable[[ThemeCase, int], list[Color]]] = {
    "terminal_canonical": terminal_canonical_generator,
    "terminal_status_canonical": terminal_status_canonical_generator,
    "ranked_key_canonical": ranked_key_canonical_generator,
    "slotwise_provenance": slotwise_provenance_generator,
    "slotwise_provenance_strict": slotwise_provenance_strict_generator,
    "slotwise_provenance_balanced": slotwise_provenance_balanced_generator,
    "slotwise_provenance_farthest": slotwise_provenance_farthest_generator,
    "aggregate_priority": aggregate_priority_generator,
    "priority_farthest": priority_farthest_generator,
    "terminal_seeded_farthest": terminal_seeded_farthest_generator,
}


def greedy_bipartite_distance(left: list[Color], right: list[Color]) -> float:
    if not left or not right:
        return float("inf")

    remaining_right = list(range(len(right)))
    total = 0.0
    for left_color in left:
        best_index = min(
            remaining_right,
            key=lambda index: oklab_distance(left_color, right[index]),
        )
        total += oklab_distance(left_color, right[best_index])
        remaining_right.remove(best_index)
        if not remaining_right:
            break
    return total / len(left)


def evaluate_explicit(cases: list[ThemeCase], generator_name: str, generator: Callable[[ThemeCase, int], list[Color]]) -> None:
    explicit_cases = [case for case in cases if case.explicit_accents]
    exact_counts = 0
    all_exact_theme_count = 0
    avg_cover_distances: list[float] = []
    avg_palette_sizes: list[int] = []

    for case in explicit_cases:
        generated = generator(case, len(case.accents))
        avg_palette_sizes.append(len(generated))
        generated_set = {normalize_rgb_hex(color) for color in generated}
        exact_matches = sum(
            normalize_rgb_hex(accent) in generated_set for accent in case.accents
        )
        exact_counts += exact_matches
        if exact_matches == len(case.accents):
            all_exact_theme_count += 1
        avg_cover_distances.append(greedy_bipartite_distance(list(case.accents), generated))

    total_accents = sum(len(case.accents) for case in explicit_cases)
    print(
        f"{generator_name:24} "
        f"exact_rgb={exact_counts:4d}/{total_accents} ({exact_counts / total_accents:.1%}) "
        f"all_exact_themes={all_exact_theme_count:3d}/{len(explicit_cases)} ({all_exact_theme_count / len(explicit_cases):.1%}) "
        f"avg_cover_d={mean(avg_cover_distances):.4f} "
        f"avg_size={mean(avg_palette_sizes):.2f}"
    )


def evaluate_fallback(cases: list[ThemeCase], generator_name: str, generator: Callable[[ThemeCase, int], list[Color]], target_count: int) -> None:
    fallback_cases = [case for case in cases if not case.explicit_accents]
    final_scores: list[float] = []
    final_bg_scores: list[float] = []
    palette_sizes: list[int] = []

    for case in fallback_cases:
        generated = generator(case, target_count)
        palette_sizes.append(len(generated))
        adjusted = [
            adjust_color_for_background(color, case.background, 35.0 if case.appearance == "light" else 30.0)
            for color in generated
        ]
        authored = make_candidate(
            "authored",
            adjusted,
            case.background,
            authored_order(len(adjusted)),
        )
        threshold = target_threshold(case.appearance)
        if authored.min_adjacent_distance >= threshold:
            final_palette = adjusted
            final_score = authored.min_adjacent_distance
        else:
            candidates = [
                make_candidate("structured", adjusted, case.background, order)
                for order in dict.fromkeys(cut_then_move_orders(len(adjusted)))
            ]
            chosen = threshold_pick(candidates, threshold)
            final_palette = [adjusted[index] for index in chosen.order]
            final_score = chosen.min_adjacent_distance

        final_scores.append(final_score)
        final_bg_scores.append(
            min(background_contrast(color, case.background) for color in final_palette)
        )

    print(
        f"{generator_name:24} "
        f"avg_final_min_adj={mean(final_scores):.4f} "
        f"avg_final_min_bg={mean(final_bg_scores):.4f} "
        f"avg_size={mean(palette_sizes):.2f}"
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fallback-count", type=int, default=6)
    args = parser.parse_args()

    cases = build_theme_cases()
    initialize_provenance_priors(cases)

    print("Explicit-theme recovery")
    print("-----------------------")
    for name, generator in GENERATORS.items():
        evaluate_explicit(cases, name, generator)

    print()
    print(f"Fallback-theme bracket evaluation (target_count={args.fallback_count})")
    print("--------------------------------------------------------------")
    for name, generator in GENERATORS.items():
        evaluate_fallback(cases, name, generator, args.fallback_count)


if __name__ == "__main__":
    main()
