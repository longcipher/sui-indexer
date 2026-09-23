//! Chain registry: config data selects the adapter.
//!
//! The chain kind is config data (`[chain] kind = "solana"`), so adding a
//! chain never touches the sync engine, the job engine or the query layer.

use std::sync::Arc;

use chain_core::{ChainAdapter, ChainKind, ChainRegistry};
use eyre::Result;

/// Build the process registry: Move today, SVM and EVM as they land.
#[must_use]
pub fn chain_registry() -> ChainRegistry {
    let mut registry = ChainRegistry::new();
    registry.register(Arc::new(adapter_move::MoveAdapterFactory));
    registry.register(Arc::new(adapter_svm::SolanaAdapterFactory::default()));
    registry
}

/// Stable chain key for tables, metrics and logs: explicit
/// `[chain] chain_id`, else `{kind}-{network}`.
pub fn chain_id(config: &sui_indexer_config::IndexerConfig) -> String {
    if config.chain.chain_id.trim().is_empty() {
        let kind = ChainKind::parse(&config.chain.kind).unwrap_or(ChainKind::Move);
        format!("{}-{}", kind.as_str(), config.network.network)
    } else {
        config.chain.chain_id.clone()
    }
}

/// Resolve `(kind, chain_id, endpoint)` from config.
///
/// `chain.endpoint` wins when set; otherwise the legacy `[network]`
/// endpoint serves Move, and SVM requires an explicit endpoint.
pub fn resolve_chain(
    config: &sui_indexer_config::IndexerConfig,
) -> Result<(ChainKind, String, String)> {
    let kind = ChainKind::parse(&config.chain.kind)
        .ok_or_else(|| eyre::eyre!("unknown [chain] kind: {}", config.chain.kind))?;
    let chain_id = if config.chain.chain_id.trim().is_empty() {
        format!("{}-{}", kind.as_str(), config.network.network)
    } else {
        config.chain.chain_id.clone()
    };
    let endpoint = if config.chain.endpoint.trim().is_empty() {
        match kind {
            ChainKind::Move => config.network.grpc_url.to_string(),
            ChainKind::Svm | ChainKind::Evm => {
                return Err(eyre::eyre!("[chain] endpoint is required for kind {kind}"));
            }
        }
    } else {
        config.chain.endpoint.clone()
    };
    Ok((kind, chain_id, endpoint))
}

/// Build the configured adapter (connects + probes the tip).
pub async fn build_adapter(
    config: &sui_indexer_config::IndexerConfig,
) -> Result<Arc<dyn ChainAdapter>> {
    let (kind, _chain_id, endpoint) = resolve_chain(config)?;
    if kind == ChainKind::Svm {
        let commitment = match config.chain.commitment.as_str() {
            "finalized" => adapter_svm::CommitmentLevel::Finalized,
            "confirmed" => adapter_svm::CommitmentLevel::Confirmed,
            other => {
                return Err(eyre::eyre!(
                    "unknown [chain] commitment for svm: {other} (want confirmed|finalized)"
                ));
            }
        };
        return Ok(Arc::new(adapter_svm::SolanaAdapter::new(
            endpoint, commitment,
        )));
    }
    chain_registry()
        .create(kind, endpoint)
        .await
        .map_err(|e| eyre::eyre!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_move_factory() {
        let registry = chain_registry();
        assert!(registry.get(ChainKind::Move).is_some());
        assert!(registry.get(ChainKind::Svm).is_some());
    }

    #[test]
    fn move_defaults_to_legacy_grpc_endpoint() {
        let config = sui_indexer_config::IndexerConfig::default();
        let (kind, chain_id, endpoint) = resolve_chain(&config).expect("resolve");
        assert_eq!(kind, ChainKind::Move);
        assert_eq!(chain_id, "sui-testnet");
        assert!(endpoint.contains("fullnode.testnet.sui.io"));
    }

    #[test]
    fn svm_requires_explicit_endpoint() {
        let mut config = sui_indexer_config::IndexerConfig::default();
        config.chain.kind = "svm".to_owned();
        assert!(resolve_chain(&config).is_err());
        config.chain.endpoint = "https://api.mainnet-beta.solana.com".to_owned();
        let (kind, _, _) = resolve_chain(&config).expect("resolve");
        assert_eq!(kind, ChainKind::Svm);
    }

    #[test]
    fn chain_id_prefers_explicit_over_derived() {
        let mut config = sui_indexer_config::IndexerConfig::default();
        assert_eq!(chain_id(&config), "sui-testnet");
        config.chain.chain_id = "custom-1".to_owned();
        assert_eq!(chain_id(&config), "custom-1");
        config.chain.kind = "svm".to_owned();
        config.chain.chain_id = String::new();
        assert_eq!(chain_id(&config), "svm-testnet");
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let mut config = sui_indexer_config::IndexerConfig::default();
        config.chain.kind = "cosmos".to_owned();
        assert!(resolve_chain(&config).is_err());
    }

    #[tokio::test]
    async fn bogus_svm_commitment_is_rejected() {
        let mut config = sui_indexer_config::IndexerConfig::default();
        config.chain.kind = "svm".to_owned();
        config.chain.endpoint = "http://127.0.0.1:1".to_owned();
        config.chain.commitment = "eventually".to_owned();
        assert!(build_adapter(&config).await.is_err());
    }

    #[tokio::test]
    async fn move_adapter_needs_a_live_endpoint() {
        // Loopback endpoint: the gRPC probe fails fast and deterministically.
        let mut config = sui_indexer_config::IndexerConfig::default();
        config.chain.endpoint = "http://127.0.0.1:1".to_owned();
        assert!(build_adapter(&config).await.is_err());
    }

    #[tokio::test]
    async fn svm_commitment_follows_config() {
        let mut config = sui_indexer_config::IndexerConfig::default();
        config.chain.kind = "svm".to_owned();
        config.chain.endpoint = "http://127.0.0.1:1".to_owned();
        config.chain.commitment = "finalized".to_owned();
        let adapter = build_adapter(&config).await.expect("svm builds");
        assert_eq!(adapter.kind(), ChainKind::Svm);
        assert_eq!(adapter.commitment(), chain_core::CommitmentModel::Final);

        config.chain.commitment = "confirmed".to_owned();
        let adapter = build_adapter(&config).await.expect("svm builds");
        assert_eq!(
            adapter.commitment(),
            chain_core::CommitmentModel::Reorgable { max_depth: 128 }
        );
    }
}
