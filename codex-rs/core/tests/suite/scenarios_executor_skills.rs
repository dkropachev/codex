//! Explicit cloud catalogs stay independent of ambient executor skill listings.
//! These scenarios still use production environment-bound reads and searches.

use codex_skills_extension::ExecutorSkillProvider;
use codex_skills_extension::SkillProvider;
use codex_skills_extension::catalog::SkillCatalog;
use codex_skills_extension::catalog::SkillReadResult;
use codex_skills_extension::catalog::SkillSearchResult;
use codex_skills_extension::provider::SkillListQuery;
use codex_skills_extension::provider::SkillProviderFuture;
use codex_skills_extension::provider::SkillReadRequest;
use codex_skills_extension::provider::SkillSearchRequest;

pub(super) struct ScenarioExecutorSkills(pub(super) ExecutorSkillProvider);

impl SkillProvider for ScenarioExecutorSkills {
    fn uses_mcp_resources(&self) -> bool {
        self.0.uses_mcp_resources()
    }

    fn list(&self, _query: SkillListQuery) -> SkillProviderFuture<'_, SkillCatalog> {
        Box::pin(async { Ok(SkillCatalog::default()) })
    }

    fn read<'a>(
        &'a self,
        request: SkillReadRequest<'a>,
    ) -> SkillProviderFuture<'a, SkillReadResult> {
        self.0.read(request)
    }

    fn search(&self, request: SkillSearchRequest) -> SkillProviderFuture<'_, SkillSearchResult> {
        self.0.search(request)
    }
}
