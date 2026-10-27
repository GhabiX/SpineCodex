use std::sync::OnceLock;

use icu_decimal::DecimalFormatter;
use icu_decimal::input::Decimal;
use icu_decimal::options::DecimalFormatterOptions;
use icu_locale_core::Locale;

fn make_local_formatter() -> Option<DecimalFormatter> {
    let loc: Locale = sys_locale::get_locale()?.parse().ok()?;
    DecimalFormatter::try_new(loc.into(), DecimalFormatterOptions::default()).ok()
}

fn make_en_us_formatter() -> DecimalFormatter {
    #![allow(clippy::expect_used)]
    let loc: Locale = "en-US".parse().expect("en-US wasn't a valid locale");
    DecimalFormatter::try_new(loc.into(), DecimalFormatterOptions::default())
        .expect("en-US wasn't a valid locale")
}

fn formatter() -> &'static DecimalFormatter {
    static FORMATTER: OnceLock<DecimalFormatter> = OnceLock::new();
    FORMATTER.get_or_init(|| make_local_formatter().unwrap_or_else(make_en_us_formatter))
}

/// Format an i64 with locale-aware digit separators (e.g. "12345" -> "12,345"
/// for en-US).
pub fn format_with_separators(n: i64) -> String {
    formatter().format(&Decimal::from(n)).to_string()
}

fn format_with_separators_with_formatter(n: i64, formatter: &DecimalFormatter) -> String {
    formatter.format(&Decimal::from(n)).to_string()
}

fn format_si_suffix_with_formatter(n: i64, formatter: &DecimalFormatter) -> String {
    let n = n.max(0);
    if n < 1000 {
        return formatter.format(&Decimal::from(n)).to_string();
    }

    let format_scaled = |n: i64, scale: i64, frac_digits: u32| -> String {
        let value = n as f64 / scale as f64;
        let scaled: i64 = (value * 10f64.powi(frac_digits as i32)).round() as i64;
        let mut dec = Decimal::from(scaled);
        dec.multiply_pow10(-(frac_digits as i16));
        formatter.format(&dec).to_string()
    };

    const UNITS: [(i64, &str); 3] = [(1_000, "K"), (1_000_000, "M"), (1_000_000_000, "G")];
    let f = n as f64;
    for &(scale, suffix) in &UNITS {
        if (100.0 * f / scale as f64).round() < 1000.0 {
            return format!("{}{suffix}", format_scaled(n, scale, 2));
        }
        if (10.0 * f / scale as f64).round() < 1000.0 {
            return format!("{}{suffix}", format_scaled(n, scale, 1));
        }
        if (f / scale as f64).round() < 1000.0 {
            return format!("{}{suffix}", format_scaled(n, scale, 0));
        }
    }

    format!(
        "{}G",
        format_with_separators_with_formatter(((n as f64) / 1e9).round() as i64, formatter)
    )
}

/// Format token counts to three significant figures with base-10 SI suffixes.
pub fn format_si_suffix(n: i64) -> String {
    format_si_suffix_with_formatter(n, formatter())
}
