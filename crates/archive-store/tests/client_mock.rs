//! ClickHouse client against a scripted mock HTTP server: success, retry,
//! chunking and deduplication without a live ClickHouse.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use archive_store::ClickHouseClient;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

struct Recorded {
    target: String,
    body: Vec<u8>,
}

struct Mock {
    responses: Mutex<VecDeque<(u16, &'static str)>>,
    requests: Mutex<Vec<Recorded>>,
}

impl Mock {
    fn new(responses: Vec<(u16, &'static str)>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn pop(&self) -> (u16, &'static str) {
        self.responses
            .lock()
            .expect("mutex")
            .pop_front()
            .unwrap_or((200, ""))
    }

    fn record(&self, target: String, body: Vec<u8>) {
        self.requests
            .lock()
            .expect("mutex")
            .push(Recorded { target, body });
    }

    fn count(&self) -> usize {
        self.requests.lock().expect("mutex").len()
    }
}

async fn serve(listener: tokio::net::TcpListener, mock: Arc<Mock>) {
    loop {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mock = mock.clone();
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
            let target = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_owned();
            mock.record(target, body);
            let (status, text) = mock.pop();
            let reason = if status == 200 { "OK" } else { "ERROR" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = reader.into_inner().write_all(response.as_bytes()).await;
        });
    }
}

async fn mock_server(responses: Vec<(u16, &'static str)>) -> (String, Arc<Mock>) {
    let mock = Mock::new(responses);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(serve(listener, mock.clone()));
    (url, mock)
}

fn client(url: &str, chunk: usize, retries: u32) -> ClickHouseClient {
    ClickHouseClient::new(url.to_owned(), "db".to_owned(), chunk, retries).expect("client")
}

#[tokio::test]
async fn query_returns_body() {
    let (url, _) = mock_server(vec![(200, "1")]).await;
    let body = client(&url, 100, 3).query("SELECT 1").await.expect("query");
    assert_eq!(body, "1");
}

#[tokio::test]
async fn query_retries_then_fails() {
    let (url, mock) = mock_server(vec![(500, "boom"), (500, "boom")]).await;
    let result = client(&url, 100, 2).query("SELECT 1").await;
    assert!(result.is_err());
    assert_eq!(mock.count(), 2);
}

#[tokio::test]
async fn ping_fails_when_server_errors() {
    let (url, _) = mock_server(vec![(500, "down"), (500, "down")]).await;
    assert!(client(&url, 100, 2).ping().await.is_err());
}

#[tokio::test]
async fn chunk_size_passes_through() {
    let (url, _) = mock_server(vec![]).await;
    assert_eq!(client(&url, 100, 3).chunk_rows(), 100);
}

#[tokio::test]
async fn insert_chunks_rows_and_reports_count() {
    let (url, mock) = mock_server(vec![(200, ""), (200, ""), (200, "")]).await;
    let rows: Vec<serde_json::Value> = (0..25).map(|i| serde_json::json!({ "i": i })).collect();
    let wrote = client(&url, 10, 3)
        .insert_json("events", &rows)
        .await
        .expect("insert");
    assert_eq!(wrote, 25);
    assert_eq!(mock.count(), 3);
    // The wire body carries newline-delimited JSON rows.
    let bodies: Vec<usize> = mock
        .requests
        .lock()
        .expect("mutex")
        .iter()
        .map(|record| record.body.len())
        .collect();
    assert_eq!(bodies.len(), 3);
    assert!(bodies.iter().all(|len| *len > 0));
}

#[tokio::test]
async fn insert_retries_same_query_id() {
    let (url, mock) = mock_server(vec![(500, "x"), (200, "")]).await;
    let rows = vec![serde_json::json!({ "i": 1 })];
    let wrote = client(&url, 10, 3)
        .insert_json("events", &rows)
        .await
        .expect("insert");
    assert_eq!(wrote, 1);
    let requests = mock.requests.lock().expect("mutex");
    assert_eq!(requests.len(), 2);
    let id = |target: &str| {
        target
            .split("query_id=")
            .nth(1)
            .and_then(|rest| rest.split('&').next())
            .unwrap_or("")
            .to_owned()
    };
    // Same chunk retried under one deduplication token.
    assert_eq!(id(&requests[0].target), id(&requests[1].target));
    assert!(!id(&requests[0].target).is_empty());
}

#[tokio::test]
async fn insert_gives_up_after_retries() {
    let (url, mock) = mock_server(vec![(500, "x"), (500, "x")]).await;
    let rows = vec![serde_json::json!({ "i": 1 })];
    assert!(
        client(&url, 10, 2)
            .insert_json("events", &rows)
            .await
            .is_err()
    );
    assert_eq!(mock.count(), 2);
}

#[tokio::test]
async fn executor_writes_rows_and_runs_chunks() {
    use archive_store::ClickHouseExecutor;
    use job_engine::{ScanChunk, VersionExecutor as _};

    let (url, mock) = mock_server(vec![(200, ""), (200, "")]).await;
    let client = client(&url, 10, 3);
    let executor = ClickHouseExecutor::new(
        client,
        "analytics".to_owned(),
        "job_x__v1".to_owned(),
        "SELECT * FROM events WHERE height >= {lo} AND height < {hi}".to_owned(),
    );
    let outcome = executor
        .run_chunk(ScanChunk { lo: 1, hi: 2 })
        .await
        .expect("chunk");
    assert_eq!(outcome.rows_written, 0);
    let wrote = executor
        .write_rows(&[serde_json::json!({ "chain": "t" })])
        .await
        .expect("write");
    assert_eq!(wrote, 1);
    assert_eq!(mock.count(), 2);
}
