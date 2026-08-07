//! Configured, feature-gated prompt extension.

use crate::Feature;
use crate::SpineConfig;

const SPINE_VIEW_START_MARKER: &str = "\n\n<spine_view>";

pub(crate) fn extend(mut base: String, config: &SpineConfig) -> String {
    if config.is_enabled(Feature::Jit)
        && let Some(start) = base.rfind(SPINE_VIEW_START_MARKER)
    {
        base.truncate(start);
    }

    let segments = [Feature::Jit, Feature::Trim, Feature::Spawn]
        .into_iter()
        .filter(|feature| config.is_enabled(*feature))
        .map(|feature| config.prompt(feature))
        .filter(|segment| !segment.is_empty());
    for segment in segments {
        if base.contains(segment) {
            continue;
        }
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

    #[test]
    fn feature_off_is_identity() {
        let config = SpineConfig::v1();
        assert_eq!(extend("base".to_string(), &config), "base");
    }

    #[test]
    fn configured_segment_is_idempotent() {
        let config = SpineConfig::parse_toml(
            r#"schema_version = 1
[limits]
trim_threshold_bytes = 100
[prompt]
jit = "<spine_view>jit</spine_view>"
node = "node prompt"
[tools.open]
description = "open"
[tools.close]
description = "close"
[tools.next]
description = "next"
"#,
        )
        .unwrap();
        let config = config.with_feature(Feature::Jit).unwrap();
        let once = extend("base".to_string(), &config);
        assert_eq!(extend(once.clone(), &config), once);
    }
}
