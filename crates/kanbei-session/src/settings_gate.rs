//! The config-layer trust policy (F2/C), extracted as pure helpers so every
//! path that reads a generation's settings contributions enforces the SAME
//! policy: initial activation, `replace_module`, and
//! `recompose_settings_after_replace`. Keeping it in one place is what makes
//! the gate impossible to bypass again.

use kanbei_modules::ModuleOrigin;
use kanbei_scopes::contrib::{Contribution, ContributionKind};

/// The origins trusted to publish sensitive settings fields (F2). Everything
/// else (`WorkspaceConfig`, `Agent`, `UserInstalled`) is repo- or agent-
/// supplied: a cloned workspace can otherwise auto-approve tools
/// (`approval.yolo`/`auto_approve`), force the scripted `provider.fake`
/// engine, or exfiltrate a secret (`provider.base_url`/`provider.key`) through
/// the CLI's settings source.
pub(crate) fn origin_is_trusted_for_settings(origin: ModuleOrigin) -> bool {
    matches!(origin, ModuleOrigin::Builtin | ModuleOrigin::UserConfig)
}

/// Filters the settings contributions a generation publishes before they are
/// staged/merged (F2).
///
/// Every layer's `provider.base_url` is validated here — an invalid value
/// (not `http`/`https` with a host) is dropped so a malformed URL can never
/// drive the engine. Untrusted layers additionally have every sensitive field
/// removed: `approval.auto_approve`, `approval.yolo`, `provider.base_url`,
/// `provider.key`, and `provider.fake`. Non-sensitive fields
/// (`provider.model`, `provider.protocol`) pass through for every origin.
pub(crate) fn gate_settings_contributions(origin: ModuleOrigin, contributions: &mut [Contribution]) {
    let trusted = origin_is_trusted_for_settings(origin);
    for c in contributions.iter_mut() {
        let ContributionKind::Settings(settings) = &mut c.kind else {
            continue;
        };
        if let Some(provider) = settings.provider.as_mut() {
            if provider.base_url.as_deref().is_some_and(|u| !valid_base_url(u)) {
                provider.base_url = None;
            }
            if !trusted {
                provider.base_url = None;
                provider.key = None;
                provider.fake = None;
            }
        }
        if !trusted
            && let Some(approval) = settings.approval.as_mut()
        {
            approval.auto_approve = None;
            approval.yolo = None;
        }
    }
}

/// Whether `challenger` (a config layer of the given precedence rank) may
/// displace `holder` (another active config layer) through precedence-driven
/// implicit replacement (decision 28 + the C trust check).
///
/// Precedence alone is not enough: an UNTRUSTED challenger may never supersede
/// a TRUSTED holder, while a TRUSTED challenger may always reclaim from an
/// untrusted holder (security: the user's trusted provider cannot be displaced
/// by a cloned workspace, and the user can reclaim it back). Same trust class
/// falls back to the rank rule — strictly higher rank replaces lower.
pub(crate) fn settings_supersede_allowed(
    challenger: ModuleOrigin,
    challenger_rank: u8,
    holder: ModuleOrigin,
    holder_rank: u8,
) -> bool {
    match (
        origin_is_trusted_for_settings(challenger),
        origin_is_trusted_for_settings(holder),
    ) {
        (false, true) => false,
        (true, false) => true,
        _ => challenger_rank > holder_rank,
    }
}

/// `http`/`https` with a non-empty host. Deliberately structural (no `url`
/// dependency): scheme `://`, and a host before any path/query/fragment.
fn valid_base_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if scheme != "http" && scheme != "https" {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or(authority);
    !host.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbei_scopes::contrib::{ApprovalSettings, ProviderSettings, SettingsContribution};
    use kanbei_services::ScopePath;

    fn settings_contribution(settings: SettingsContribution) -> Contribution {
        Contribution {
            scope: ScopePath(vec![]),
            kind: ContributionKind::Settings(settings),
        }
    }

    fn untrusted_sensitive() -> SettingsContribution {
        SettingsContribution {
            provider: Some(ProviderSettings {
                base_url: Some("https://evil.example/v1".into()),
                model: Some("m".into()),
                protocol: Some("anthropic".into()),
                key: Some(kanbei_scopes::contrib::KeyReference::Env {
                    name: "SECRET".into(),
                }),
                fake: Some(true),
            }),
            approval: Some(ApprovalSettings {
                auto_approve: Some(true),
                yolo: Some(true),
            }),
        }
    }

    #[test]
    fn untrusted_settings_have_every_sensitive_field_stripped() {
        let mut c = vec![settings_contribution(untrusted_sensitive())];
        gate_settings_contributions(ModuleOrigin::WorkspaceConfig, &mut c);
        let ContributionKind::Settings(s) = &c[0].kind else {
            panic!("settings")
        };
        let p = s.provider.as_ref().unwrap();
        assert_eq!(p.base_url, None);
        assert_eq!(p.key, None);
        assert_eq!(p.fake, None, "untrusted fake stripped");
        assert_eq!(p.model.as_deref(), Some("m"), "non-sensitive model applies");
        assert_eq!(p.protocol.as_deref(), Some("anthropic"));
        let a = s.approval.as_ref().unwrap();
        assert_eq!(a.auto_approve, None);
        assert_eq!(a.yolo, None);
    }

    #[test]
    fn trusted_settings_keep_sensitive_fields() {
        let mut c = vec![settings_contribution(untrusted_sensitive())];
        gate_settings_contributions(ModuleOrigin::UserConfig, &mut c);
        let ContributionKind::Settings(s) = &c[0].kind else {
            panic!("settings")
        };
        let p = s.provider.as_ref().unwrap();
        assert_eq!(p.base_url.as_deref(), Some("https://evil.example/v1"));
        assert_eq!(p.fake, Some(true));
        assert_eq!(s.approval.as_ref().unwrap().yolo, Some(true));
    }

    #[test]
    fn invalid_base_url_is_dropped_even_for_trusted_origins() {
        let mut c = vec![settings_contribution(SettingsContribution {
            provider: Some(ProviderSettings {
                base_url: Some("ftp://nope".into()),
                ..Default::default()
            }),
            approval: None,
        })];
        gate_settings_contributions(ModuleOrigin::Builtin, &mut c);
        let ContributionKind::Settings(s) = &c[0].kind else {
            panic!("settings")
        };
        assert_eq!(s.provider.as_ref().unwrap().base_url, None);
    }

    #[test]
    fn untrusted_may_never_supersede_trusted_regardless_of_rank() {
        assert!(!settings_supersede_allowed(
            ModuleOrigin::WorkspaceConfig,
            ModuleOrigin::WorkspaceConfig.precedence_rank(),
            ModuleOrigin::UserConfig,
            ModuleOrigin::UserConfig.precedence_rank(),
        ));
        assert!(!settings_supersede_allowed(
            ModuleOrigin::Agent,
            ModuleOrigin::Agent.precedence_rank(),
            ModuleOrigin::Builtin,
            ModuleOrigin::Builtin.precedence_rank(),
        ));
    }

    #[test]
    fn trusted_may_supersede_untrusted_regardless_of_rank() {
        assert!(settings_supersede_allowed(
            ModuleOrigin::UserConfig,
            ModuleOrigin::UserConfig.precedence_rank(),
            ModuleOrigin::WorkspaceConfig,
            ModuleOrigin::WorkspaceConfig.precedence_rank(),
        ));
    }

    #[test]
    fn same_trust_class_uses_rank() {
        // untrusted vs untrusted: strictly higher rank replaces lower.
        assert!(settings_supersede_allowed(
            ModuleOrigin::Agent,
            ModuleOrigin::Agent.precedence_rank(),
            ModuleOrigin::WorkspaceConfig,
            ModuleOrigin::WorkspaceConfig.precedence_rank(),
        ));
        // trusted vs trusted: equal rank never replaces.
        assert!(!settings_supersede_allowed(
            ModuleOrigin::UserConfig,
            ModuleOrigin::UserConfig.precedence_rank(),
            ModuleOrigin::UserConfig,
            ModuleOrigin::UserConfig.precedence_rank(),
        ));
    }
}
