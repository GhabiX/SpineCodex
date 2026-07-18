use super::*;

#[test]
fn device_code_prompt_renders_phishing_warning() {
    let prompt = device_code_prompt(
        "https://example.com/device",
        "ABCD-EFGH",
        /*use_spine_brand*/ false,
    );

    assert!(prompt.contains(
        "\x1b[90mContinue only if you started this login in Codex. If a website or another person gave you this code, cancel.\x1b[0m"
    ));
}

#[test]
fn device_code_prompt_renders_spine_brand() {
    let prompt = device_code_prompt(
        "https://example.com/device",
        "ABCD-EFGH",
        /*use_spine_brand*/ true,
    );

    assert!(prompt.contains("Welcome to \x1b[32mSpine\x1b[0m Codex"));
    assert!(prompt.contains(
        "\x1b[90mContinue only if you started this login in Spine Codex. If a website or another person gave you this code, cancel.\x1b[0m"
    ));
}
