use super::*;

pub fn collect_discovered_roots_from_registrations(
    io: &'static dyn PackageIo,
    registrations: &[SkillRootRegistration],
) -> Result<Vec<SkillRootView>> {
    super::collect_discovered_roots_from_registrations(io, registrations)
}

pub fn retain_active_skills_for_catalog(
    io: &'static dyn PackageIo,
    catalog: &[SkillCatalogEntry],
    active_skills: &[ActiveSkillRecord],
) -> Result<Vec<ActiveSkillRecord>> {
    super::retain_active_skills_for_catalog(io, catalog, active_skills)
}
