//! [`ChainAdapter`]: the contract is decoded rows, not raw blocks.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use async_trait::async_trait;

use crate::{ChainKind, ChainSchema, CommitmentModel, DecodedBlock, ParentRef};

/// Batched fetch *and* decode. The engine never sees a chain-native type.
///
/// Every `decode_block` / `decode_transaction` / `decode_log` call site
/// collapses into a single [`ChainAdapter::fetch`] call, which makes gap
/// filling, reorg handling, pruning and tiering chain-independent.
#[async_trait]
pub trait ChainAdapter: Send + Sync {
    /// Which chain family this adapter serves.
    fn kind(&self) -> ChainKind;

    /// Reorg behaviour of the underlying chain.
    fn commitment(&self) -> CommitmentModel;

    /// Native tables plus column descriptors contributed by this adapter.
    fn schema(&self) -> &ChainSchema;

    /// Current tip height (block number / slot / checkpoint sequence).
    async fn head(&self) -> Result<u64, crate::ChainError>;

    /// Fetch *and* decode `[range.start, range.end)`. Skipped heights
    /// (Solana) come back as [`DecodedBlock::skipped`] markers.
    async fn fetch(&self, range: Range<u64>) -> Result<Vec<DecodedBlock>, crate::ChainError>;

    /// Parent reference used for fork detection.
    fn parent_ref(&self, block: &DecodedBlock) -> ParentRef {
        block.parent.clone()
    }
}

/// Factory that builds an adapter from an endpoint string.
#[async_trait]
pub trait ChainAdapterFactory: Send + Sync {
    /// Chain family this factory builds.
    fn kind(&self) -> ChainKind;

    /// Connect to `endpoint` and return a ready adapter.
    async fn create(&self, endpoint: String) -> Result<Arc<dyn ChainAdapter>, crate::ChainError>;
}

/// Registry mapping [`ChainKind`] to its factory.
///
/// The chain kind is config data (`[chain] kind = "solana"`), so adding a
/// chain never touches the sync engine, the job engine or the query layer.
#[derive(Default)]
pub struct ChainRegistry {
    factories: HashMap<ChainKind, Arc<dyn ChainAdapterFactory>>,
}

impl ChainRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    /// Register one factory (overwrites any previous entry for the kind).
    pub fn register(&mut self, factory: Arc<dyn ChainAdapterFactory>) {
        self.factories.insert(factory.kind(), factory);
    }

    /// Look up the factory for `kind`.
    #[must_use]
    pub fn get(&self, kind: ChainKind) -> Option<Arc<dyn ChainAdapterFactory>> {
        self.factories.get(&kind).cloned()
    }

    /// Build an adapter for `kind` against `endpoint`.
    pub async fn create(
        &self,
        kind: ChainKind,
        endpoint: String,
    ) -> Result<Arc<dyn ChainAdapter>, crate::ChainError> {
        let factory = self
            .get(kind)
            .ok_or(crate::ChainError::NoAdapter(kind.as_str().to_owned()))?;
        factory.create(endpoint).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubAdapter;
    struct StubFactory;

    #[async_trait]
    impl ChainAdapter for StubAdapter {
        fn kind(&self) -> ChainKind {
            ChainKind::Move
        }

        fn commitment(&self) -> CommitmentModel {
            CommitmentModel::Final
        }

        fn schema(&self) -> &ChainSchema {
            static SCHEMA: std::sync::OnceLock<ChainSchema> = std::sync::OnceLock::new();
            SCHEMA.get_or_init(ChainSchema::default)
        }

        async fn head(&self) -> Result<u64, crate::ChainError> {
            Ok(7)
        }

        async fn fetch(&self, range: Range<u64>) -> Result<Vec<DecodedBlock>, crate::ChainError> {
            Ok(range.map(DecodedBlock::skipped).collect())
        }
    }

    #[async_trait]
    impl ChainAdapterFactory for StubFactory {
        fn kind(&self) -> ChainKind {
            ChainKind::Move
        }

        async fn create(
            &self,
            _endpoint: String,
        ) -> Result<Arc<dyn ChainAdapter>, crate::ChainError> {
            Ok(Arc::new(StubAdapter))
        }
    }

    #[tokio::test]
    async fn registry_creates_registered_kind_and_rejects_unknown() {
        let mut registry = ChainRegistry::new();
        registry.register(Arc::new(StubFactory));
        let adapter = registry
            .create(ChainKind::Move, "endpoint".to_owned())
            .await
            .expect("registered kind builds");
        assert_eq!(adapter.head().await.expect("head"), 7);
        assert!(
            registry
                .create(ChainKind::Svm, "x".to_owned())
                .await
                .is_err()
        );
    }

    #[test]
    fn default_parent_ref_forwards_the_block_parent() {
        let adapter = StubAdapter;
        let block = DecodedBlock {
            height: 1,
            hash: vec![1],
            parent: crate::ParentRef::from_hash(vec![9]),
            ts: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap_or_default(),
            commitment: crate::Commitment::Final,
            rows: crate::CoreRows::default(),
            events: Vec::new(),
            native: Vec::new(),
            skipped: false,
        };
        // StubAdapter does not override `parent_ref`: the default must
        // forward the block's own parent link.
        assert_eq!(adapter.parent_ref(&block).hash, vec![9]);
    }
}
