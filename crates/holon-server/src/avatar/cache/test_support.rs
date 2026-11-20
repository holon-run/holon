use super::{AvatarResources, OwnedSemaphorePermit};

impl AvatarResources {
    #[doc(hidden)]
    pub async fn hold_capacity(&self) -> (OwnedSemaphorePermit, OwnedSemaphorePermit) {
        let builders = self
            .builders
            .clone()
            .acquire_many_owned(super::MAX_BUILDERS as u32)
            .await
            .unwrap();
        let waiters = self
            .waiters
            .clone()
            .acquire_many_owned(super::MAX_WAITERS as u32)
            .await
            .unwrap();
        (builders, waiters)
    }
}
