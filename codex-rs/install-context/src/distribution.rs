use crate::InstallMethod;

pub const PRODUCT_NAME: &str = "SpineCodex";
pub const NPM_PACKAGE: &str = "@spinejit/spine-codex";
pub const NPM_PACKAGE_LATEST: &str = "@spinejit/spine-codex@latest";
pub const NPM_GLOBAL_UPDATE_ARGS: &[&str] = &["install", "-g", NPM_PACKAGE_LATEST];
pub const BUN_GLOBAL_UPDATE_ARGS: &[&str] = &["install", "-g", NPM_PACKAGE_LATEST];
pub const PNPM_GLOBAL_UPDATE_ARGS: &[&str] = &["add", "-g", NPM_PACKAGE_LATEST];
pub const NPM_SCOPE_DIR: &str = "@spinejit";
pub const NPM_PACKAGE_DIR: &str = "spine-codex";
pub const NPM_REGISTRY_URL: &str = "https://registry.npmjs.org/@spinejit%2fspine-codex";
pub const GITHUB_REPOSITORY_URL: &str = "https://github.com/GhabiX/SpineCodex";
pub const GITHUB_LATEST_RELEASE_URL: &str = "https://github.com/GhabiX/SpineCodex/releases/latest";
pub const GITHUB_LATEST_RELEASE_API_URL: &str =
    "https://api.github.com/repos/GhabiX/SpineCodex/releases/latest";
pub const GITHUB_ISSUE_TEMPLATE_URL: &str =
    "https://github.com/GhabiX/SpineCodex/issues/new?template=3-cli.yml";
pub const ANNOUNCEMENT_TIP_URL: &str =
    "https://raw.githubusercontent.com/GhabiX/SpineCodex/main/announcement_tip.toml";
pub const VERSION_CACHE_FILENAME: &str = "spine-codex-version.json";

pub fn supports_automatic_update(method: &InstallMethod) -> bool {
    matches!(
        method,
        InstallMethod::Npm | InstallMethod::Bun | InstallMethod::Pnpm
    )
}

pub fn release_version_from_tag(tag_name: &str) -> Option<&str> {
    tag_name
        .strip_prefix('v')
        .or_else(|| tag_name.strip_prefix("rust-v"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_updates_are_limited_to_spine_codex_package_managers() {
        assert!(supports_automatic_update(&InstallMethod::Npm));
        assert!(supports_automatic_update(&InstallMethod::Bun));
        assert!(supports_automatic_update(&InstallMethod::Pnpm));
        assert!(!supports_automatic_update(&InstallMethod::Brew));
        assert!(!supports_automatic_update(&InstallMethod::Other));
    }

    #[test]
    fn release_tags_use_spine_codex_v_with_legacy_read_compatibility() {
        assert_eq!(release_version_from_tag("v0.1.0"), Some("0.1.0"));
        assert_eq!(release_version_from_tag("rust-v0.1.0"), Some("0.1.0"));
        assert_eq!(release_version_from_tag("0.1.0"), None);
    }

    #[test]
    fn package_manager_commands_share_the_spine_codex_target() {
        assert_eq!(NPM_GLOBAL_UPDATE_ARGS.last(), Some(&NPM_PACKAGE_LATEST));
        assert_eq!(BUN_GLOBAL_UPDATE_ARGS.last(), Some(&NPM_PACKAGE_LATEST));
        assert_eq!(PNPM_GLOBAL_UPDATE_ARGS.last(), Some(&NPM_PACKAGE_LATEST));
    }
}
