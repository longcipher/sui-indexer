//! Shared sample constructors for unit tests.

use sui_json_rpc_types::SuiEvent;

/// Build a minimal `SuiEvent` with the given identity.
pub fn sample_event(
    seq: u64,
    package: &str,
    module: &str,
    name: &str,
    parsed_json: serde_json::Value,
) -> SuiEvent {
    SuiEvent {
        id: sui_types::event::EventID {
            tx_digest: sui_types::base_types::TransactionDigest::new([seq as u8; 32]),
            event_seq: seq,
        },
        package_id: package.parse().expect("package"),
        transaction_module: module.parse().expect("module"),
        sender: "0x0000000000000000000000000000000000000000000000000000000000000001"
            .parse()
            .expect("sender"),
        type_: format!("{package}::{module}::{name}")
            .parse()
            .expect("type"),
        parsed_json,
        bcs: sui_json_rpc_types::BcsEvent::new(vec![1, 2, 3]),
        timestamp_ms: Some(1_000),
    }
}

/// Navi lending `DepositEvent` carrying amount/asset/user/pool fields.
pub fn navi_deposit_event() -> SuiEvent {
    sample_event(
        7,
        "0xa99b8952d4f7d947ea77fe0ecdcc9e5fc0bcab2841d6e2a5aa00c3044e5544b5",
        "lending",
        "DepositEvent",
        serde_json::json!({
            "amount": 1_000,
            "asset_id": "0x2::sui::SUI",
            "user": "0xabc",
            "pool_id": "0xpool",
        }),
    )
}
