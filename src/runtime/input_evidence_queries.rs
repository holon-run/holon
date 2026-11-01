//! Finite accepted query metadata. Birth membership never qualifies a body.
use super::*;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct InputQuerySelection {
    pub required_keys: BTreeSet<InputEvidenceKey>,
    pub birth_after: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct InputQueryCoverage {
    pub required_keys: BTreeSet<InputEvidenceKey>,
    pub birth_after: Option<u64>,
    pub birth_cursor: u64,
    // Sequence keys make completeness checkable in one ordered pass without
    // allocating another set; body addresses remain independently qualified.
    pub born: BTreeMap<u64, InputEvidenceKey>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InputQueryError {
    #[error("Input query source was not prepared")]
    NeedsPreparation,
    #[error("Input query birth frontier is incomplete")]
    Incomplete,
    #[error("Input query requested frontier was not prepared")]
    FrontierNotPrepared,
}

impl InputQueryCoverage {
    pub(crate) fn observation_keys(&self) -> impl Iterator<Item = &InputEvidenceKey> {
        self.required_keys.iter().chain(self.born.values())
    }

    pub(crate) fn validate(&self) -> Result<()> {
        match self.birth_after {
            None => ensure!(self.born.is_empty(), InputQueryError::Incomplete),
            Some(after) => {
                ensure!(after <= self.birth_cursor, InputQueryError::Incomplete);
                ensure!(
                    u64::try_from(self.born.len())? == self.birth_cursor - after,
                    InputQueryError::Incomplete
                );
                let mut expected = after;
                for seq in self.born.keys() {
                    expected = expected
                        .checked_add(1)
                        .context(InputQueryError::Incomplete)?;
                    ensure!(*seq == expected, InputQueryError::Incomplete);
                }
                ensure!(expected == self.birth_cursor, InputQueryError::Incomplete);
            }
        }
        Ok(())
    }

    pub(crate) fn validate_selection(&self, selection: &InputQuerySelection) -> Result<()> {
        self.validate()?;
        ensure!(
            selection.required_keys.iter().all(|key| {
                self.required_keys.contains(key) || self.born.values().any(|born| born == key)
            }),
            InputQueryError::NeedsPreparation
        );
        if let Some(after) = selection.birth_after {
            ensure!(
                after <= self.birth_cursor
                    && (after == self.birth_cursor
                        || self.birth_after.is_some_and(|base| base <= after)),
                InputQueryError::FrontierNotPrepared
            );
        }
        Ok(())
    }

    pub(crate) fn for_selection(&self, selection: &InputQuerySelection) -> Result<Self> {
        self.validate_selection(selection)?;
        crate::diagnostics::resource_io::admit_cloned_value(selection)?;
        let mut born = BTreeMap::new();
        if let Some(after) = selection.birth_after {
            for (seq, key) in self
                .born
                .range((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded))
            {
                crate::diagnostics::resource_io::admit_cloned_value(&(seq, key))?;
                born.insert(*seq, key.clone());
            }
        }
        Ok(Self {
            required_keys: selection.required_keys.clone(),
            birth_after: selection.birth_after,
            birth_cursor: self.birth_cursor,
            born,
        })
    }
}

/// Explicit preparation carries its caller's allocation ledger, not authority
/// to read any address outside the original body qualification contract.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct InputQueryRead {
    pub selection: InputQuerySelection,
    #[serde(skip)]
    pub ledger: InputEvidenceBudgetLedger,
}

/// Frozen owner metadata, not a body or mutation capability. It holds no domain
/// map and cannot discover a missing selection during a writer transaction.
#[derive(Debug)]
pub(crate) struct OriginalInputQuerySource {
    scope: InputEvidenceScope,
    base: Arc<InputQueryCoverage>,
    coverage: Arc<InputQueryCoverage>,
    ledger: InputEvidenceBudgetLedger,
    material: Vec<Arc<BudgetViewGuard>>,
    acquisition: crate::diagnostics::resource_io::ResourceReadMaterial,
    _roles: Option<super::super::input_query_roles::InputQueryRoles>,
}

impl OriginalInputQuerySource {
    pub(crate) fn validate_binding(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.scope == *scope && self.ledger.same_ledger(ledger),
            "Input query source differs from original scope/ledger"
        );
        ensure!(
            !self.material.is_empty(),
            "Input query source has no material"
        );
        for guard in &self.material {
            guard.validate_owned_material_binding(scope, ledger, 0)?;
        }
        self.coverage.validate()
    }

    pub(crate) fn coverage(&self) -> &Arc<InputQueryCoverage> {
        &self.coverage
    }

    pub(crate) fn baseline(&self) -> &Arc<InputQueryCoverage> {
        &self.base
    }

    pub(crate) fn material(&self) -> &[Arc<BudgetViewGuard>] {
        &self.material
    }

    pub(crate) fn acquisition(&self) -> &crate::diagnostics::resource_io::ResourceReadMaterial {
        &self.acquisition
    }
}

/// A finite selected view, never a published Root or a full-history cache.
#[derive(Debug)]
pub(crate) struct CapturedBirthInputs {
    pub(crate) query: InputQueryCoverage,
    pub(crate) records: Vec<Arc<QualifiedInputEvidence>>,
    _material: crate::diagnostics::resource_io::ResourceReadMaterial,
    _source: crate::diagnostics::resource_io::ResourceReadMaterial,
    _view: Arc<BudgetViewGuard>,
}

impl CapturedBirthInputs {
    pub(crate) fn validate_binding(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        self._view
            .validate_owned_material_binding(scope, ledger, 0)?;
        self._material.finish()?;
        self._source.finish()?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "input_birth_reads_tests.rs"]
mod birth_read_tests;

impl ResidentInputEvidence {
    /// Product readers select qualified bodies from a complete birth interval.
    /// A poll owns a fresh bounded acquisition, not another spend from the
    /// original writer's allowance. A failed source still cannot be reused.
    pub(crate) fn capture_birth_inputs(
        &self,
        selection: &InputQuerySelection,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<CapturedBirthInputs> {
        let accepted = self.query_coverage()?;
        accepted.validate_selection(selection)?;
        ensure!(
            selection.birth_after.is_some() && selection.required_keys.is_empty(),
            "Input birth reader requires an exact birth role"
        );
        for guard in &self.query_material {
            guard.validate_owned_material_binding(&self.scope, ledger, 0)?;
        }
        ensure!(
            self.budget
                .as_ref()
                .is_some_and(|original| original.same_ledger(ledger)),
            "Input birth reader ledger differs"
        );
        let source = self
            .query_acquisition
            .as_ref()
            .context("Input birth reader omitted original acquisition")?;
        source.finish()?;
        let material =
            crate::diagnostics::resource_io::ResourceReadMaterial::new(ledger, &self.scope)?;
        let (query, records, view) = material.read(|| {
            let query = accepted.for_selection(selection)?;
            let count = query
                .born
                .values()
                .filter(|key| {
                    matches!(
                        key.family,
                        InputEvidenceFamily::Message | InputEvidenceFamily::ToolExecution
                    )
                })
                .count();
            crate::diagnostics::resource_io::admit_owned_metadata(
                u64::try_from(
                    count
                        .checked_mul(std::mem::size_of::<Arc<QualifiedInputEvidence>>())
                        .context("Input birth vector size overflow")?,
                )?,
                count,
            )?;
            let mut records = Vec::with_capacity(count);
            for key in query.born.values().filter(|key| {
                matches!(
                    key.family,
                    InputEvidenceFamily::Message | InputEvidenceFamily::ToolExecution
                )
            }) {
                if let Some(record) = self.records.get(key) {
                    ensure!(
                        record.locator.scope == self.scope
                            && record.locator.key == *key
                            && record.locator.revision > 0,
                        "Input birth body locator differs"
                    );
                    records.push(record.clone());
                } else if !self.coverage.unavailable.contains(key) {
                    return Err(InputEvidenceReadError::NeedsPreparation(key.clone()).into());
                }
            }
            let view = ledger.register_view(&self.scope, InputMemoryCategory::Pins, &records)?;
            Ok((query, records, view))
        })?;
        Ok(CapturedBirthInputs {
            query,
            records,
            _material: material,
            _source: source.clone(),
            _view: view,
        })
    }

    pub(crate) fn query_coverage(&self) -> Result<&InputQueryCoverage> {
        self.query
            .as_deref()
            .ok_or_else(|| InputQueryError::NeedsPreparation.into())
    }

    pub(crate) fn original_query_source_for_roles(
        &self,
        roles: super::super::input_query_roles::InputQueryRoles,
    ) -> Result<Option<OriginalInputQuerySource>> {
        roles.validate_binding(
            &self.scope,
            self.budget
                .as_ref()
                .context("Input query source has no ledger")?,
        )?;
        let Some(mut source) = self.original_query_source()? else {
            ensure!(
                roles.selection == InputQuerySelection::default(),
                InputQueryError::NeedsPreparation
            );
            return Ok(None);
        };
        source.coverage = source.acquisition.read(|| {
            let coverage = source.base.for_selection(&roles.selection)?;
            crate::diagnostics::resource_io::charge_owned_metadata(
                u64::try_from(
                    std::mem::size_of::<InputQueryCoverage>() + 2 * std::mem::size_of::<usize>(),
                )?,
                1,
            )?;
            Ok(Arc::new(coverage))
        })?;
        source._roles = Some(roles);
        Ok(Some(source))
    }

    pub(crate) fn original_query_source(&self) -> Result<Option<OriginalInputQuerySource>> {
        let Some(coverage) = &self.query else {
            return Ok(None);
        };
        let ledger = self
            .budget
            .as_ref()
            .context("Input query source has no ledger")?;
        // A new owner transaction is a new acquisition, not another spend
        // against the previous cut. Its source must still have succeeded.
        self.query_acquisition
            .as_ref()
            .context("Input query source has no acquisition")?
            .finish()?;
        let acquisition =
            crate::diagnostics::resource_io::ResourceReadMaterial::new(ledger, &self.scope)?;
        acquisition.read(|| {
            crate::diagnostics::resource_io::admit_cloned_value(&self.scope)?;
            let bytes = self
                .query_material
                .len()
                .checked_mul(std::mem::size_of::<Arc<BudgetViewGuard>>())
                .and_then(|bytes| {
                    bytes.checked_add(std::mem::size_of::<OriginalInputQuerySource>())
                })
                .context("Input query source material overflow")?;
            crate::diagnostics::resource_io::charge_owned_metadata(
                u64::try_from(bytes)?,
                self.query_material.len(),
            )?;
            let source = OriginalInputQuerySource {
                scope: self.scope.clone(),
                base: coverage.clone(),
                coverage: coverage.clone(),
                ledger: ledger.clone(),
                material: self.query_material.clone(),
                acquisition: acquisition.clone(),
                _roles: None,
            };
            source.validate_binding(&self.scope, ledger)?;
            Ok(Some(source))
        })
    }
}
