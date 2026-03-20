from __future__ import annotations

from dataclasses import dataclass
from statistics import mean

from coloraide import Color

from analyze_extension_themes import load_results
from palette_probe import authored_order
from strategy_sweep import cut_then_move_orders, make_candidate
from theme_pipeline import adjust_color_for_background, background_contrast, target_threshold


@dataclass(frozen=True)
class ThemeEval:
    theme_name: str
    appearance: str
    explicit_accents: bool
    background: Color
    final_palette: tuple[Color, ...]
    final_min_adj: float
    final_min_bg: float
    final_bg_contrasts: tuple[float, ...]


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


def evaluate_theme(result, background_floor: float) -> ThemeEval:
    background = Color(result.inputs.background)
    adjusted_colors = [
        adjust_color_for_background(Color(value), background, background_floor)
        for value in result.inputs.accents
    ]
    authored = make_candidate(
        "authored",
        adjusted_colors,
        background,
        authored_order(len(adjusted_colors)),
    )
    final_threshold = target_threshold(result.inputs.appearance)
    if authored.min_adjacent_distance >= final_threshold:
        final_palette = tuple(adjusted_colors)
        final_min_adj = authored.min_adjacent_distance
    else:
        candidates = [
            make_candidate("structured", adjusted_colors, background, order)
            for order in dict.fromkeys(cut_then_move_orders(len(adjusted_colors)))
        ]
        chosen = threshold_pick(candidates, final_threshold)
        final_palette = tuple(adjusted_colors[index] for index in chosen.order)
        final_min_adj = chosen.min_adjacent_distance

    bg_contrasts = tuple(background_contrast(color, background) for color in final_palette)
    return ThemeEval(
        theme_name=result.inputs.theme_name,
        appearance=result.inputs.appearance,
        explicit_accents=result.inputs.explicit_accents,
        background=background,
        final_palette=final_palette,
        final_min_adj=final_min_adj,
        final_min_bg=min(bg_contrasts),
        final_bg_contrasts=bg_contrasts,
    )


def compare(appearance: str, current_floor: float, candidate_floor: float, reference_floor: float) -> None:
    results = [result for result in load_results() if result.inputs.appearance == appearance]
    current_evals = [evaluate_theme(result, current_floor) for result in results]
    candidate_evals = [evaluate_theme(result, candidate_floor) for result in results]
    explicit_indexes = [index for index, result in enumerate(results) if result.inputs.explicit_accents]
    fallback_indexes = [index for index, result in enumerate(results) if not result.inputs.explicit_accents]
    subsets = {
        "all": list(range(len(results))),
        "explicit": explicit_indexes,
        "fallback": fallback_indexes,
    }

    print()
    print(
        f"{appearance}: current_bg={current_floor:.1f} candidate_bg={candidate_floor:.1f} reference={reference_floor:.1f}"
    )
    print(
        f"{'group':10} {'themes':>5} {'new_theme_fail':>14} {'new_acc_fail':>13} "
        f"{'worse_min_bg':>12} {'avg_delta_min_bg':>16} {'avg_final_min_bg':>16} {'avg_final_min_adj':>17}"
    )

    for label, indexes in subsets.items():
        new_theme_fail = 0
        new_acc_fail = 0
        worse_min_bg = 0
        min_bg_deltas: list[float] = []

        for index in indexes:
            current_eval = current_evals[index]
            candidate_eval = candidate_evals[index]
            min_bg_deltas.append(candidate_eval.final_min_bg - current_eval.final_min_bg)
            if candidate_eval.final_min_bg < current_eval.final_min_bg:
                worse_min_bg += 1
            if (
                current_eval.final_min_bg >= reference_floor
                and candidate_eval.final_min_bg < reference_floor
            ):
                new_theme_fail += 1

            for current_contrast, candidate_contrast in zip(
                current_eval.final_bg_contrasts,
                candidate_eval.final_bg_contrasts,
            ):
                if current_contrast >= reference_floor and candidate_contrast < reference_floor:
                    new_acc_fail += 1

        print(
            f"{label:10} {len(indexes):5d} {new_theme_fail:14d} {new_acc_fail:13d} "
            f"{worse_min_bg:12d} {mean(min_bg_deltas):16.4f} "
            f"{mean(candidate_evals[index].final_min_bg for index in indexes):16.4f} "
            f"{mean(candidate_evals[index].final_min_adj for index in indexes):17.4f}"
        )


def main() -> None:
    compare("dark", current_floor=30.0, candidate_floor=27.5, reference_floor=30.0)
    compare("light", current_floor=35.0, candidate_floor=32.5, reference_floor=35.0)


if __name__ == "__main__":
    main()
