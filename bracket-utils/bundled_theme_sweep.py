from __future__ import annotations

import argparse
import json
from collections import defaultdict
from pathlib import Path
from statistics import mean

from theme_pipeline import (
    PipelineResult,
    analyze_theme,
    intervention_threshold,
    resolve_theme_inputs,
    target_threshold,
)


def load_bundled_results() -> list[PipelineResult]:
    results: list[PipelineResult] = []
    for path in sorted(Path("assets/themes").glob("*/*.json")):
        data = json.load(open(path))
        for theme in data["themes"]:
            theme_inputs = resolve_theme_inputs(str(path), theme)
            results.append(analyze_theme(theme_inputs))
    return results


def print_per_theme(results: list[PipelineResult]) -> None:
    print(
        f"{'theme':24} {'app':5} {'exp':3} {'raw':>7} {'bg':>7} {'swap':>7} {'stride':>7} {'greedy':>7} "
        f"{'swap_d':>7} {'stride_d':>9} {'greedy_d':>9}"
    )
    for result in results:
        print(
            f"{result.inputs.theme_name[:24]:24} {result.inputs.appearance[:5]:5} "
            f"{('yes' if result.inputs.explicit_accents else 'no'):3} "
            f"{result.authored.min_adjacent_distance:7.4f} "
            f"{result.background_adjusted.min_adjacent_distance:7.4f} "
            f"{result.swap.min_adjacent_distance:7.4f} "
            f"{result.stride.min_adjacent_distance:7.4f} "
            f"{result.greedy.min_adjacent_distance:7.4f} "
            f"{result.swap.displacement_cost:7d} "
            f"{result.stride.displacement_cost:9d} "
            f"{result.greedy.displacement_cost:9d}"
        )


def print_summary(results: list[PipelineResult]) -> None:
    by_group: dict[str, list[PipelineResult]] = defaultdict(list)
    for result in results:
        by_group["all"].append(result)
        by_group[result.inputs.appearance].append(result)
        by_group["explicit" if result.inputs.explicit_accents else "fallback"].append(result)

    print()
    print("Summary")
    print("-------")
    for group_name in ["all", "dark", "light", "explicit", "fallback"]:
        group = by_group.get(group_name)
        if not group:
            continue
        print(f"{group_name}: {len(group)} themes")
        appearances = sorted({result.inputs.appearance for result in group})
        if len(appearances) == 1:
            appearance = appearances[0]
            print(
                f"  thresholds: intervention={intervention_threshold(appearance):.3f} "
                f"target={target_threshold(appearance):.3f}"
            )

        for label, getter in [
            ("raw", lambda result: result.authored),
            ("bg", lambda result: result.background_adjusted),
            ("swap", lambda result: result.swap),
            ("stride", lambda result: result.stride),
            ("greedy", lambda result: result.greedy),
        ]:
            scores = [getter(result).min_adjacent_distance for result in group]
            print(f"  {label:7} avg_min_adj={mean(scores):.4f}")

        swap_gain_share = mean(
            1.0
            if abs(
                result.greedy.min_adjacent_distance
                - result.background_adjusted.min_adjacent_distance
            )
            < 1e-9
            else (
                (result.swap.min_adjacent_distance - result.background_adjusted.min_adjacent_distance)
                / (
                    result.greedy.min_adjacent_distance
                    - result.background_adjusted.min_adjacent_distance
                )
            )
            for result in group
        )
        stride_gain_share = mean(
            1.0
            if abs(
                result.greedy.min_adjacent_distance
                - result.background_adjusted.min_adjacent_distance
            )
            < 1e-9
            else (
                (result.stride.min_adjacent_distance - result.background_adjusted.min_adjacent_distance)
                / (
                    result.greedy.min_adjacent_distance
                    - result.background_adjusted.min_adjacent_distance
                )
            )
            for result in group
        )
        print(
            f"  gain share vs greedy after bg adjust: swap={swap_gain_share:.3f} stride={stride_gain_share:.3f}"
        )
        print(
            "  avg displacement:"
            f" swap={mean(result.swap.displacement_cost for result in group):.2f}"
            f" stride={mean(result.stride.displacement_cost for result in group):.2f}"
            f" greedy={mean(result.greedy.displacement_cost for result in group):.2f}"
        )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--summary-only", action="store_true")
    args = parser.parse_args()

    results = load_bundled_results()
    print(f"Bundled theme variants analyzed: {len(results)}")
    print("Raw themes with no explicit accents are resolved through the same fallback rule as Rust.")
    print()
    if not args.summary_only:
        print_per_theme(results)
    print_summary(results)


if __name__ == "__main__":
    main()
