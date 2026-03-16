use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use editor::{
    BracketColorizationPaletteAnalysis, BracketColorizationPaletteStrategy,
    analyze_bracket_colorization_palette,
};
use gpui::{Hsla, Rgba, SharedString};
use language::language_settings::BracketColorizationMode;
use theme::{ThemeFamilyContent, refine_theme_family};

fn main() -> Result<()> {
    let theme_paths = env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if theme_paths.is_empty() {
        bail!(
            "usage: cargo run -p editor --example test_bracket_contrast -- <theme.json> [theme.json ...]"
        );
    }

    for (path_index, theme_path) in theme_paths.iter().enumerate() {
        if path_index > 0 {
            println!();
        }

        analyze_theme_file(theme_path)?;
    }

    Ok(())
}

fn analyze_theme_file(theme_path: &Path) -> Result<()> {
    let bytes = std::fs::read(theme_path)
        .with_context(|| format!("failed to read {}", theme_path.display()))?;
    let theme_family_content: ThemeFamilyContent = serde_json_lenient::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", theme_path.display()))?;

    println!("Theme file: {}", theme_path.display());
    println!(
        "Family: {} (author: {})",
        theme_family_content.name, theme_family_content.author
    );

    let theme_family = refine_theme_family(theme_family_content);
    for theme in &theme_family.themes {
        println!();
        println!("Theme: {}", theme.name);
        println!("Appearance: {}", appearance_label(theme.appearance));

        let analysis = analyze_bracket_colorization_palette(
            theme.accents().0.as_ref(),
            theme.appearance,
            BracketColorizationMode::Auto,
        );

        print_analysis(&theme.name, &analysis);
    }

    Ok(())
}

fn print_analysis(theme_name: &SharedString, analysis: &BracketColorizationPaletteAnalysis) {
    let threshold = analysis.threshold.unwrap_or_default();
    let contrast_status = if analysis.threshold.is_some()
        && analysis.original_score.min_adjacent_distance >= threshold
    {
        "good"
    } else {
        "bad"
    };

    println!(
        "Original palette: {}",
        format_palette(&analysis.original_palette)
    );
    println!(
        "Original min adjacent distance: {:.3}",
        analysis.original_score.min_adjacent_distance
    );
    println!(
        "Original average adjacent distance: {:.3}",
        analysis.original_score.average_adjacent_distance
    );

    if analysis.threshold.is_some() {
        println!("Threshold: {:.3}", threshold);
    }

    println!("Result: contrast {contrast_status}");
    println!(
        "Bracket colors changed: {}",
        if analysis.changed { "yes" } else { "no" }
    );
    println!("Strategy used: {}", strategy_label(analysis.strategy));
    println!("Final palette: {}", format_palette(&analysis.final_palette));
    println!(
        "Final min adjacent distance: {:.3}",
        analysis.final_score.min_adjacent_distance
    );
    println!(
        "Final average adjacent distance: {:.3}",
        analysis.final_score.average_adjacent_distance
    );

    if analysis.changed {
        println!("Summary: {theme_name} uses a reordered bracket palette in auto mode.");
    } else {
        println!("Summary: {theme_name} keeps the theme palette in auto mode.");
    }
}

fn appearance_label(appearance: theme::Appearance) -> &'static str {
    match appearance {
        theme::Appearance::Light => "light",
        theme::Appearance::Dark => "dark",
    }
}

fn strategy_label(strategy: BracketColorizationPaletteStrategy) -> &'static str {
    match strategy {
        BracketColorizationPaletteStrategy::ThemeOrder => "theme order",
        BracketColorizationPaletteStrategy::PreservedSmallPalette => "palette too small to reorder",
        BracketColorizationPaletteStrategy::PreservedStrongPalette => "contrast already sufficient",
        BracketColorizationPaletteStrategy::PreservedWeakPalette => {
            "weak palette kept because reorder was not meaningfully better"
        }
        BracketColorizationPaletteStrategy::ReorderedWeakPalette => "reordered weak palette",
    }
}

fn format_palette(colors: &[Hsla]) -> String {
    let formatted_colors = colors.iter().copied().map(format_color).collect::<Vec<_>>();
    format!("[{}]", formatted_colors.join(", "))
}

fn format_color(color: Hsla) -> String {
    let rgba = Rgba::from(color);
    let red = to_u8(rgba.r);
    let green = to_u8(rgba.g);
    let blue = to_u8(rgba.b);
    let alpha = to_u8(rgba.a);

    if alpha == u8::MAX {
        format!("#{red:02X}{green:02X}{blue:02X}")
    } else {
        format!("#{red:02X}{green:02X}{blue:02X}{alpha:02X}")
    }
}

fn to_u8(channel: f32) -> u8 {
    (channel.clamp(0.0, 1.0) * 255.0).round() as u8
}
