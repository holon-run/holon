use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use serde::de::{EnumAccess, VariantAccess, Visitor};
use std::fmt;

fn old_decode<T: serde::de::DeserializeOwned>(value: &str, label: &str) -> Result<T> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .with_context(|| format!("decoding {label}"))
}

macro_rules! each_enum {
    ($check:ident) => {
        $check(&[
            ("legacy_spawn", AgentLineageCreationCause::LegacySpawn),
            ("create_agent", AgentLineageCreationCause::CreateAgent),
            ("migration", AgentLineageCreationCause::Migration),
        ])?;
        $check(&[
            ("active", AgentSupervisionState::Active),
            ("cleanup_required", AgentSupervisionState::CleanupRequired),
            ("closed", AgentSupervisionState::Closed),
        ])?;
        $check(&[
            ("persistent", AgentCanonicalDurability::Persistent),
            ("ephemeral", AgentCanonicalDurability::Ephemeral),
        ])?;
        $check(&[
            ("independent", AgentLifecycleAttachment::Independent),
            (
                "supervision_attached",
                AgentLifecycleAttachment::SupervisionAttached,
            ),
        ])?;
        $check(&[
            ("core_agent", AgentCapabilityFamily::CoreAgent),
            ("local_environment", AgentCapabilityFamily::LocalEnvironment),
            ("web", AgentCapabilityFamily::Web),
            ("agent_creation", AgentCapabilityFamily::AgentCreation),
            (
                "authority_expanding",
                AgentCapabilityFamily::AuthorityExpanding,
            ),
            ("external_trigger", AgentCapabilityFamily::ExternalTrigger),
        ])?;
        $check(&[
            ("allow", AgentPolicyEffect::Allow),
            ("deny", AgentPolicyEffect::Deny),
        ])?;
        $check(&[
            ("operator", AgentMessagePrincipalKind::Operator),
            (
                "supervising_parent",
                AgentMessagePrincipalKind::SupervisingParent,
            ),
            ("peer_agent", AgentMessagePrincipalKind::PeerAgent),
            (
                "external_ingress",
                AgentMessagePrincipalKind::ExternalIngress,
            ),
            (
                "runtime_capability",
                AgentMessagePrincipalKind::RuntimeCapability,
            ),
        ])?;
    };
}

#[test]
fn all_relation_enum_variants_equal_old_decoder_and_wire_value() -> Result<()> {
    fn check<T>(variants: &[(&str, T)]) -> Result<()>
    where
        T: serde::de::DeserializeOwned + serde::Serialize + Copy + PartialEq + fmt::Debug,
    {
        for &(text, expected) in variants {
            let decoded = decode_enum::<T>(text, "relation enum")?;
            assert_eq!(decoded, expected);
            assert_eq!(decoded, old_decode::<T>(text, "relation enum")?);
            assert_eq!(serde_json::to_value(decoded)?, text);
        }
        Ok(())
    }
    each_enum!(check);
    Ok(())
}

#[test]
fn unknown_relation_enum_errors_and_context_equal_old_decoder() -> Result<()> {
    fn check<T>(_: &[(&str, T)]) -> Result<()>
    where
        T: serde::de::DeserializeOwned + Copy + fmt::Debug,
    {
        for text in [
            "",
            "UNKNOWN",
            " active ",
            "\"active\"",
            "null",
            "{}",
            "[]",
            "汉字\0\"\\\n",
            "\\u0061ctive",
        ] {
            let old = old_decode::<T>(text, "relation enum").unwrap_err();
            let new = decode_enum::<T>(text, "relation enum").unwrap_err();
            assert_eq!(format!("{new:#}"), format!("{old:#}"), "{text:?}");
            let old = old.downcast_ref::<serde_json::Error>().unwrap();
            let new = new.downcast_ref::<serde_json::Error>().unwrap();
            assert_eq!(new.classify(), old.classify());
            assert_eq!((new.line(), new.column()), (old.line(), old.column()));
        }
        Ok(())
    }
    each_enum!(check);
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct BorrowedVariant(usize, usize);

impl<'de> serde::Deserialize<'de> for BorrowedVariant {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct BorrowedToken(usize, usize);
        impl<'de> serde::Deserialize<'de> for BorrowedToken {
            fn deserialize<D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> std::result::Result<Self, D::Error> {
                struct TokenVisitor;
                impl<'de> Visitor<'de> for TokenVisitor {
                    type Value = BorrowedToken;

                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("a borrowed variant identifier")
                    }

                    fn visit_borrowed_str<E: serde::de::Error>(
                        self,
                        value: &'de str,
                    ) -> std::result::Result<Self::Value, E> {
                        Ok(BorrowedToken(value.as_ptr() as usize, value.len()))
                    }
                }
                deserializer.deserialize_identifier(TokenVisitor)
            }
        }
        struct EnumVisitor;
        impl<'de> Visitor<'de> for EnumVisitor {
            type Value = BorrowedVariant;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an enum whose variant borrows its source")
            }

            fn visit_enum<A: EnumAccess<'de>>(
                self,
                data: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let (variant, unit) = data.variant::<BorrowedToken>()?;
                unit.unit_variant()?;
                Ok(BorrowedVariant(variant.0, variant.1))
            }
        }
        deserializer.deserialize_enum("BorrowedVariant", &[], EnumVisitor)
    }
}

#[test]
fn relation_enum_variant_borrows_original_input_not_a_json_string() -> Result<()> {
    let source = String::from("probe-汉字\0\"\\\n");
    let value = decode_enum::<BorrowedVariant>(&source, "probe")?;
    assert_eq!((value.0, value.1), (source.as_ptr() as usize, source.len()));
    assert!(old_decode::<BorrowedVariant>(&source, "probe").is_err());
    Ok(())
}

#[test]
fn relation_enum_decoding_uses_no_extra_original_phase_quota() -> Result<()> {
    let scope = InputEvidenceScope {
        agent_id: "relation-enum".into(),
        identity_incarnation: 1,
    };
    let exact = ((scope.agent_id.len() as u64 + 256) * 8 + 13 * 8, 64 + 8);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, exact.0, exact.1)?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(13, 1)?;
        fn check<T>(variants: &[(&str, T)]) -> Result<()>
        where
            T: serde::de::DeserializeOwned + Copy + PartialEq + fmt::Debug,
        {
            for &(text, expected) in variants {
                assert_eq!(decode_enum::<T>(text, "relation enum")?, expected);
            }
            Ok(())
        }
        each_enum!(check);
    }
    assert_eq!(phase.finish()?, exact);
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
