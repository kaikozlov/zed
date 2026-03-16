use std::env;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use editor::{
    AutoBracketColorizationConfig, BracketColorizationPaletteAnalysis,
    BracketColorizationPaletteStrategy, analyze_bracket_colorization_palette_with_config,
};
use gpui::{Hsla, Rgba, SharedString};
use language::language_settings::BracketColorizationMode;
use theme::{Theme, ThemeFamilyContent, refine_theme_family};

const SVG_WIDTH: f32 = 1600.0;
const PANEL_WIDTH: f32 = 740.0;
const PANEL_GAP: f32 = 40.0;
const ROOT_PADDING: f32 = 40.0;
const PANEL_PADDING: f32 = 24.0;
const PANEL_TOP: f32 = 150.0;
const ROW_HEIGHT: f32 = 28.0;
const CODE_ROW_HEIGHT: f32 = 22.0;
const CODE_FONT_SIZE: f32 = 16.0;
const CODE_CHARACTER_WIDTH: f32 = 9.6;
const BRACKET_DEMO_FONT_SIZE: f32 = 18.0;
const BRACKET_CHARACTER_WIDTH: f32 = 12.0;
const CODE_DEMO_BOX_HEIGHT: f32 = 408.0;
const CODE_DEMO_LABEL_Y: f32 = 176.0;
const CODE_DEMO_BOX_Y: f32 = 190.0;
const PALETTE_LABEL_Y: f32 = 626.0;
const PALETTE_ROWS_Y: f32 = 650.0;
const FONT_FAMILY: &str =
    "'Zed Mono', 'SF Mono', 'JetBrains Mono', 'Cascadia Code', Menlo, Consolas, monospace";

fn main() -> Result<()> {
    let options = Options::parse()?;
    if let Some(svg_output_directory) = &options.svg_output_directory {
        fs::create_dir_all(svg_output_directory).with_context(|| {
            format!(
                "failed to create SVG output directory {}",
                svg_output_directory.display()
            )
        })?;
    }

    for (path_index, theme_path) in options.theme_paths.iter().enumerate() {
        if path_index > 0 {
            println!();
        }

        analyze_theme_file(theme_path, &options)?;
    }

    Ok(())
}

struct Options {
    theme_paths: Vec<PathBuf>,
    svg_output_directory: Option<PathBuf>,
    auto_config: AutoBracketColorizationConfig,
}

impl Options {
    fn parse() -> Result<Self> {
        let mut args = env::args_os().skip(1);
        let mut theme_paths = Vec::new();
        let mut svg_output_directory = None;
        let mut auto_config = AutoBracketColorizationConfig::default();

        while let Some(arg) = args.next() {
            if arg == "--svg-out" {
                let Some(directory) = args.next() else {
                    bail!("expected directory after --svg-out");
                };
                svg_output_directory = Some(PathBuf::from(directory));
            } else if arg == "--auto-background-apca-light-threshold" {
                let Some(threshold) = args.next() else {
                    bail!("expected threshold after --auto-background-apca-light-threshold");
                };
                auto_config.minimum_background_apca_light =
                    threshold.to_string_lossy().parse().context(
                        "failed to parse --auto-background-apca-light-threshold as a number",
                    )?;
            } else if arg == "--auto-background-apca-dark-threshold" {
                let Some(threshold) = args.next() else {
                    bail!("expected threshold after --auto-background-apca-dark-threshold");
                };
                auto_config.minimum_background_apca_dark = threshold
                    .to_string_lossy()
                    .parse()
                    .context("failed to parse --auto-background-apca-dark-threshold as a number")?;
            } else if arg == "--auto-adjacent-oklab-light-threshold" {
                let Some(threshold) = args.next() else {
                    bail!("expected threshold after --auto-adjacent-oklab-light-threshold");
                };
                auto_config.minimum_adjacent_oklab_light = threshold
                    .to_string_lossy()
                    .parse()
                    .context("failed to parse --auto-adjacent-oklab-light-threshold as a number")?;
            } else if arg == "--auto-adjacent-oklab-dark-threshold" {
                let Some(threshold) = args.next() else {
                    bail!("expected threshold after --auto-adjacent-oklab-dark-threshold");
                };
                auto_config.minimum_adjacent_oklab_dark = threshold
                    .to_string_lossy()
                    .parse()
                    .context("failed to parse --auto-adjacent-oklab-dark-threshold as a number")?;
            } else {
                theme_paths.push(PathBuf::from(arg));
            }
        }

        if theme_paths.is_empty() {
            bail!(
                "usage: cargo run -p editor --example test_bracket_contrast -- [--svg-out <dir>] [--auto-background-apca-light-threshold <value>] [--auto-background-apca-dark-threshold <value>] [--auto-adjacent-oklab-light-threshold <value>] [--auto-adjacent-oklab-dark-threshold <value>] <theme.json> [theme.json ...]"
            );
        }

        Ok(Self {
            theme_paths,
            svg_output_directory,
            auto_config,
        })
    }
}

fn analyze_theme_file(theme_path: &Path, options: &Options) -> Result<()> {
    let bytes =
        fs::read(theme_path).with_context(|| format!("failed to read {}", theme_path.display()))?;
    let theme_family_content: ThemeFamilyContent = serde_json_lenient::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", theme_path.display()))?;

    println!("Theme file: {}", theme_path.display());
    println!(
        "Family: {} (author: {})",
        theme_family_content.name, theme_family_content.author
    );

    let theme_family_name = theme_family_content.name.clone();
    let theme_family = refine_theme_family(theme_family_content);
    for theme in &theme_family.themes {
        println!();
        println!("Theme: {}", theme.name);
        println!("Appearance: {}", appearance_label(theme.appearance));

        let analysis = analyze_bracket_colorization_palette_with_config(
            theme.accents().0.as_ref(),
            theme.appearance,
            theme.colors().editor_background,
            BracketColorizationMode::Auto,
            options.auto_config,
        );

        print_analysis(
            &theme.name,
            &analysis,
            theme.colors().editor_background,
            std::io::stdout().is_terminal(),
        );

        if let Some(svg_output_directory) = &options.svg_output_directory {
            let svg_path =
                write_svg_report(svg_output_directory, &theme_family_name, theme, &analysis)?;
            println!("SVG report: {}", svg_path.display());
        }
    }

    Ok(())
}

fn print_analysis(
    theme_name: &SharedString,
    analysis: &BracketColorizationPaletteAnalysis,
    editor_background: Hsla,
    use_color: bool,
) {
    let adjacent_threshold = analysis.adjacent_threshold.unwrap_or_default();
    let original_adjacent_status = threshold_result(
        analysis.original_score.min_adjacent_distance,
        analysis.adjacent_threshold,
    );
    let final_adjacent_status = threshold_result(
        analysis.final_score.min_adjacent_distance,
        analysis.adjacent_threshold,
    );
    let original_background_status = threshold_result(
        analysis.original_score.min_background_contrast,
        analysis.background_threshold,
    );
    let final_background_status = threshold_result(
        analysis.final_score.min_background_contrast,
        analysis.background_threshold,
    );

    println!("Overview:");
    println!("Editor background: {}", format_color(editor_background));
    println!("Adjacent threshold: {original_adjacent_status} -> {final_adjacent_status}");
    println!("Background threshold: {original_background_status} -> {final_background_status}");
    println!(
        "Bracket colors changed: {}",
        if analysis.changed { "yes" } else { "no" }
    );
    println!("Strategy used: {}", strategy_label(analysis.strategy));
    println!(
        "Reordered: {}",
        if strategy_reordered(analysis.strategy) {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        "Background-adjusted: {}",
        if strategy_adjusted_background(analysis.strategy) {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        "Min adjacent distance: {:.3} -> {:.3}",
        analysis.original_score.min_adjacent_distance, analysis.final_score.min_adjacent_distance
    );
    println!(
        "Average adjacent distance: {:.3} -> {:.3}",
        analysis.original_score.average_adjacent_distance,
        analysis.final_score.average_adjacent_distance
    );
    println!(
        "Min background contrast: {:.3} -> {:.3}",
        analysis.original_score.min_background_contrast,
        analysis.final_score.min_background_contrast
    );
    println!(
        "Average background contrast: {:.3} -> {:.3}",
        analysis.original_score.average_background_contrast,
        analysis.final_score.average_background_contrast
    );
    if analysis.adjacent_threshold.is_some() {
        println!("Adjacent threshold value: {:.3}", adjacent_threshold);
    }
    if let Some(threshold) = analysis.background_threshold {
        println!("Background threshold value: {:.3}", threshold);
    }

    let original_pairs = adjacent_pairs(&analysis.original_palette);
    let final_pairs = adjacent_pairs(&analysis.final_palette);
    if let Some((left, right, distance)) = weakest_pair(&original_pairs) {
        println!(
            "Weakest original pair: {}-{} ({}, {}) distance {:.3}",
            left + 1,
            right + 1,
            format_color(analysis.original_palette[left]),
            format_color(analysis.original_palette[right]),
            distance
        );
    }
    if let Some((left, right, distance)) = weakest_pair(&final_pairs) {
        println!(
            "Weakest final pair: {}-{} ({}, {}) distance {:.3}",
            left + 1,
            right + 1,
            format_color(analysis.final_palette[left]),
            format_color(analysis.final_palette[right]),
            distance
        );
    }

    println!();
    println!("Original palette:");
    print_palette_rows(&analysis.original_palette, editor_background, use_color);

    println!();
    println!("Final palette:");
    print_palette_rows(&analysis.final_palette, editor_background, use_color);

    println!();
    println!("Order mapping:");
    println!(
        "Final positions from original palette: {}",
        format_order_mapping(&analysis.original_palette, &analysis.final_palette)
    );

    println!();
    println!("Bracket demo (on editor background):");
    println!(
        "Original: {}",
        format_bracket_demo(&analysis.original_palette, editor_background, use_color)
    );
    println!(
        "Final:    {}",
        format_bracket_demo(&analysis.final_palette, editor_background, use_color)
    );
    println!(
        "Depths:   {}",
        format_depth_demo(analysis.final_palette.len())
    );

    if analysis.changed {
        println!("Summary: {theme_name} uses a reordered bracket palette in auto mode.");
    } else {
        println!("Summary: {theme_name} keeps the theme palette in auto mode.");
    }
}

fn print_palette_rows(colors: &[Hsla], editor_background: Hsla, use_color: bool) {
    for (index, color) in colors.iter().copied().enumerate() {
        println!(
            "{:>2}. {} {} {}",
            index + 1,
            swatch(color, use_color),
            bracket_sample(color, editor_background, use_color),
            format_color(color)
        );
    }
}

fn write_svg_report(
    output_directory: &Path,
    theme_family_name: &str,
    theme: &Theme,
    analysis: &BracketColorizationPaletteAnalysis,
) -> Result<PathBuf> {
    let filename = format!(
        "{}-{}.svg",
        slugify(theme_family_name),
        slugify(theme.name.as_ref())
    );
    let output_path = output_directory.join(filename);
    let svg = render_svg_report(theme_family_name, theme, analysis);
    fs::write(&output_path, svg)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    Ok(output_path)
}

fn render_svg_report(
    theme_family_name: &str,
    theme: &Theme,
    analysis: &BracketColorizationPaletteAnalysis,
) -> String {
    let editor_background = theme.colors().editor_background;
    let editor_foreground = theme.colors().editor_foreground;
    let muted_text = theme.colors().text_muted;
    let border_color = muted_text;

    let palette_row_count = analysis
        .original_palette
        .len()
        .max(analysis.final_palette.len()) as f32;
    let panel_height = 728.0 + palette_row_count * ROW_HEIGHT;
    let svg_height = PANEL_TOP + panel_height + ROOT_PADDING;

    let left_panel_x = ROOT_PADDING;
    let right_panel_x = ROOT_PADDING + PANEL_WIDTH + PANEL_GAP;

    let original_adjacent_status = threshold_result(
        analysis.original_score.min_adjacent_distance,
        analysis.adjacent_threshold,
    );
    let final_adjacent_status = threshold_result(
        analysis.final_score.min_adjacent_distance,
        analysis.adjacent_threshold,
    );
    let original_background_status = threshold_result(
        analysis.original_score.min_background_contrast,
        analysis.background_threshold,
    );
    let final_background_status = threshold_result(
        analysis.final_score.min_background_contrast,
        analysis.background_threshold,
    );

    let mut svg = String::new();
    svg.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{SVG_WIDTH}" height="{svg_height}" viewBox="0 0 {SVG_WIDTH} {svg_height}">"#
    ));
    svg.push_str(&format!(
        r#"<rect width="100%" height="100%" fill="{}"/>"#,
        css_color(editor_background)
    ));
    svg.push_str(&format!(
        r#"<text x="{ROOT_PADDING}" y="46" font-family="{FONT_FAMILY}" font-size="28" font-weight="700" fill="{}">{}</text>"#,
        css_color(editor_foreground),
        escape_xml(&format!("{} / {}", theme_family_name, theme.name))
    ));
    svg.push_str(&format!(
        r#"<text x="{ROOT_PADDING}" y="74" font-family="{FONT_FAMILY}" font-size="16" fill="{}">{}</text>"#,
        css_color(muted_text),
        escape_xml(&format!(
            "appearance: {}  |  strategy: {}  |  adjacent: {} -> {}  |  background: {} -> {}",
            appearance_label(theme.appearance),
            strategy_label(analysis.strategy),
            original_adjacent_status,
            final_adjacent_status,
            original_background_status,
            final_background_status
        ))
    ));
    svg.push_str(&format!(
        r#"<text x="{ROOT_PADDING}" y="102" font-family="{FONT_FAMILY}" font-size="16" fill="{}">{}</text>"#,
        css_color(muted_text),
        escape_xml(&format!(
            "min adjacent distance: {:.3} -> {:.3}    min background contrast: {:.3} -> {:.3}    background: {}",
            analysis.original_score.min_adjacent_distance,
            analysis.final_score.min_adjacent_distance,
            analysis.original_score.min_background_contrast,
            analysis.final_score.min_background_contrast,
            format_color(editor_background)
        ))
    ));

    svg.push_str(&render_svg_panel(
        left_panel_x,
        PANEL_TOP,
        PANEL_WIDTH,
        panel_height,
        "Original Theme Order",
        &analysis.original_palette,
        editor_background,
        editor_foreground,
        muted_text,
        border_color,
    ));
    svg.push_str(&render_svg_panel(
        right_panel_x,
        PANEL_TOP,
        PANEL_WIDTH,
        panel_height,
        "Auto / Final Order",
        &analysis.final_palette,
        editor_background,
        editor_foreground,
        muted_text,
        border_color,
    ));

    svg.push_str("</svg>");
    svg
}

fn render_svg_panel(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    title: &str,
    colors: &[Hsla],
    editor_background: Hsla,
    editor_foreground: Hsla,
    muted_text: Hsla,
    border_color: Hsla,
) -> String {
    let mut svg = String::new();
    svg.push_str(&format!(
        r#"<rect x="{x}" y="{y}" width="{width}" height="{height}" rx="16" fill="{}" stroke="{}" stroke-width="1.5"/>"#,
        css_color(editor_background),
        css_color(border_color)
    ));
    svg.push_str(&format!(
        r#"<text x="{}" y="{}" font-family="{FONT_FAMILY}" font-size="22" font-weight="700" fill="{}">{}</text>"#,
        x + PANEL_PADDING,
        y + 34.0,
        css_color(editor_foreground),
        escape_xml(title)
    ));
    svg.push_str(&format!(
        r#"<text x="{}" y="{}" font-family="{FONT_FAMILY}" font-size="14" fill="{}">{}</text>"#,
        x + PANEL_PADDING,
        y + 58.0,
        css_color(muted_text),
        escape_xml("direct adjacent brackets")
    ));
    svg.push_str(&render_svg_demo_box(
        x + PANEL_PADDING,
        y + 72.0,
        width - PANEL_PADDING * 2.0,
        76.0,
        editor_background,
        editor_foreground,
        border_color,
        &[SvgLine::BracketRun(colors), SvgLine::Depths(colors.len())],
        colors,
    ));
    svg.push_str(&format!(
        r#"<text x="{}" y="{}" font-family="{FONT_FAMILY}" font-size="14" fill="{}">{}</text>"#,
        x + PANEL_PADDING,
        y + CODE_DEMO_LABEL_Y,
        css_color(muted_text),
        escape_xml("code demo")
    ));
    svg.push_str(&render_svg_demo_box(
        x + PANEL_PADDING,
        y + CODE_DEMO_BOX_Y,
        width - PANEL_PADDING * 2.0,
        CODE_DEMO_BOX_HEIGHT,
        editor_background,
        editor_foreground,
        border_color,
        &code_demo_lines(),
        colors,
    ));
    svg.push_str(&format!(
        r#"<text x="{}" y="{}" font-family="{FONT_FAMILY}" font-size="14" fill="{}">{}</text>"#,
        x + PANEL_PADDING,
        y + PALETTE_LABEL_Y,
        css_color(muted_text),
        escape_xml("palette")
    ));

    let row_start_y = y + PALETTE_ROWS_Y;
    for (index, color) in colors.iter().copied().enumerate() {
        let row_y = row_start_y + index as f32 * ROW_HEIGHT;
        svg.push_str(&format!(
            r#"<text x="{}" y="{}" font-family="{FONT_FAMILY}" font-size="14" fill="{}">{:>2}.</text>"#,
            x + PANEL_PADDING,
            row_y,
            css_color(muted_text),
            index + 1
        ));
        svg.push_str(&format!(
            r#"<rect x="{}" y="{}" width="18" height="18" rx="4" fill="{}"/>"#,
            x + PANEL_PADDING + 28.0,
            row_y - 14.0,
            css_color(color)
        ));
        svg.push_str(&render_svg_text_line(
            x + PANEL_PADDING + 56.0,
            row_y,
            "[ ]",
            editor_foreground,
            Some(colors),
            14.0,
        ));
        svg.push_str(&format!(
            r#"<text x="{}" y="{}" font-family="{FONT_FAMILY}" font-size="14" fill="{}">{}</text>"#,
            x + PANEL_PADDING + 96.0,
            row_y,
            css_color(editor_foreground),
            escape_xml(&format_color(color))
        ));
    }

    svg
}

fn render_svg_demo_box(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    background: Hsla,
    default_color: Hsla,
    border_color: Hsla,
    lines: &[SvgLine<'_>],
    palette: &[Hsla],
) -> String {
    let mut svg = String::new();
    let clip_id = format!("clip-{}-{}", x.round() as i32, y.round() as i32);
    svg.push_str(&format!(
        r#"<rect x="{x}" y="{y}" width="{width}" height="{height}" rx="12" fill="{}" stroke="{}" stroke-width="1"/>"#,
        css_color(background),
        css_color(border_color)
    ));
    svg.push_str(&format!(
        r#"<defs><clipPath id="{clip_id}"><rect x="{}" y="{}" width="{}" height="{}" rx="10"/></clipPath></defs>"#,
        x + 12.0,
        y + 12.0,
        width - 24.0,
        height - 24.0
    ));
    svg.push_str(&format!(r#"<g clip-path="url(#{clip_id})">"#));

    let mut line_y = y + 28.0;
    let max_characters_per_line = ((width - 32.0) / CODE_CHARACTER_WIDTH).floor() as usize;
    for line in lines {
        match line {
            SvgLine::BracketRun(colors) => {
                svg.push_str(&render_svg_text_line(
                    x + 16.0,
                    line_y,
                    &bracket_run_text(colors.len()),
                    default_color,
                    Some(colors),
                    BRACKET_DEMO_FONT_SIZE,
                ));
                line_y += CODE_ROW_HEIGHT;
            }
            SvgLine::Depths(depth_count) => {
                let depths = (1..=*depth_count)
                    .chain((1..=*depth_count).rev())
                    .collect::<Vec<_>>();
                for (index, depth) in depths.into_iter().enumerate() {
                    let depth_x = x + 16.0 + index as f32 * BRACKET_CHARACTER_WIDTH;
                    svg.push_str(&format!(
                        r#"<text x="{depth_x}" y="{line_y}" font-family="{FONT_FAMILY}" font-size="13" fill="{}" text-anchor="middle">{depth}</text>"#,
                        css_color(default_color)
                    ));
                }
                line_y += CODE_ROW_HEIGHT;
            }
            SvgLine::Code(content) => {
                for wrapped_line in wrap_code_line(content, max_characters_per_line.max(1)) {
                    svg.push_str(&render_svg_text_line(
                        x + 16.0,
                        line_y,
                        &wrapped_line,
                        default_color,
                        Some(palette),
                        CODE_FONT_SIZE,
                    ));
                    line_y += CODE_ROW_HEIGHT;
                }
            }
        }
    }

    svg.push_str("</g>");
    svg
}

enum SvgLine<'a> {
    BracketRun(&'a [Hsla]),
    Depths(usize),
    Code(&'a str),
}

fn render_svg_text_line(
    x: f32,
    y: f32,
    text: &str,
    default_color: Hsla,
    palette_override: Option<&[Hsla]>,
    font_size: f32,
) -> String {
    let runs = colorize_demo_text(text, palette_override.unwrap_or(&[]), default_color);
    let mut svg = String::new();
    svg.push_str(&format!(
        r#"<text x="{x}" y="{y}" font-family="{FONT_FAMILY}" font-size="{font_size}" xml:space="preserve">"#
    ));

    for run in runs {
        svg.push_str(&format!(
            r#"<tspan fill="{}">{}</tspan>"#,
            css_color(run.color),
            escape_xml(&run.text)
        ));
    }

    svg.push_str("</text>");
    svg
}

struct ColoredRun {
    text: String,
    color: Hsla,
}

fn colorize_demo_text(text: &str, palette: &[Hsla], default_color: Hsla) -> Vec<ColoredRun> {
    if palette.is_empty() {
        return vec![ColoredRun {
            text: text.to_string(),
            color: default_color,
        }];
    }

    let mut runs = Vec::new();
    let mut current_text = String::new();
    let mut current_color = default_color;
    let mut stack = Vec::new();

    for character in text.chars() {
        let color = match character {
            '(' | '[' | '{' => {
                let depth = stack.len();
                stack.push(character);
                palette[depth % palette.len()]
            }
            ')' | ']' | '}' => {
                let depth = stack.len().saturating_sub(1);
                if !stack.is_empty() {
                    stack.pop();
                }
                palette[depth % palette.len()]
            }
            _ => default_color,
        };

        if current_text.is_empty() || colors_match(current_color, color) {
            current_text.push(character);
            current_color = color;
        } else {
            runs.push(ColoredRun {
                text: current_text,
                color: current_color,
            });
            current_text = character.to_string();
            current_color = color;
        }
    }

    if !current_text.is_empty() {
        runs.push(ColoredRun {
            text: current_text,
            color: current_color,
        });
    }

    runs
}

fn bracket_run_text(depth_count: usize) -> String {
    "[".repeat(depth_count) + &"]".repeat(depth_count)
}

fn code_demo_lines() -> Vec<SvgLine<'static>> {
    vec![
        SvgLine::Code("fn main() {"),
        SvgLine::Code("    // Direct adjacent brackets"),
        SvgLine::Code("    [[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]"),
        SvgLine::Code(""),
        SvgLine::Code("    // Mixed delimiters at nearby depths"),
        SvgLine::Code("    let _ = vec![(Some([1, 2, 3]), Ok::<_, ()>({ [4, 5, 6] }))];"),
        SvgLine::Code(""),
        SvgLine::Code("    // Multiline nesting"),
        SvgLine::Code("    let _value = foo("),
        SvgLine::Code("        bar("),
        SvgLine::Code("            baz(["),
        SvgLine::Code("                alpha({ beta(gamma([delta(), epsilon()])) }),"),
        SvgLine::Code("                zeta([eta({ theta(iota()) })]),"),
        SvgLine::Code("            ]),"),
        SvgLine::Code("        ),"),
        SvgLine::Code("    );"),
        SvgLine::Code(""),
        SvgLine::Code("    if true {"),
        SvgLine::Code("        while let Some(value) = maybe_call(vec![Some((1, [2, 3, 4]))]) {"),
        SvgLine::Code("            println!(\"{value:?}\");"),
        SvgLine::Code("        }"),
        SvgLine::Code("    }"),
        SvgLine::Code("}"),
    ]
}

fn wrap_code_line(content: &str, max_characters_per_line: usize) -> Vec<String> {
    if content.chars().count() <= max_characters_per_line {
        return vec![content.to_string()];
    }

    let indentation = content
        .chars()
        .take_while(|character| *character == ' ')
        .collect::<String>();
    let continuation_indent = format!("{indentation}    ");

    let mut remaining = content.to_string();
    let mut wrapped_lines = Vec::new();
    let mut is_first_line = true;

    while remaining.chars().count() > max_characters_per_line {
        let available_characters = if is_first_line {
            max_characters_per_line
        } else {
            max_characters_per_line.saturating_sub(continuation_indent.chars().count())
        };

        let split_at = split_index_for_wrap(&remaining, available_characters.max(1));
        let (current_line, rest) = remaining.split_at(split_at);
        if is_first_line {
            wrapped_lines.push(current_line.to_string());
        } else {
            wrapped_lines.push(format!("{continuation_indent}{current_line}"));
        }

        remaining = rest.trim_start().to_string();
        is_first_line = false;
    }

    if !remaining.is_empty() {
        if is_first_line {
            wrapped_lines.push(remaining);
        } else {
            wrapped_lines.push(format!("{continuation_indent}{remaining}"));
        }
    }

    if wrapped_lines.is_empty() {
        vec![String::new()]
    } else {
        wrapped_lines
    }
}

fn split_index_for_wrap(content: &str, max_characters: usize) -> usize {
    let mut last_boundary = 0;
    let mut last_preferred_boundary = None;

    for (character_index, (byte_index, character)) in content.char_indices().enumerate() {
        if character_index >= max_characters {
            break;
        }

        last_boundary = byte_index + character.len_utf8();
        if matches!(character, ' ' | ',' | ')' | ']' | '}') {
            last_preferred_boundary = Some(last_boundary);
        }
    }

    last_preferred_boundary.unwrap_or(last_boundary.max(1))
}

fn colors_match(left: Hsla, right: Hsla) -> bool {
    let left = Rgba::from(left);
    let right = Rgba::from(right);
    (left.r - right.r).abs() < 0.0001
        && (left.g - right.g).abs() < 0.0001
        && (left.b - right.b).abs() < 0.0001
        && (left.a - right.a).abs() < 0.0001
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
        BracketColorizationPaletteStrategy::AdjustedBackgroundPalette => {
            "adjusted only the background-failing accents"
        }
        BracketColorizationPaletteStrategy::ReorderedWeakPalette => "reordered weak palette",
        BracketColorizationPaletteStrategy::AdjustedBackgroundAndReorderedPalette => {
            "adjusted background-failing accents, then reordered"
        }
    }
}

fn strategy_reordered(strategy: BracketColorizationPaletteStrategy) -> bool {
    matches!(
        strategy,
        BracketColorizationPaletteStrategy::ReorderedWeakPalette
            | BracketColorizationPaletteStrategy::AdjustedBackgroundAndReorderedPalette
    )
}

fn strategy_adjusted_background(strategy: BracketColorizationPaletteStrategy) -> bool {
    matches!(
        strategy,
        BracketColorizationPaletteStrategy::AdjustedBackgroundPalette
            | BracketColorizationPaletteStrategy::AdjustedBackgroundAndReorderedPalette
    )
}

fn threshold_result(value: f32, threshold: Option<f32>) -> &'static str {
    match threshold {
        Some(threshold) if value >= threshold => "pass",
        Some(_) => "fail",
        None => "n/a",
    }
}

fn format_order_mapping(original: &[Hsla], final_palette: &[Hsla]) -> String {
    let mapping = final_palette
        .iter()
        .copied()
        .map(|color| {
            original
                .iter()
                .position(|candidate| *candidate == color)
                .map(|index| (index + 1).to_string())
                .unwrap_or_else(|| "?".to_string())
        })
        .collect::<Vec<_>>();
    format!("[{}]", mapping.join(", "))
}

fn format_bracket_demo(colors: &[Hsla], background: Hsla, use_color: bool) -> String {
    let left_brackets = colors
        .iter()
        .copied()
        .map(|color| tint("[", color, background, use_color));
    let right_brackets = colors
        .iter()
        .copied()
        .rev()
        .map(|color| tint("]", color, background, use_color));

    left_brackets
        .chain(right_brackets)
        .collect::<Vec<_>>()
        .join(&background_fill(" ", background, use_color))
}

fn format_depth_demo(depth_count: usize) -> String {
    let left_depths = (1..=depth_count).map(|depth| format!("{depth:>2}"));
    let right_depths = (1..=depth_count).rev().map(|depth| format!("{depth:>2}"));

    left_depths
        .chain(right_depths)
        .collect::<Vec<_>>()
        .join(" ")
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

fn css_color(color: Hsla) -> String {
    let rgba = Rgba::from(color);
    let red = to_u8(rgba.r);
    let green = to_u8(rgba.g);
    let blue = to_u8(rgba.b);
    if rgba.a >= 0.999 {
        format!("#{red:02X}{green:02X}{blue:02X}")
    } else {
        format!("rgba({red},{green},{blue},{:.3})", rgba.a)
    }
}

fn swatch(color: Hsla, use_color: bool) -> String {
    if !use_color {
        return "[ ]".to_string();
    }

    let (red, green, blue) = rgb_channels(color);
    format!("\x1b[48;2;{red};{green};{blue}m   \x1b[0m")
}

fn bracket_sample(color: Hsla, background: Hsla, use_color: bool) -> String {
    if !use_color {
        return "[ ]".to_string();
    }

    let (foreground_red, foreground_green, foreground_blue) = rgb_channels(color);
    let (background_red, background_green, background_blue) = rgb_channels(background);
    format!(
        "\x1b[48;2;{background_red};{background_green};{background_blue}m\x1b[38;2;{foreground_red};{foreground_green};{foreground_blue}m[ ]\x1b[0m"
    )
}

fn tint(text: &str, color: Hsla, background: Hsla, use_color: bool) -> String {
    if !use_color {
        return text.to_string();
    }

    let (foreground_red, foreground_green, foreground_blue) = rgb_channels(color);
    let (background_red, background_green, background_blue) = rgb_channels(background);
    format!(
        "\x1b[48;2;{background_red};{background_green};{background_blue}m\x1b[38;2;{foreground_red};{foreground_green};{foreground_blue}m{text}\x1b[0m"
    )
}

fn background_fill(text: &str, background: Hsla, use_color: bool) -> String {
    if !use_color {
        return text.to_string();
    }

    let (background_red, background_green, background_blue) = rgb_channels(background);
    format!("\x1b[48;2;{background_red};{background_green};{background_blue}m{text}\x1b[0m")
}

fn adjacent_pairs(colors: &[Hsla]) -> Vec<(usize, usize, f32)> {
    if colors.len() < 2 {
        return Vec::new();
    }

    (0..colors.len())
        .map(|index| {
            let next = (index + 1) % colors.len();
            (index, next, adjacent_distance(colors[index], colors[next]))
        })
        .collect()
}

fn weakest_pair(pairs: &[(usize, usize, f32)]) -> Option<(usize, usize, f32)> {
    pairs
        .iter()
        .copied()
        .min_by(|left, right| left.2.total_cmp(&right.2))
}

fn adjacent_distance(left: Hsla, right: Hsla) -> f32 {
    let left = Rgba::from(left);
    let right = Rgba::from(right);
    let red_delta = left.r - right.r;
    let green_delta = left.g - right.g;
    let blue_delta = left.b - right.b;
    (red_delta * red_delta + green_delta * green_delta + blue_delta * blue_delta).sqrt()
}

fn rgb_channels(color: Hsla) -> (u8, u8, u8) {
    let rgba = Rgba::from(color);
    (to_u8(rgba.r), to_u8(rgba.g), to_u8(rgba.b))
}

fn to_u8(channel: f32) -> u8 {
    (channel.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn slugify(input: &str) -> String {
    let mut slug = String::new();
    let mut previous_was_dash = false;

    for character in input.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            previous_was_dash = false;
        } else if !previous_was_dash {
            slug.push('-');
            previous_was_dash = true;
        }
    }

    slug.trim_matches('-').to_string()
}

fn escape_xml(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
