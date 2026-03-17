#![allow(clippy::disallowed_methods)]

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("zed_bracket_contrast_capture is only supported on macOS");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
use {
    anyhow::{Context as _, Result, bail},
    assets::Assets,
    editor::{
        AutoBracketColorizationConfig, BracketColorizationPaletteAnalysis,
        BracketColorizationPaletteStrategy, Editor, EditorMode,
        analyze_bracket_colorization_palette_with_config,
    },
    fs::{Fs, RealFs},
    gpui::{
        AppContext as _, Hsla, Rgba as GpuiRgba, UpdateGlobal as _, VisualTestAppContext, px, size,
    },
    image::{
        Rgba, RgbaImage,
        imageops::{crop_imm, overlay},
    },
    language::{Buffer, language_settings::BracketColorizationMode, rust_lang},
    multi_buffer::MultiBuffer,
    settings::{AccentContent, SettingsStore},
    std::{
        collections::HashMap,
        env, fs as std_fs,
        path::{Path, PathBuf},
        sync::Arc,
        time::Duration,
    },
    theme::{
        ActiveTheme, Appearance, GlobalTheme, SystemAppearance, ThemeFamilyContent, ThemeRegistry,
        ThemeStyleContent, refine_theme_family, set_theme,
    },
    ui::utils::ensure_minimum_contrast,
};

#[cfg(target_os = "macos")]
const WINDOW_WIDTH: f32 = 900.0;
#[cfg(target_os = "macos")]
const WINDOW_HEIGHT: f32 = 660.0;
#[cfg(target_os = "macos")]
const IMAGE_GAP: u32 = 24;
#[cfg(target_os = "macos")]
const UPSTREAM_MINIMUM_APCA_CONTRAST: f32 = 55.0;
#[cfg(target_os = "macos")]
const TRIM_PADDING: u32 = 24;
#[cfg(target_os = "macos")]
const MIN_CONTENT_PIXELS_PER_ROW: u32 = 8;
#[cfg(target_os = "macos")]
const MIN_CONTENT_PIXELS_PER_COLUMN: u32 = 8;
#[cfg(target_os = "macos")]
const COLUMN_DENSITY_WINDOW: usize = 8;

#[cfg(target_os = "macos")]
fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "macos")]
fn run() -> Result<()> {
    let options = Options::parse()?;

    let mut cx = VisualTestAppContext::with_asset_source(
        gpui_platform::current_platform(false),
        Arc::new(Assets),
    );
    let file_system = initialize_capture_context(&mut cx)?;

    for (path_index, theme_path) in options.theme_paths.iter().enumerate() {
        if path_index > 0 {
            println!();
        }

        capture_theme_file(
            theme_path,
            options.output_directory.as_deref(),
            options.upstream_apca_threshold,
            &options.stacked_upstream_apca_thresholds,
            options.auto_config,
            &options.stacked_auto_background_apca_thresholds,
            &options.stacked_auto_adjacent_oklab_thresholds,
            file_system.clone(),
            &mut cx,
        )?;
    }

    Ok(())
}

#[cfg(target_os = "macos")]
struct Options {
    theme_paths: Vec<PathBuf>,
    output_directory: Option<PathBuf>,
    upstream_apca_threshold: f32,
    stacked_upstream_apca_thresholds: Vec<f32>,
    auto_config: AutoBracketColorizationConfig,
    stacked_auto_background_apca_thresholds: Vec<f32>,
    stacked_auto_adjacent_oklab_thresholds: Vec<f32>,
}

#[cfg(target_os = "macos")]
impl Options {
    fn parse() -> Result<Self> {
        let mut args = env::args_os().skip(1);
        let mut output_directory = None;
        let mut upstream_apca_threshold = UPSTREAM_MINIMUM_APCA_CONTRAST;
        let mut stacked_upstream_apca_thresholds = Vec::new();
        let mut auto_config = AutoBracketColorizationConfig::default();
        let mut stacked_auto_background_apca_thresholds = Vec::new();
        let mut stacked_auto_adjacent_oklab_thresholds = Vec::new();
        let mut theme_paths = Vec::new();

        while let Some(argument) = args.next() {
            if argument == "--output-dir" {
                let Some(directory) = args.next() else {
                    bail!("expected directory after --output-dir");
                };
                output_directory = Some(PathBuf::from(directory));
            } else if argument == "--upstream-apca-threshold" {
                let Some(threshold) = args.next() else {
                    bail!("expected threshold after --upstream-apca-threshold");
                };
                upstream_apca_threshold = threshold
                    .to_string_lossy()
                    .parse()
                    .context("failed to parse --upstream-apca-threshold as a number")?;
            } else if argument == "--stack-upstream-apca-thresholds" {
                let Some(thresholds) = args.next() else {
                    bail!(
                        "expected comma-separated thresholds after --stack-upstream-apca-thresholds"
                    );
                };
                stacked_upstream_apca_thresholds = parse_threshold_list(&thresholds)?;
            } else if argument == "--auto-background-apca-light-threshold" {
                let Some(threshold) = args.next() else {
                    bail!("expected threshold after --auto-background-apca-light-threshold");
                };
                auto_config.minimum_background_apca_light =
                    threshold.to_string_lossy().parse().context(
                        "failed to parse --auto-background-apca-light-threshold as a number",
                    )?;
            } else if argument == "--auto-background-apca-dark-threshold" {
                let Some(threshold) = args.next() else {
                    bail!("expected threshold after --auto-background-apca-dark-threshold");
                };
                auto_config.minimum_background_apca_dark = threshold
                    .to_string_lossy()
                    .parse()
                    .context("failed to parse --auto-background-apca-dark-threshold as a number")?;
            } else if argument == "--auto-adjacent-oklab-light-threshold" {
                let Some(threshold) = args.next() else {
                    bail!("expected threshold after --auto-adjacent-oklab-light-threshold");
                };
                auto_config.minimum_adjacent_oklab_light = threshold
                    .to_string_lossy()
                    .parse()
                    .context("failed to parse --auto-adjacent-oklab-light-threshold as a number")?;
            } else if argument == "--auto-adjacent-oklab-dark-threshold" {
                let Some(threshold) = args.next() else {
                    bail!("expected threshold after --auto-adjacent-oklab-dark-threshold");
                };
                auto_config.minimum_adjacent_oklab_dark = threshold
                    .to_string_lossy()
                    .parse()
                    .context("failed to parse --auto-adjacent-oklab-dark-threshold as a number")?;
            } else if argument == "--stack-auto-background-apca-thresholds" {
                let Some(thresholds) = args.next() else {
                    bail!(
                        "expected comma-separated thresholds after --stack-auto-background-apca-thresholds"
                    );
                };
                stacked_auto_background_apca_thresholds = parse_threshold_list(&thresholds)?;
            } else if argument == "--stack-auto-adjacent-oklab-thresholds" {
                let Some(thresholds) = args.next() else {
                    bail!(
                        "expected comma-separated thresholds after --stack-auto-adjacent-oklab-thresholds"
                    );
                };
                stacked_auto_adjacent_oklab_thresholds = parse_threshold_list(&thresholds)?;
            } else {
                theme_paths.push(PathBuf::from(argument));
            }
        }

        if theme_paths.is_empty() {
            bail!(
                "usage: cargo run -p zed --features visual-tests --bin zed_bracket_contrast_capture -- [--output-dir <dir>] [--upstream-apca-threshold <value>] [--stack-upstream-apca-thresholds <comma-separated-values>] [--auto-background-apca-light-threshold <value>] [--auto-background-apca-dark-threshold <value>] [--auto-adjacent-oklab-light-threshold <value>] [--auto-adjacent-oklab-dark-threshold <value>] [--stack-auto-background-apca-thresholds <comma-separated-values>] [--stack-auto-adjacent-oklab-thresholds <comma-separated-values>] <theme.json> [theme.json ...]"
            );
        }

        Ok(Self {
            theme_paths,
            output_directory,
            upstream_apca_threshold,
            stacked_upstream_apca_thresholds,
            auto_config,
            stacked_auto_background_apca_thresholds,
            stacked_auto_adjacent_oklab_thresholds,
        })
    }
}

#[cfg(target_os = "macos")]
fn parse_threshold_list(raw_thresholds: &std::ffi::OsStr) -> Result<Vec<f32>> {
    let raw_thresholds = raw_thresholds.to_string_lossy();
    raw_thresholds
        .split(',')
        .map(str::trim)
        .filter(|threshold| !threshold.is_empty())
        .map(|threshold| {
            threshold
                .parse()
                .with_context(|| format!("failed to parse APCA threshold `{threshold}`"))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn initialize_capture_context(cx: &mut VisualTestAppContext) -> Result<Arc<dyn Fs>> {
    cx.update(|cx| -> Result<Arc<dyn Fs>> {
        Assets
            .load_fonts(cx)
            .context("failed to load embedded fonts for visual capture")?;

        settings::init(cx);

        let file_system: Arc<dyn Fs> =
            Arc::new(RealFs::new(None, cx.background_executor().clone()));
        <dyn Fs>::set_global(file_system.clone(), cx);

        theme::init(theme::LoadThemes::JustBase, cx);
        editor::init(cx);

        Ok(file_system)
    })
}

#[cfg(target_os = "macos")]
fn capture_theme_file(
    theme_path: &Path,
    output_directory_override: Option<&Path>,
    upstream_apca_threshold: f32,
    stacked_upstream_apca_thresholds: &[f32],
    auto_config: AutoBracketColorizationConfig,
    stacked_auto_background_apca_thresholds: &[f32],
    stacked_auto_adjacent_oklab_thresholds: &[f32],
    file_system: Arc<dyn Fs>,
    cx: &mut VisualTestAppContext,
) -> Result<()> {
    let output_directory = output_directory_override
        .map(Path::to_path_buf)
        .or_else(|| theme_path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    std_fs::create_dir_all(&output_directory).with_context(|| {
        format!(
            "failed to create output directory {}",
            output_directory.display()
        )
    })?;

    let bytes = std_fs::read(theme_path)
        .with_context(|| format!("failed to read {}", theme_path.display()))?;
    let theme_family_content: ThemeFamilyContent = serde_json_lenient::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", theme_path.display()))?;

    let theme_family_name = theme_family_content.name.clone();
    let refined_theme_family = refine_theme_family(theme_family_content);
    let theme_variants = refined_theme_family
        .themes
        .iter()
        .map(|theme| ThemeVariantCapture {
            name: theme.name.to_string(),
            appearance: theme.appearance,
            editor_background: theme.colors().editor_background,
            raw_accents: theme.accents().0.iter().copied().collect(),
            upstream_adjusted_accents: upstream_adjusted_accents(
                theme.accents().0.as_ref(),
                theme.colors().editor_background,
                upstream_apca_threshold,
            ),
            auto_adjusted_accents: analysis_palette_overrides(
                analyze_bracket_colorization_palette_with_config(
                    theme.accents().0.as_ref(),
                    theme.appearance,
                    theme.colors().editor_background,
                    BracketColorizationMode::Auto,
                    auto_config,
                )
                .final_palette
                .as_ref(),
            ),
            analysis: analyze_bracket_colorization_palette_with_config(
                theme.accents().0.as_ref(),
                theme.appearance,
                theme.colors().editor_background,
                BracketColorizationMode::Auto,
                auto_config,
            ),
        })
        .collect::<Vec<_>>();

    println!("Theme file: {}", theme_path.display());
    println!("Family: {}", theme_family_name);
    println!("Output: {}", output_directory.display());

    let theme_registry = cx.update(|cx| ThemeRegistry::global(cx));
    cx.foreground_executor()
        .block_test(theme_registry.load_user_theme(theme_path, file_system))
        .with_context(|| {
            format!(
                "failed to load {} into the theme registry",
                theme_path.display()
            )
        })?;

    let mut capture_results = Vec::with_capacity(theme_variants.len());
    for variant in theme_variants {
        let theme_name = &variant.name;
        println!();
        println!("Theme: {theme_name}");

        let raw_theme_path = output_directory.join(format!(
            "{}-{}-theme.png",
            slugify(&theme_family_name),
            slugify(&theme_name)
        ));
        let upstream_path = output_directory.join(format!(
            "{}-{}-upstream.png",
            slugify(&theme_family_name),
            slugify(&theme_name)
        ));
        let auto_mode_path = output_directory.join(format!(
            "{}-{}-auto.png",
            slugify(&theme_family_name),
            slugify(&theme_name)
        ));
        let comparison_path = output_directory.join(format!(
            "{}-{}-comparison.png",
            slugify(&theme_family_name),
            slugify(&theme_name)
        ));
        let upstream_threshold_stack_path =
            (!stacked_upstream_apca_thresholds.is_empty()).then(|| {
                output_directory.join(format!(
                    "{}-{}-upstream-apca-thresholds.png",
                    slugify(&theme_family_name),
                    slugify(&theme_name)
                ))
            });
        let auto_background_threshold_stack_path =
            (!stacked_auto_background_apca_thresholds.is_empty()).then(|| {
                output_directory.join(format!(
                    "{}-{}-auto-background-apca-thresholds.png",
                    slugify(&theme_family_name),
                    slugify(&theme_name)
                ))
            });
        let auto_adjacent_threshold_stack_path =
            (!stacked_auto_adjacent_oklab_thresholds.is_empty()).then(|| {
                output_directory.join(format!(
                    "{}-{}-auto-adjacent-oklab-thresholds.png",
                    slugify(&theme_family_name),
                    slugify(&theme_name)
                ))
            });

        let raw_theme_screenshot = capture_editor_mode(
            cx,
            theme_name,
            variant.appearance,
            BracketColorizationMode::Theme,
            &demo_source("Raw Theme Order - unchanged"),
            None,
        )
        .with_context(|| format!("failed to capture theme-mode screenshot for {theme_name}"))?;
        raw_theme_screenshot
            .save(&raw_theme_path)
            .with_context(|| format!("failed to save {}", raw_theme_path.display()))?;

        let upstream_screenshot = capture_editor_mode(
            cx,
            theme_name,
            variant.appearance,
            BracketColorizationMode::Theme,
            &demo_source(&upstream_behavior_label(upstream_apca_threshold)),
            Some(&variant.upstream_adjusted_accents),
        )
        .with_context(|| format!("failed to capture upstream screenshot for {theme_name}"))?;
        upstream_screenshot
            .save(&upstream_path)
            .with_context(|| format!("failed to save {}", upstream_path.display()))?;

        let auto_screenshot = capture_editor_mode(
            cx,
            theme_name,
            variant.appearance,
            BracketColorizationMode::Theme,
            &demo_source(&auto_behavior_label(
                variant.analysis.strategy,
                variant.analysis.background_threshold,
                variant.analysis.adjacent_threshold,
            )),
            Some(&variant.auto_adjusted_accents),
        )
        .with_context(|| format!("failed to capture auto-mode screenshot for {theme_name}"))?;
        auto_screenshot
            .save(&auto_mode_path)
            .with_context(|| format!("failed to save {}", auto_mode_path.display()))?;

        let comparison_image = stitch_images_horizontally(&[
            &raw_theme_screenshot,
            &upstream_screenshot,
            &auto_screenshot,
        ]);
        comparison_image
            .save(&comparison_path)
            .with_context(|| format!("failed to save {}", comparison_path.display()))?;

        if let Some(upstream_threshold_stack_path) = &upstream_threshold_stack_path {
            let mut threshold_images = vec![raw_theme_screenshot.clone()];
            for threshold in stacked_upstream_apca_thresholds {
                let threshold_accents = upstream_adjusted_accents(
                    &variant.raw_accents,
                    variant.editor_background,
                    *threshold,
                );
                let threshold_screenshot = capture_editor_mode(
                    cx,
                    theme_name,
                    variant.appearance,
                    BracketColorizationMode::Theme,
                    &demo_source(&upstream_behavior_label(*threshold)),
                    Some(&threshold_accents),
                )
                .with_context(|| {
                    format!(
                        "failed to capture APCA-threshold comparison screenshot for {theme_name} at {threshold:.1}"
                    )
                })?;
                threshold_images.push(threshold_screenshot);
            }

            let threshold_image_refs = threshold_images.iter().collect::<Vec<_>>();
            let threshold_stack = stitch_images_horizontally(&threshold_image_refs);
            threshold_stack
                .save(upstream_threshold_stack_path)
                .with_context(|| {
                    format!("failed to save {}", upstream_threshold_stack_path.display())
                })?;
        }

        if let Some(auto_background_threshold_stack_path) = &auto_background_threshold_stack_path {
            let mut threshold_images = vec![raw_theme_screenshot.clone()];
            for threshold in stacked_auto_background_apca_thresholds {
                let threshold_config = AutoBracketColorizationConfig {
                    minimum_background_apca_light: *threshold,
                    minimum_background_apca_dark: *threshold,
                    ..auto_config
                };
                let threshold_analysis = analyze_bracket_colorization_palette_with_config(
                    &variant.raw_accents,
                    variant.appearance,
                    variant.editor_background,
                    BracketColorizationMode::Auto,
                    threshold_config,
                );
                let threshold_screenshot = capture_editor_mode(
                    cx,
                    theme_name,
                    variant.appearance,
                    BracketColorizationMode::Theme,
                    &demo_source(&auto_behavior_label(
                        threshold_analysis.strategy,
                        threshold_analysis.background_threshold,
                        threshold_analysis.adjacent_threshold,
                    )),
                    Some(&analysis_palette_overrides(
                        threshold_analysis.final_palette.as_ref(),
                    )),
                )
                .with_context(|| {
                    format!(
                        "failed to capture auto background-threshold comparison for {theme_name} at {threshold:.1}"
                    )
                })?;
                threshold_images.push(threshold_screenshot);
            }

            let threshold_image_refs = threshold_images.iter().collect::<Vec<_>>();
            stitch_images_horizontally(&threshold_image_refs)
                .save(auto_background_threshold_stack_path)
                .with_context(|| {
                    format!(
                        "failed to save {}",
                        auto_background_threshold_stack_path.display()
                    )
                })?;
        }

        if let Some(auto_adjacent_threshold_stack_path) = &auto_adjacent_threshold_stack_path {
            let mut threshold_images = vec![raw_theme_screenshot.clone()];
            for threshold in stacked_auto_adjacent_oklab_thresholds {
                let threshold_config = AutoBracketColorizationConfig {
                    minimum_adjacent_oklab_light: *threshold,
                    minimum_adjacent_oklab_dark: *threshold,
                    ..auto_config
                };
                let threshold_analysis = analyze_bracket_colorization_palette_with_config(
                    &variant.raw_accents,
                    variant.appearance,
                    variant.editor_background,
                    BracketColorizationMode::Auto,
                    threshold_config,
                );
                let threshold_screenshot = capture_editor_mode(
                    cx,
                    theme_name,
                    variant.appearance,
                    BracketColorizationMode::Theme,
                    &demo_source(&auto_behavior_label(
                        threshold_analysis.strategy,
                        threshold_analysis.background_threshold,
                        threshold_analysis.adjacent_threshold,
                    )),
                    Some(&analysis_palette_overrides(
                        threshold_analysis.final_palette.as_ref(),
                    )),
                )
                .with_context(|| {
                    format!(
                        "failed to capture auto adjacent-threshold comparison for {theme_name} at {threshold:.2}"
                    )
                })?;
                threshold_images.push(threshold_screenshot);
            }

            let threshold_image_refs = threshold_images.iter().collect::<Vec<_>>();
            stitch_images_horizontally(&threshold_image_refs)
                .save(auto_adjacent_threshold_stack_path)
                .with_context(|| {
                    format!(
                        "failed to save {}",
                        auto_adjacent_threshold_stack_path.display()
                    )
                })?;
        }

        println!("theme  : {}", raw_theme_path.display());
        println!("upstream: {}", upstream_path.display());
        println!("auto   : {}", auto_mode_path.display());
        println!("compare: {}", comparison_path.display());
        if let Some(upstream_threshold_stack_path) = &upstream_threshold_stack_path {
            println!("apca   : {}", upstream_threshold_stack_path.display());
        }
        if let Some(auto_background_threshold_stack_path) = &auto_background_threshold_stack_path {
            println!(
                "auto-bg: {}",
                auto_background_threshold_stack_path.display()
            );
        }
        if let Some(auto_adjacent_threshold_stack_path) = &auto_adjacent_threshold_stack_path {
            println!("auto-adj: {}", auto_adjacent_threshold_stack_path.display());
        }

        capture_results.push(ThemeCaptureResult {
            name: theme_name.clone(),
            appearance: variant.appearance,
            editor_background: variant.editor_background,
            analysis: variant.analysis,
            raw_theme_path,
            upstream_path,
            auto_mode_path,
            comparison_path,
            upstream_threshold_stack_path,
            auto_background_threshold_stack_path,
            auto_adjacent_threshold_stack_path,
        });
    }

    let report_path = output_directory.join(format!(
        "{}-bracket-contrast-report.md",
        slugify(&theme_family_name)
    ));
    let report = format_capture_report(
        theme_path,
        &theme_family_name,
        &capture_results,
        output_directory.as_path(),
        upstream_apca_threshold,
        auto_config,
    );
    std_fs::write(&report_path, report)
        .with_context(|| format!("failed to write {}", report_path.display()))?;
    println!();
    println!("report : {}", report_path.display());

    Ok(())
}

#[cfg(target_os = "macos")]
struct ThemeVariantCapture {
    name: String,
    appearance: Appearance,
    editor_background: Hsla,
    raw_accents: Vec<Hsla>,
    upstream_adjusted_accents: Vec<String>,
    auto_adjusted_accents: Vec<String>,
    analysis: BracketColorizationPaletteAnalysis,
}

#[cfg(target_os = "macos")]
struct ThemeCaptureResult {
    name: String,
    appearance: Appearance,
    editor_background: Hsla,
    analysis: BracketColorizationPaletteAnalysis,
    raw_theme_path: PathBuf,
    upstream_path: PathBuf,
    auto_mode_path: PathBuf,
    comparison_path: PathBuf,
    upstream_threshold_stack_path: Option<PathBuf>,
    auto_background_threshold_stack_path: Option<PathBuf>,
    auto_adjacent_threshold_stack_path: Option<PathBuf>,
}

#[cfg(target_os = "macos")]
fn capture_editor_mode(
    cx: &mut VisualTestAppContext,
    theme_name: &str,
    appearance: Appearance,
    bracket_colorization_mode: BracketColorizationMode,
    text: &str,
    accent_overrides: Option<&[String]>,
) -> Result<RgbaImage> {
    apply_theme_settings(
        cx,
        theme_name,
        appearance,
        bracket_colorization_mode,
        accent_overrides,
    )?;

    let window =
        cx.open_offscreen_window(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), |window, cx| {
            let language = rust_lang();
            language.set_theme(cx.theme().syntax());
            let buffer = cx.new(|cx| Buffer::local(text, cx).with_language(language, cx));
            let multi_buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
            cx.new(|cx| {
                let mut editor = Editor::new(EditorMode::full(), multi_buffer, None, window, cx);
                editor.set_read_only(true);
                editor.disable_scrollbars_and_minimap(window, cx);
                editor
            })
        })?;

    wait_for_ui_stabilization(cx);
    let any_window = window.into();
    let screenshot = cx
        .capture_screenshot(any_window)
        .with_context(|| format!("failed to capture screenshot for {theme_name}"))?;
    cx.update_window(any_window, |_, window, _| {
        window.remove_window();
    })?;
    wait_for_ui_stabilization(cx);
    Ok(trim_screenshot(&screenshot, TRIM_PADDING))
}

#[cfg(target_os = "macos")]
fn apply_theme_settings(
    cx: &mut VisualTestAppContext,
    theme_name: &str,
    appearance: Appearance,
    bracket_colorization_mode: BracketColorizationMode,
    accent_overrides: Option<&[String]>,
) -> Result<()> {
    cx.update(|cx| {
        *SystemAppearance::global_mut(cx) = SystemAppearance(appearance);

        SettingsStore::update_global(cx, |store: &mut SettingsStore, cx| {
            store.update_user_settings(cx, |settings| {
                set_theme(settings, theme_name, appearance, appearance);
                settings.project.all_languages.defaults.colorize_brackets = Some(true);
                settings
                    .project
                    .all_languages
                    .defaults
                    .bracket_colorization_mode = Some(bracket_colorization_mode);
                settings.theme.theme_overrides = accent_overrides
                    .map(|accent_overrides| {
                        HashMap::from_iter([(
                            theme_name.to_string(),
                            ThemeStyleContent {
                                accents: accent_overrides
                                    .iter()
                                    .cloned()
                                    .map(|accent| AccentContent(Some(accent)))
                                    .collect(),
                                ..ThemeStyleContent::default()
                            },
                        )])
                    })
                    .unwrap_or_default();
            });
        });

        GlobalTheme::reload_theme(cx);
    });

    wait_for_ui_stabilization(cx);
    Ok(())
}

#[cfg(target_os = "macos")]
fn wait_for_ui_stabilization(cx: &mut VisualTestAppContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.background_executor.run_until_parked();
        cx.run_until_parked();
        cx.advance_clock(Duration::from_millis(200));
    }
    cx.run_until_parked();
    cx.background_executor.run_until_parked();
    cx.run_until_parked();
}

#[cfg(target_os = "macos")]
fn stitch_images_horizontally(images: &[&RgbaImage]) -> RgbaImage {
    let Some(first_image) = images.first() else {
        return RgbaImage::default();
    };
    let background = *first_image.get_pixel(0, 0);
    let total_width = images.iter().map(|image| image.width()).sum::<u32>()
        + IMAGE_GAP * images.len().saturating_sub(1) as u32;
    let max_height = images.iter().map(|image| image.height()).max().unwrap_or(0);
    let mut stitched = RgbaImage::from_pixel(total_width, max_height, background);

    let mut current_x = 0_i64;
    for (image_index, image) in images.iter().enumerate() {
        if image_index > 0 {
            current_x += i64::from(IMAGE_GAP);
        }
        overlay(&mut stitched, *image, current_x, 0);
        current_x += i64::from(image.width());
    }

    stitched
}

#[cfg(target_os = "macos")]
fn trim_screenshot(screenshot: &RgbaImage, padding: u32) -> RgbaImage {
    let background = sample_background_color(screenshot);
    let Some((left, top, right, bottom)) = content_bounds(screenshot, background) else {
        return screenshot.clone();
    };

    let left = left.saturating_sub(padding);
    let top = top.saturating_sub(padding);
    let right = (right + padding).min(screenshot.width().saturating_sub(1));
    let bottom = (bottom + padding).min(screenshot.height().saturating_sub(1));
    let width = right.saturating_sub(left) + 1;
    let height = bottom.saturating_sub(top) + 1;

    crop_imm(screenshot, left, top, width, height).to_image()
}

#[cfg(target_os = "macos")]
fn content_bounds(screenshot: &RgbaImage, background: Rgba<u8>) -> Option<(u32, u32, u32, u32)> {
    let mut row_counts = vec![0_u32; screenshot.height() as usize];
    let mut column_counts = vec![0_u32; screenshot.width() as usize];

    for (x, y, pixel) in screenshot.enumerate_pixels() {
        if *pixel != background {
            row_counts[y as usize] += 1;
            column_counts[x as usize] += 1;
        }
    }

    let top = row_counts
        .iter()
        .position(|count| *count >= MIN_CONTENT_PIXELS_PER_ROW)?;
    let bottom = row_counts
        .iter()
        .rposition(|count| *count >= MIN_CONTENT_PIXELS_PER_ROW)?;
    let left = dense_column_start(&column_counts)?;
    let right = dense_column_end(&column_counts)?;

    Some((left as u32, top as u32, right as u32, bottom as u32))
}

#[cfg(target_os = "macos")]
fn dense_column_start(column_counts: &[u32]) -> Option<usize> {
    let threshold = MIN_CONTENT_PIXELS_PER_COLUMN * COLUMN_DENSITY_WINDOW as u32;
    column_counts
        .windows(COLUMN_DENSITY_WINDOW)
        .position(|window| window.iter().copied().sum::<u32>() >= threshold)
}

#[cfg(target_os = "macos")]
fn dense_column_end(column_counts: &[u32]) -> Option<usize> {
    let threshold = MIN_CONTENT_PIXELS_PER_COLUMN * COLUMN_DENSITY_WINDOW as u32;
    column_counts
        .windows(COLUMN_DENSITY_WINDOW)
        .rposition(|window| window.iter().copied().sum::<u32>() >= threshold)
        .map(|start| start + COLUMN_DENSITY_WINDOW - 1)
}

#[cfg(target_os = "macos")]
fn sample_background_color(screenshot: &RgbaImage) -> Rgba<u8> {
    let sample_width = screenshot.width().min(64);
    let sample_height = screenshot.height().min(64);
    let start_x = screenshot.width().saturating_sub(sample_width);
    let start_y = screenshot.height().saturating_sub(sample_height);
    let mut color_counts = HashMap::<Rgba<u8>, u32>::new();

    for y in start_y..screenshot.height() {
        for x in start_x..screenshot.width() {
            let color = *screenshot.get_pixel(x, y);
            *color_counts.entry(color).or_default() += 1;
        }
    }

    color_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(color, _)| color)
        .unwrap_or_else(|| *screenshot.get_pixel(screenshot.width() - 1, screenshot.height() - 1))
}

#[cfg(target_os = "macos")]
fn demo_source(label: &str) -> String {
    format!(
        r#"fn main() {{
    // {label}

    // Direct adjacent brackets
    [[[[[[[[[[[[[[[[[[[]]]]]]]]]]]]]]]]]]]

    // Mixed delimiters at nearby depths
    let _ = vec![(Some([1, 2, 3]), Ok::<_, ()>({{ [4, 5, 6] }}))];

    // Multiline nesting
    let _value = foo(
        bar(
            baz([
                alpha({{ beta(gamma([delta(), epsilon()])) }}),
                zeta([eta({{ theta(iota()) }})]),
            ]),
        ),
    );

    if true {{
        while let Some(value) = maybe_call(vec![Some((1, [2, 3, 4]))]) {{
            println!("{{value:?}}");
        }}
    }}
}}
"#
    )
}

#[cfg(target_os = "macos")]
fn upstream_adjusted_accents(
    accents: &[Hsla],
    editor_background: Hsla,
    upstream_apca_threshold: f32,
) -> Vec<String> {
    accents
        .iter()
        .copied()
        .map(|accent| {
            format_color(ensure_minimum_contrast(
                accent,
                editor_background,
                upstream_apca_threshold,
            ))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn analysis_palette_overrides(palette: &[Hsla]) -> Vec<String> {
    palette.iter().copied().map(format_color).collect()
}

#[cfg(target_os = "macos")]
fn auto_behavior_label(
    strategy: BracketColorizationPaletteStrategy,
    background_threshold: Option<f32>,
    adjacent_threshold: Option<f32>,
) -> String {
    format!(
        "Proposed Auto Behavior - {} [APCA {} | OKLab {}]",
        strategy_treatments_label(strategy),
        background_threshold
            .map(|threshold| format!("{threshold:.1}"))
            .unwrap_or_else(|| "n/a".to_string()),
        adjacent_threshold
            .map(|threshold| format!("{threshold:.2}"))
            .unwrap_or_else(|| "n/a".to_string())
    )
}

#[cfg(target_os = "macos")]
fn upstream_behavior_label(upstream_apca_threshold: f32) -> String {
    format!(
        "Current Upstream Behavior - APCA {:.1} per-accent adjustment",
        upstream_apca_threshold
    )
}

#[cfg(target_os = "macos")]
fn strategy_treatments_label(strategy: BracketColorizationPaletteStrategy) -> &'static str {
    match (
        strategy_reordered(strategy),
        strategy_adjusted_background(strategy),
    ) {
        (true, true) => "per-accent background fix + adjacency reorder",
        (true, false) => "adjacency reorder only",
        (false, true) => "per-accent background fix only",
        (false, false) => "unchanged",
    }
}

#[cfg(target_os = "macos")]
fn slugify(input: &str) -> String {
    let mut slug = String::with_capacity(input.len());
    let mut last_was_separator = false;

    for character in input.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator {
            slug.push('-');
            last_was_separator = true;
        }
    }

    slug.trim_matches('-').to_string()
}

#[cfg(target_os = "macos")]
fn format_capture_report(
    theme_path: &Path,
    theme_family_name: &str,
    results: &[ThemeCaptureResult],
    output_directory: &Path,
    upstream_apca_threshold: f32,
    auto_config: AutoBracketColorizationConfig,
) -> String {
    let mut report = String::new();
    report.push_str(&format!(
        "# Bracket Contrast Capture: {theme_family_name}\n\n"
    ));
    report.push_str(&format!("- Theme file: `{}`\n", theme_path.display()));
    report.push_str(&format!(
        "- Output directory: `{}`\n",
        output_directory.display()
    ));
    report.push_str(&format!(
        "- Upstream APCA threshold: `{upstream_apca_threshold:.1}`\n"
    ));
    report.push_str(&format!(
        "- Auto background APCA floors: `light={:.1}`, `dark={:.1}`\n",
        auto_config.minimum_background_apca_light, auto_config.minimum_background_apca_dark
    ));
    report.push_str(&format!(
        "- Auto adjacent OKLab floors: `light={:.3}`, `dark={:.3}`\n",
        auto_config.minimum_adjacent_oklab_light, auto_config.minimum_adjacent_oklab_dark
    ));
    report.push_str("- Notes: This report scores bracket-vs-background using absolute APCA Lc and adjacent bracket separation using OKLab distance. Use the saved captures as the final visual check.\n");
    report.push('\n');

    for result in results {
        let analysis = &result.analysis;
        report.push_str(&format!("## {}\n\n", result.name));
        report.push_str(&format!(
            "- Appearance: `{}`\n",
            appearance_label(result.appearance)
        ));
        report.push_str(&format!(
            "- Editor background: `{}`\n",
            format_color(result.editor_background)
        ));
        report.push_str(&format!(
            "- Strategy: `{}`\n",
            strategy_label(analysis.strategy)
        ));
        report.push_str(&format!(
            "- Reordered: `{}`\n",
            yes_or_no(strategy_reordered(analysis.strategy))
        ));
        report.push_str(&format!(
            "- Background-adjusted: `{}`\n",
            yes_or_no(strategy_adjusted_background(analysis.strategy))
        ));
        report.push_str(&format!(
            "- Changed colors: `{}`\n",
            yes_or_no(analysis.changed)
        ));

        if let Some(threshold) = analysis.adjacent_threshold {
            report.push_str(&format!("- Adjacent threshold: `{threshold:.3}`\n"));
        } else {
            report.push_str("- Adjacent threshold: `n/a`\n");
        }

        if let Some(threshold) = analysis.background_threshold {
            report.push_str(&format!("- Background threshold: `{threshold:.3}`\n"));
        } else {
            report.push_str("- Background threshold: `n/a`\n");
        }

        report.push_str(&format!(
            "- Original min adjacent distance: `{:.3}`\n",
            analysis.original_score.min_adjacent_distance
        ));
        report.push_str(&format!(
            "- Final min adjacent distance: `{:.3}`\n",
            analysis.final_score.min_adjacent_distance
        ));
        report.push_str(&format!(
            "- Original threshold result: `{}`\n",
            threshold_result(
                analysis.original_score.min_adjacent_distance,
                analysis.adjacent_threshold
            )
        ));
        report.push_str(&format!(
            "- Final threshold result: `{}`\n",
            threshold_result(
                analysis.final_score.min_adjacent_distance,
                analysis.adjacent_threshold
            )
        ));
        report.push_str(&format!(
            "- Original min background contrast: `{:.3}`\n",
            analysis.original_score.min_background_contrast
        ));
        report.push_str(&format!(
            "- Final min background contrast: `{:.3}`\n",
            analysis.final_score.min_background_contrast
        ));
        report.push_str(&format!(
            "- Original background threshold result: `{}`\n",
            threshold_result(
                analysis.original_score.min_background_contrast,
                analysis.background_threshold
            )
        ));
        report.push_str(&format!(
            "- Final background threshold result: `{}`\n",
            threshold_result(
                analysis.final_score.min_background_contrast,
                analysis.background_threshold
            )
        ));
        report.push_str(&format!(
            "- Original average adjacent distance: `{:.3}`\n",
            analysis.original_score.average_adjacent_distance
        ));
        report.push_str(&format!(
            "- Final average adjacent distance: `{:.3}`\n",
            analysis.final_score.average_adjacent_distance
        ));
        report.push_str(&format!(
            "- Original average background contrast: `{:.3}`\n",
            analysis.original_score.average_background_contrast
        ));
        report.push_str(&format!(
            "- Final average background contrast: `{:.3}`\n",
            analysis.final_score.average_background_contrast
        ));
        report.push_str(&format!(
            "- Raw theme capture: `{}`\n",
            result.raw_theme_path.display()
        ));
        report.push_str(&format!(
            "- Upstream capture: `{}`\n",
            result.upstream_path.display()
        ));
        report.push_str(&format!(
            "- Auto capture: `{}`\n",
            result.auto_mode_path.display()
        ));
        report.push_str(&format!(
            "- Comparison capture: `{}`\n\n",
            result.comparison_path.display()
        ));
        if let Some(upstream_threshold_stack_path) = &result.upstream_threshold_stack_path {
            report.push_str(&format!(
                "- Upstream APCA threshold stack capture: `{}`\n",
                upstream_threshold_stack_path.display()
            ));
        }
        if let Some(auto_background_threshold_stack_path) =
            &result.auto_background_threshold_stack_path
        {
            report.push_str(&format!(
                "- Auto background APCA stack capture: `{}`\n",
                auto_background_threshold_stack_path.display()
            ));
        }
        if let Some(auto_adjacent_threshold_stack_path) = &result.auto_adjacent_threshold_stack_path
        {
            report.push_str(&format!(
                "- Auto adjacent OKLab stack capture: `{}`\n",
                auto_adjacent_threshold_stack_path.display()
            ));
        }
        report.push('\n');

        report.push_str("### Palettes\n\n");
        report.push_str(&format!(
            "- Original order: {}\n",
            format_palette(&analysis.original_palette)
        ));
        report.push_str(&format!(
            "- Final order: {}\n",
            format_palette(&analysis.final_palette)
        ));
        report.push_str(&format!(
            "- Final order mapped to original indices: `{}`\n\n",
            format_order_mapping(&analysis.original_palette, &analysis.final_palette)
        ));
        report.push_str("> Passing the APCA floor or the OKLab adjacent-distance floor does not by itself guarantee a good bracket palette. Use the saved captures as the final check.\n\n");
    }

    report
}

#[cfg(target_os = "macos")]
fn strategy_label(strategy: BracketColorizationPaletteStrategy) -> &'static str {
    match strategy {
        BracketColorizationPaletteStrategy::ThemeOrder => "theme order",
        BracketColorizationPaletteStrategy::PreservedSmallPalette => "palette too small to reorder",
        BracketColorizationPaletteStrategy::PreservedStrongPalette => "contrast already sufficient",
        BracketColorizationPaletteStrategy::PreservedWeakPalette => {
            "weak palette kept in theme order"
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

#[cfg(target_os = "macos")]
fn strategy_reordered(strategy: BracketColorizationPaletteStrategy) -> bool {
    matches!(
        strategy,
        BracketColorizationPaletteStrategy::ReorderedWeakPalette
            | BracketColorizationPaletteStrategy::AdjustedBackgroundAndReorderedPalette
    )
}

#[cfg(target_os = "macos")]
fn strategy_adjusted_background(strategy: BracketColorizationPaletteStrategy) -> bool {
    matches!(
        strategy,
        BracketColorizationPaletteStrategy::AdjustedBackgroundPalette
            | BracketColorizationPaletteStrategy::AdjustedBackgroundAndReorderedPalette
    )
}

#[cfg(target_os = "macos")]
fn appearance_label(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::Light => "light",
        Appearance::Dark => "dark",
    }
}

#[cfg(target_os = "macos")]
fn yes_or_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(target_os = "macos")]
fn threshold_result(distance: f32, threshold: Option<f32>) -> &'static str {
    match threshold {
        Some(threshold) if distance >= threshold => "pass",
        Some(_) => "fail",
        None => "n/a",
    }
}

#[cfg(target_os = "macos")]
fn format_palette(palette: &[Hsla]) -> String {
    palette
        .iter()
        .enumerate()
        .map(|(index, color)| format!("`{}:{}`", index + 1, format_color(*color)))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(target_os = "macos")]
fn format_order_mapping(original: &[Hsla], final_palette: &[Hsla]) -> String {
    final_palette
        .iter()
        .map(|color| {
            original
                .iter()
                .position(|candidate| *candidate == *color)
                .map(|index| (index + 1).to_string())
                .unwrap_or_else(|| "?".to_string())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(target_os = "macos")]
fn format_color(color: Hsla) -> String {
    let rgba = GpuiRgba::from(color);
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
