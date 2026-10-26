//! `cargo run -p tf-apple --example speech_install -- [locale]`

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let locale = std::env::args().nth(1).unwrap_or_else(|| "en-US".to_owned());
    let locales = tf_apple::speech::locales()?;
    let known = locales.iter().find(|candidate| candidate.id == locale);
    match known {
        Some(locale) => eprintln!("{} ({}) installed={}", locale.id, locale.name, locale.installed),
        None => eprintln!("{locale} is not in the supported locale list"),
    }
    tf_apple::speech::install(&locale, |progress| {
        eprintln!("{:.0}%", progress * 100.0);
    })?;
    println!("ready: {locale}");
    Ok(())
}
