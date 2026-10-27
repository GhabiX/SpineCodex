//! Deterministic composition of raw host instructions and feature-gated segments.

use crate::Feature;
use crate::SpineConfig;

pub(crate) fn extend(mut base: String, config: &SpineConfig) -> String {
    let segments = [Feature::Jit, Feature::Spawn]
        .into_iter()
        .filter(|feature| config.is_enabled(*feature))
        .map(|feature| config.prompt(feature))
        .filter(|segment| !segment.is_empty());
    for segment in segments {
        if !base.is_empty() {
            base.push_str("\n\n");
        }
        base.push_str(segment);
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"schema_version = 1
[prompt]
jit = "<spine_instruction>jit</spine_instruction>"
spawn = "spawn"
spawn_explicit_request_only = "explicit"
spawn_proactive = "proactive"
[tools.open]
description = "open"
[tools.close]
description = "close"
[tools.next]
description = "next"
[tools.spawn]
description = "spawn"
"#;

    #[test]
    fn feature_off_is_identity() {
        let config = SpineConfig::v1();
        let base = " host \n\n<spine_instruction>example</spine_instruction>\n tail ";
        assert_eq!(extend(base.to_string(), &config), base);
    }

    #[test]
    fn raw_host_text_is_preserved_and_composition_is_repeatable() {
        let config = SpineConfig::parse_toml(CONFIG)
            .unwrap()
            .with_feature(Feature::Jit)
            .unwrap();
        for base in [
            " host \n\n<spine_instruction>example</spine_instruction>\n tail ",
            "host\n\n<spine_view>example</spine_view>\n tail",
            "host\n\n<spine_instruction>jit</spine_instruction>\n tail",
        ] {
            let expected = format!("{base}\n\n{}", config.prompt(Feature::Jit));
            assert_eq!(extend(base.to_string(), &config), expected);
            assert_eq!(extend(base.to_string(), &config), expected);
        }
    }

    #[test]
    fn configured_sources_have_fixed_order_and_separators() {
        for source in [
            CONFIG.to_string(),
            CONFIG.replace(
                "spawn = \"spawn\"",
                "spawn = \"<spine_instruction>jit</spine_instruction>\"",
            ),
        ] {
            for features in [vec![Feature::Jit], vec![Feature::Spawn, Feature::Jit]] {
                let config = SpineConfig::parse_toml(&source)
                    .unwrap()
                    .with_features(features)
                    .unwrap();
                let mut segments = vec![config.prompt(Feature::Jit)];
                if config.is_enabled(Feature::Spawn) {
                    segments.push(config.prompt(Feature::Spawn));
                }
                let expected = segments.join("\n\n");
                assert_eq!(extend(String::new(), &config), expected);
                assert_eq!(
                    extend("base".to_string(), &config),
                    format!("base\n\n{expected}")
                );
            }
        }
    }
}
