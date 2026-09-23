//! Window flow: encode → native rule → rows, with KV state and rollback.

use rule_host::{ABI_VERSION, Kv as _, MemoryKv, NativeRule, Rule as _, RuleSpec, WindowInput};

fn spec() -> RuleSpec {
    RuleSpec {
        name: "sandwich".to_owned(),
        version: 3,
        abi_version: ABI_VERSION,
    }
}

#[tokio::test]
async fn stateless_rule_emits_one_row_per_event() {
    let rule = NativeRule::new(spec(), |input| {
        Ok(input
            .events
            .iter()
            .map(|ev| chain_core::OutRow {
                height: ev.height,
                rule_version: 3,
                commitment: 1,
                values: serde_json::json!({ "emitter": ev.emitter }),
            })
            .collect())
    });
    let input = WindowInput {
        abi_version: ABI_VERSION,
        events: vec![sample_ev(10), sample_ev(11)],
        blocks: Vec::new(),
        feeds: Vec::new(),
        window_lo: 10,
        window_hi: 12,
    };
    let rows = rule.on_window(&input).await.expect("run");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].height, 10);
    assert_eq!(rows[0].rule_version, 3);
}

#[test]
fn kv_state_rolls_back_with_the_fork() {
    let mut kv = MemoryKv::new();
    kv.put("pool".to_owned(), 10, b"r10".to_vec());
    kv.put("pool".to_owned(), 30, b"r30".to_vec());
    assert_eq!(kv.get("pool", 25), Some(b"r10".to_vec()));
    kv.rollback_above(20);
    assert_eq!(kv.get("pool", 30), Some(b"r10".to_vec()));
    assert_eq!(kv.max_height(), Some(10));
}

fn sample_ev(height: u64) -> chain_core::Ev {
    chain_core::Ev {
        height,
        block_ts: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap_or_default(),
        tx_index: 0,
        ev_index: 0,
        inner_ix: 0,
        stack_height: 0,
        emitter: "prog".to_owned(),
        topics: vec!["swap".to_owned()],
        payload: vec![1, 2, 3],
        tx_hash: vec![4, 5],
        sender: "s".to_owned(),
        extra: serde_json::json!({}),
    }
}
