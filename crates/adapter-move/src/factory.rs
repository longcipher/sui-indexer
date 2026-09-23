//! Factory registration for the Move adapter.

use std::sync::Arc;

use async_trait::async_trait;
use chain_core::{ChainAdapter, ChainAdapterFactory, ChainError, ChainKind};

use crate::MoveAdapter;

/// Factory behind `"sui"` / `"move"` config keys.
pub struct MoveAdapterFactory;

#[async_trait]
impl ChainAdapterFactory for MoveAdapterFactory {
    fn kind(&self) -> ChainKind {
        ChainKind::Move
    }

    async fn create(&self, endpoint: String) -> Result<Arc<dyn ChainAdapter>, ChainError> {
        Ok(Arc::new(MoveAdapter::new(endpoint).await?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_reports_move_kind() {
        assert_eq!(MoveAdapterFactory.kind(), ChainKind::Move);
    }
}
