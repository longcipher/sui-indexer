//! Factory registration for the Solana adapter.

use std::sync::Arc;

use async_trait::async_trait;
use chain_core::{ChainAdapter, ChainAdapterFactory, ChainError, ChainKind};

use crate::{CommitmentLevel, SolanaAdapter};

/// Factory behind `"solana"` / `"svm"` config keys.
#[derive(Debug, Default)]
pub struct SolanaAdapterFactory {
    /// Commitment reads run at.
    pub commitment: CommitmentLevel,
}

#[async_trait]
impl ChainAdapterFactory for SolanaAdapterFactory {
    fn kind(&self) -> ChainKind {
        ChainKind::Svm
    }

    async fn create(&self, endpoint: String) -> Result<Arc<dyn ChainAdapter>, ChainError> {
        Ok(Arc::new(SolanaAdapter::new(endpoint, self.commitment)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_reports_svm_kind() {
        assert_eq!(SolanaAdapterFactory::default().kind(), ChainKind::Svm);
    }
}
