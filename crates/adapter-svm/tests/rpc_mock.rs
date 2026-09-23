//! Solana JSON-RPC against a scripted mock: slots, blocks, skipped slots.

use adapter_svm::{CommitmentLevel, RpcClient, SolanaAdapter};
use chain_core::ChainAdapter;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

async fn serve(listener: tokio::net::TcpListener) {
    loop {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(&mut socket);
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).await.is_err() {
                return;
            }
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.is_err() || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            if content_length > 0 {
                let _ = reader.read_exact(&mut body).await;
            }
            let request: serde_json::Value =
                serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
            let response = mock_response(&request);
            let text = response.to_string();
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = reader.into_inner().write_all(reply.as_bytes()).await;
        });
    }
}

fn mock_response(request: &serde_json::Value) -> serde_json::Value {
    let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = request.get("id").cloned().unwrap_or(serde_json::json!(1));
    if method == "getBlock"
        && request
            .get("params")
            .and_then(|p| p.get(0))
            .and_then(|s| s.as_u64())
            == Some(102)
    {
        // Real nodes report errors at the envelope top level.
        return serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32004, "message": "missing" },
        });
    }
    let result = match method {
        "getSlot" => serde_json::json!(100),
        "getBlock" => {
            let slot = request
                .get("params")
                .and_then(|p| p.get(0))
                .and_then(|s| s.as_u64())
                .unwrap_or(0);
            match slot {
                100 => serde_json::json!({
                    "blockhash": "H",
                    "previousBlockhash": "P",
                    "parentSlot": 99,
                    "blockTime": 1_700_000_000,
                    "transactions": [{
                        "meta": {
                            "err": null,
                            "fee": 5000,
                            "innerInstructions": [],
                            "logMessages": ["Program 111 invoke [1]"],
                        },
                        "transaction": {
                            "signatures": ["sig"],
                            "message": {
                                "accountKeys": ["payer", "prog"],
                                "instructions": [{
                                    "programIdIndex": 1,
                                    "accounts": [],
                                    "data": bs58::encode([7u8; 10]).into_string(),
                                }],
                            },
                        },
                    }],
                }),
                102 => serde_json::Value::Null,
                _ => serde_json::Value::Null,
            }
        }
        _ => serde_json::Value::Null,
    };
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

async fn mock_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(serve(listener));
    url
}

#[tokio::test]
async fn slot_block_and_skipped_round_trip() {
    let url = mock_url().await;
    let rpc = RpcClient::new(url.clone());
    assert_eq!(rpc.endpoint(), url);
    assert_eq!(
        rpc.get_slot(CommitmentLevel::Confirmed)
            .await
            .expect("slot"),
        100
    );
    let block = rpc
        .get_block(100, CommitmentLevel::Confirmed)
        .await
        .expect("block")
        .expect("present");
    assert_eq!(block.parent_slot, 99);
    assert_eq!(block.transactions.len(), 1);
    // Skipped slots are markers, not errors.
    assert!(
        rpc.get_block(101, CommitmentLevel::Confirmed)
            .await
            .expect("skipped")
            .is_none()
    );
    assert!(
        rpc.get_block(102, CommitmentLevel::Confirmed)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn adapter_head_and_fetch_use_the_rpc() {
    use chain_core::Commitment;
    use std::sync::Arc;
    let url = mock_url().await;
    let adapter: Arc<dyn ChainAdapter> =
        Arc::new(SolanaAdapter::new(url, CommitmentLevel::Confirmed));
    assert_eq!(adapter.head().await.expect("head"), 100);
    let blocks = adapter.fetch(100..101).await.expect("fetch");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].height, 100);
    assert_eq!(blocks[0].events.len(), 1);
    assert_eq!(blocks[0].commitment, Commitment::Confirmed);
    let skipped = adapter.fetch(101..102).await.expect("fetch");
    assert!(skipped[0].skipped);
}

#[tokio::test]
async fn finalized_reads_decode_as_final() {
    use chain_core::Commitment;
    use std::sync::Arc;
    let url = mock_url().await;
    let adapter: Arc<dyn ChainAdapter> =
        Arc::new(SolanaAdapter::new(url, CommitmentLevel::Finalized));
    let blocks = adapter.fetch(100..101).await.expect("fetch");
    assert_eq!(blocks[0].commitment, Commitment::Final);
}
