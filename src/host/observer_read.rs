//! Observer inventory has a Host accounting scope, not Agent body authority.
use super::*;
use crate::diagnostics::resource_io::{self, ResourceReadMaterial};

impl RuntimeHost {
    pub(crate) fn observer_inventory_material(&self) -> Result<ResourceReadMaterial> {
        // The durable Runtime ID is not an Agent identity. This synthetic
        // accounting scope never supplies a domain qualification or token.
        ResourceReadMaterial::current_or_new_for_identity(
            &self.inner.input_evidence_budget,
            &self.runtime_db().runtime_id()?,
            1,
        )
    }
}

pub(crate) struct ObserverWire {
    bytes: Vec<u8>,
    _material: ResourceReadMaterial,
}

impl AsRef<[u8]> for ObserverWire {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

pub(crate) fn observer_json_bytes<T: serde::Serialize>(
    value: &T,
    material: ResourceReadMaterial,
) -> Result<tokio_util::bytes::Bytes> {
    let bytes = material.read(|| {
        let size = crate::runtime::input_evidence_budget::serialized_bytes(value)?;
        resource_io::charge_owned_payload(size)?;
        resource_io::charge_owned_metadata(
            u64::try_from(std::mem::size_of::<ObserverWire>() + 128)?,
            1,
        )?;
        let mut bytes = vec![0; usize::try_from(size)?];
        let mut writer = std::io::Cursor::new(bytes.as_mut_slice());
        serde_json::to_writer(&mut writer, value)?;
        anyhow::ensure!(writer.position() == size, "observer JSON size changed");
        Ok(bytes)
    })?;
    Ok(tokio_util::bytes::Bytes::from_owner(ObserverWire {
        bytes,
        _material: material,
    }))
}

pub(crate) fn observer_hint_bytes(
    agent_id: &str,
    material: ResourceReadMaterial,
) -> Result<tokio_util::bytes::Bytes> {
    use std::io::Write;
    #[derive(serde::Serialize)]
    struct Hint<'a> {
        agent_id: &'a str,
    }
    let value = Hint { agent_id };
    let bytes = material.read(|| {
        let size = crate::runtime::input_evidence_budget::serialized_bytes(&value)?
            .checked_add(u64::try_from(
                b"event: agent_roster_hint\ndata: \n\n".len(),
            )?)
            .context("observer hint size overflow")?;
        resource_io::charge_owned_payload(size)?;
        resource_io::charge_owned_metadata(
            u64::try_from(std::mem::size_of::<ObserverWire>() + 128)?,
            1,
        )?;
        let mut bytes = vec![0; usize::try_from(size)?];
        let mut writer = std::io::Cursor::new(bytes.as_mut_slice());
        writer.write_all(b"event: agent_roster_hint\ndata: ")?;
        serde_json::to_writer(&mut writer, &value)?;
        writer.write_all(b"\n\n")?;
        anyhow::ensure!(writer.position() == size, "observer hint size changed");
        Ok(bytes)
    })?;
    Ok(tokio_util::bytes::Bytes::from_owner(ObserverWire {
        bytes,
        _material: material,
    }))
}
