from __future__ import annotations

import argparse
import json
from collections import defaultdict
from pathlib import Path
from statistics import mean

import json5

from theme_pipeline import (
    PipelineResult,
    analyze_theme,
    intervention_threshold,
    resolve_theme_inputs,
    target_threshold,
)


REPOS_DIR = Path("bracket-utils/theme-repos")


def iter_theme_json_files() -> list[Path]:
    return sorted(REPOS_DIR.glob("*/themes/*.json"))


def load_theme_file(path: Path) -> object:
    text = path.read_text()
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return json5.loads(text)


def load_results() -> list[PipelineResult]:
    results: list[PipelineResult] = []
    for path in iter_theme_json_files():
        data = load_theme_file(path)
        if not isinstance(data, dict) or "themes" not in data:
            continue
        for theme in data["themes"]:
            inputs = resolve_theme_inputs(str(path), theme)
            if len(inputs.accents) < 2:
                continue
            results.append(analyze_theme(inputs))
    return results


def print_summary(results: list[PipelineResult]) -> None:
    grouped: dict[str, list[PipelineResult]] = defaultdict(list)
    for result in results:
        grouped["all"].append(result)
        grouped[result.inputs.appearance].append(result)
        grouped["explicit" if result.inputs.explicit_accents else "fallback"].append(result)

    print(f"Analyzed theme variants: {len(results)}")
    print()
    for group_name in ["all", "dark", "light", "explicit", "fallback"]:
        group = grouped.get(group_name)
        if not group:
            continue
        print(f"{group_name}: {len(group)} themes")
        if group_name in {"dark", "light"}:
            print(
                f"  thresholds: intervention={intervention_threshold(group_name):.3f} "
                f"target={target_threshold(group_name):.3f}"
            )

        for label, getter in [
            ("raw", lambda result: result.authored),
            ("bg", lambda result: result.background_adjusted),
            ("swap", lambda result: result.swap),
            ("stride", lambda result: result.stride),
            ("greedy", lambda result: result.greedy),
        ]:
            values = [getter(result).min_adjacent_distance for result in group]
            print(f"  {label:7} avg_min_adj={mean(values):.4f}")

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
        print()


def print_top_examples(results: list[PipelineResult], limit: int) -> None:
    print("Most improved by single swap over background-adjusted order")
    print("----------------------------------------------------------")
    rows = sorted(
        results,
        key=lambda result: (
            result.swap.min_adjacent_distance - result.background_adjusted.min_adjacent_distance,
            -result.swap.displacement_cost,
        ),
        reverse=True,
    )
    for result in rows[:limit]:
        print(
            f"{result.inputs.theme_name[:28]:28} "
            f"exp={'yes' if result.inputs.explicit_accents else 'no':3} "
            f"bg={result.background_adjusted.min_adjacent_distance:.4f} "
            f"swap={result.swap.min_adjacent_distance:.4f} "
            f"greedy={result.greedy.min_adjacent_distance:.4f} "
            f"file={result.inputs.file}"
        )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--limit", type=int, default=10)
    parser.add_argument("--summary-only", action="store_true")
    args = parser.parse_args()

    results = load_results()
    print_summary(results)
    if not args.summary_only:
        print_top_examples(results, args.limit)


if __name__ == "__main__":
    main()
