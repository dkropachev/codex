use codex_app_server_protocol::ThreadSourceKind;
use codex_core::INTERACTIVE_SESSION_SOURCES;
use codex_protocol::protocol::SessionSource as CoreSessionSource;
use codex_protocol::protocol::SubAgentSource as CoreSubAgentSource;
use codex_protocol::protocol::ThreadSource as CoreThreadSource;

const ALL_THREAD_SOURCE_KINDS: &[ThreadSourceKind] = &[
    ThreadSourceKind::Cli,
    ThreadSourceKind::VsCode,
    ThreadSourceKind::Exec,
    ThreadSourceKind::AppServer,
    ThreadSourceKind::SubAgent,
    ThreadSourceKind::SubAgentReview,
    ThreadSourceKind::SubAgentCompact,
    ThreadSourceKind::SubAgentThreadSpawn,
    ThreadSourceKind::SubAgentOther,
    ThreadSourceKind::Unknown,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceFilterMode {
    DefaultVisible,
    ExplicitKinds,
    AllSources,
}

pub(crate) fn compute_source_filters(
    source_kinds: Option<Vec<ThreadSourceKind>>,
) -> (Vec<CoreSessionSource>, Option<Vec<ThreadSourceKind>>) {
    let Some(source_kinds) = source_kinds else {
        return (INTERACTIVE_SESSION_SOURCES.to_vec(), None);
    };

    if source_kinds.is_empty() {
        return (INTERACTIVE_SESSION_SOURCES.to_vec(), None);
    }

    if ALL_THREAD_SOURCE_KINDS
        .iter()
        .all(|kind| source_kinds.contains(kind))
    {
        return (Vec::new(), None);
    }

    let requires_post_filter = source_kinds.iter().any(|kind| {
        matches!(
            kind,
            ThreadSourceKind::Exec
                | ThreadSourceKind::AppServer
                | ThreadSourceKind::SubAgent
                | ThreadSourceKind::SubAgentReview
                | ThreadSourceKind::SubAgentCompact
                | ThreadSourceKind::SubAgentThreadSpawn
                | ThreadSourceKind::SubAgentOther
                | ThreadSourceKind::Unknown
        )
    });

    if requires_post_filter {
        (Vec::new(), Some(source_kinds))
    } else {
        let interactive_sources = source_kinds
            .iter()
            .filter_map(|kind| match kind {
                ThreadSourceKind::Cli => Some(CoreSessionSource::Cli),
                ThreadSourceKind::VsCode => Some(CoreSessionSource::VSCode),
                ThreadSourceKind::Exec
                | ThreadSourceKind::AppServer
                | ThreadSourceKind::SubAgent
                | ThreadSourceKind::SubAgentReview
                | ThreadSourceKind::SubAgentCompact
                | ThreadSourceKind::SubAgentThreadSpawn
                | ThreadSourceKind::SubAgentOther
                | ThreadSourceKind::Unknown => None,
            })
            .collect::<Vec<_>>();
        (interactive_sources, Some(source_kinds))
    }
}

pub(crate) fn source_filter_mode(
    allowed_sources: &[CoreSessionSource],
    filter: Option<&[ThreadSourceKind]>,
) -> SourceFilterMode {
    match (allowed_sources.is_empty(), filter) {
        (true, None) => SourceFilterMode::AllSources,
        (_, Some(_)) => SourceFilterMode::ExplicitKinds,
        (false, None) => SourceFilterMode::DefaultVisible,
    }
}

pub(crate) fn source_filter_allows_thread(
    source: &CoreSessionSource,
    thread_source: Option<CoreThreadSource>,
    filter: Option<&[ThreadSourceKind]>,
    mode: SourceFilterMode,
) -> bool {
    if matches!(mode, SourceFilterMode::AllSources) {
        return true;
    }

    if matches!(thread_source, Some(CoreThreadSource::Subagent))
        && !filter.is_some_and(filter_includes_subagent_sources)
    {
        return false;
    }

    filter.is_none_or(|filter| source_kind_matches(source, thread_source, filter))
}

fn source_kind_matches(
    source: &CoreSessionSource,
    thread_source: Option<CoreThreadSource>,
    filter: &[ThreadSourceKind],
) -> bool {
    filter.iter().any(|kind| match kind {
        ThreadSourceKind::Cli => matches!(source, CoreSessionSource::Cli),
        ThreadSourceKind::VsCode => matches!(source, CoreSessionSource::VSCode),
        ThreadSourceKind::Exec => matches!(source, CoreSessionSource::Exec),
        ThreadSourceKind::AppServer => matches!(source, CoreSessionSource::Mcp),
        ThreadSourceKind::SubAgent => {
            matches!(source, CoreSessionSource::SubAgent(_))
                || matches!(thread_source, Some(CoreThreadSource::Subagent))
        }
        ThreadSourceKind::SubAgentReview => {
            matches!(
                source,
                CoreSessionSource::SubAgent(CoreSubAgentSource::Review)
            )
        }
        ThreadSourceKind::SubAgentCompact => {
            matches!(
                source,
                CoreSessionSource::SubAgent(CoreSubAgentSource::Compact)
            )
        }
        ThreadSourceKind::SubAgentThreadSpawn => matches!(
            source,
            CoreSessionSource::SubAgent(CoreSubAgentSource::ThreadSpawn { .. })
        ),
        ThreadSourceKind::SubAgentOther => matches!(
            source,
            CoreSessionSource::SubAgent(CoreSubAgentSource::Other(_))
        ),
        ThreadSourceKind::Unknown => matches!(source, CoreSessionSource::Unknown),
    })
}

fn filter_includes_subagent_sources(filter: &[ThreadSourceKind]) -> bool {
    filter.iter().any(|kind| {
        matches!(
            kind,
            ThreadSourceKind::SubAgent
                | ThreadSourceKind::SubAgentReview
                | ThreadSourceKind::SubAgentCompact
                | ThreadSourceKind::SubAgentThreadSpawn
                | ThreadSourceKind::SubAgentOther
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::ThreadId;
    use pretty_assertions::assert_eq;
    use uuid::Uuid;

    #[test]
    fn compute_source_filters_defaults_to_interactive_sources() {
        let (allowed_sources, filter) = compute_source_filters(/*source_kinds*/ None);

        assert_eq!(allowed_sources, INTERACTIVE_SESSION_SOURCES.to_vec());
        assert_eq!(filter, None);
    }

    #[test]
    fn compute_source_filters_empty_means_interactive_sources() {
        let (allowed_sources, filter) = compute_source_filters(Some(Vec::new()));

        assert_eq!(allowed_sources, INTERACTIVE_SESSION_SOURCES.to_vec());
        assert_eq!(filter, None);
    }

    #[test]
    fn compute_source_filters_all_source_kinds_disable_filtering() {
        let (allowed_sources, filter) =
            compute_source_filters(Some(ALL_THREAD_SOURCE_KINDS.to_vec()));

        assert_eq!(allowed_sources, Vec::new());
        assert_eq!(filter, None);
        assert_eq!(
            source_filter_mode(allowed_sources.as_slice(), filter.as_deref()),
            SourceFilterMode::AllSources
        );
    }

    #[test]
    fn compute_source_filters_interactive_only_skips_post_filtering() {
        let source_kinds = vec![ThreadSourceKind::Cli, ThreadSourceKind::VsCode];
        let (allowed_sources, filter) = compute_source_filters(Some(source_kinds.clone()));

        assert_eq!(
            allowed_sources,
            vec![CoreSessionSource::Cli, CoreSessionSource::VSCode]
        );
        assert_eq!(filter, Some(source_kinds));
    }

    #[test]
    fn compute_source_filters_subagent_variant_requires_post_filtering() {
        let source_kinds = vec![ThreadSourceKind::SubAgentReview];
        let (allowed_sources, filter) = compute_source_filters(Some(source_kinds.clone()));

        assert_eq!(allowed_sources, Vec::new());
        assert_eq!(filter, Some(source_kinds));
    }

    #[test]
    fn source_kind_matches_distinguishes_subagent_variants() {
        let parent_thread_id =
            ThreadId::from_string(&Uuid::new_v4().to_string()).expect("valid thread id");
        let review = CoreSessionSource::SubAgent(CoreSubAgentSource::Review);
        let spawn = CoreSessionSource::SubAgent(CoreSubAgentSource::ThreadSpawn {
            parent_thread_id,
            depth: 1,
            agent_path: None,
            agent_nickname: None,
            agent_role: None,
        });

        assert!(source_kind_matches(
            &review,
            /*thread_source*/ None,
            &[ThreadSourceKind::SubAgentReview]
        ));
        assert!(!source_kind_matches(
            &review,
            /*thread_source*/ None,
            &[ThreadSourceKind::SubAgentThreadSpawn]
        ));
        assert!(source_kind_matches(
            &spawn,
            /*thread_source*/ None,
            &[ThreadSourceKind::SubAgentThreadSpawn]
        ));
        assert!(!source_kind_matches(
            &spawn,
            /*thread_source*/ None,
            &[ThreadSourceKind::SubAgentReview]
        ));
    }

    #[test]
    fn source_filter_hides_subagent_thread_sources_by_default() {
        assert!(!source_filter_allows_thread(
            &CoreSessionSource::Mcp,
            Some(CoreThreadSource::Subagent),
            /*filter*/ None,
            SourceFilterMode::DefaultVisible,
        ));
        assert!(!source_filter_allows_thread(
            &CoreSessionSource::Mcp,
            Some(CoreThreadSource::Subagent),
            Some(&[ThreadSourceKind::AppServer]),
            SourceFilterMode::ExplicitKinds,
        ));
        assert!(source_filter_allows_thread(
            &CoreSessionSource::Mcp,
            Some(CoreThreadSource::Subagent),
            Some(&[ThreadSourceKind::SubAgent]),
            SourceFilterMode::ExplicitKinds,
        ));
        assert!(source_filter_allows_thread(
            &CoreSessionSource::Mcp,
            Some(CoreThreadSource::Subagent),
            /*filter*/ None,
            SourceFilterMode::AllSources,
        ));
    }
}
