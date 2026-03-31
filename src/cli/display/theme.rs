use crossterm::style::{Color, Stylize};
use openclaudia::tui;

/// Handle theme command - list or switch themes.
///
/// Returns the new theme name if one was selected, so the caller
/// can update its active theme reference.
pub fn handle_theme_command(args: &str) -> Option<String> {
    let themes: &[(&str, &str, Color, Color)] = &[
        (
            "default",
            "Default terminal colors",
            Color::Reset,
            Color::Reset,
        ),
        ("ocean", "Cool blue tones", Color::Cyan, Color::Blue),
        (
            "forest",
            "Earthy green tones",
            Color::Green,
            Color::DarkGreen,
        ),
        ("sunset", "Warm orange tones", Color::Yellow, Color::Red),
        ("mono", "Monochrome grayscale", Color::White, Color::Grey),
        ("neon", "Bright vibrant colors", Color::Magenta, Color::Cyan),
    ];

    if args.is_empty() {
        println!("\n=== Available Themes ===\n");

        for (name, desc, primary, _secondary) in themes {
            let preview = format!("  {} - {}", name, desc);
            println!("{}", preview.with(*primary));
        }

        println!("\nUse /theme <name> to switch themes.");
        println!("Theme affects status bar, headings, and code highlighting.\n");
        None
    } else {
        let theme_name = args.trim().to_lowercase();

        if let Some((name, desc, primary, _)) = themes.iter().find(|(n, _, _, _)| *n == theme_name)
        {
            if let Some(theme) = tui::Theme::from_name(name) {
                if let Err(e) = theme.save() {
                    eprintln!("Warning: could not save theme preference: {}", e);
                }
            }

            println!();
            println!(
                "{}",
                format!("Switched to '{}' theme: {}", name, desc).with(*primary)
            );
            println!(
                "{}",
                "Theme preview: This is how messages will appear.".with(*primary)
            );
            println!("Theme saved and will persist across sessions.");
            println!();

            Some(name.to_string())
        } else {
            eprintln!("\nUnknown theme: '{}'\n", theme_name);
            eprintln!(
                "Available themes: {}\n",
                themes
                    .iter()
                    .map(|(n, _, _, _)| *n)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            None
        }
    }
}
