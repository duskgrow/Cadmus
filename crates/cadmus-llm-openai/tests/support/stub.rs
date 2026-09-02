//! A hand-rolled HTTP/1.1 stub replaying queued responses to the genai
//! client. Stays on tokio only — no extra test dependencies. One queued reply
//! per request; requests are recorded for request-side dialect assertions.
//!
//! Shared across integration-test targets, each of which compiles this module
//! independently and uses a different subset — hence the dead-code allowance.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// One queued HTTP reply.
pub struct StubReply {
    status: u16,
    content_type: &'static str,
    body: String,
}

impl StubReply {
    /// A `200 OK` SSE stream body.
    pub fn sse(body: String) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            body,
        }
    }

    /// An error reply with a JSON body.
    pub fn json(status: u16, body: String) -> Self {
        Self {
            status,
            content_type: "application/json",
            body,
        }
    }
}

/// A request the stub served, for request-side assertions.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub body: String,
}

/// The stub server. Replies are consumed in FIFO order; an unscripted request
/// gets a loud `500 stub exhausted`. Dropping aborts the accept loop.
pub struct SseStub {
    endpoint: String,
    replies: Arc<Mutex<VecDeque<StubReply>>>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    server: JoinHandle<()>,
}

impl SseStub {
    /// Binds on an OS-assigned loopback port and starts accepting. Must be
    /// called from within a tokio runtime (the contract suite's factory runs
    /// inside one); synchronous so factories stay plain closures.
    pub fn start() -> Self {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
        std_listener.set_nonblocking(true).expect("nonblocking");
        let listener = TcpListener::from_std(std_listener).expect("tokio listener");
        let addr = listener.local_addr().expect("stub local addr");
        let replies = Arc::new(Mutex::new(VecDeque::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server = {
            let replies = Arc::clone(&replies);
            let requests = Arc::clone(&requests);
            tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        break;
                    };
                    let replies = Arc::clone(&replies);
                    let requests = Arc::clone(&requests);
                    tokio::spawn(async move {
                        let request = read_request(&mut socket).await;
                        requests.lock().expect("requests").push(request);
                        let reply =
                            replies
                                .lock()
                                .expect("replies")
                                .pop_front()
                                .unwrap_or_else(|| StubReply {
                                    status: 500,
                                    content_type: "text/plain",
                                    body: "stub exhausted: no scripted reply".into(),
                                });
                        write_reply(&mut socket, &reply).await;
                    });
                }
            })
        };
        Self {
            endpoint: format!("http://{addr}"),
            replies,
            requests,
            server,
        }
    }

    /// The base URL to point a dialect's endpoint at.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Queues the reply for the next request.
    pub fn push(&self, reply: StubReply) {
        self.replies.lock().expect("replies").push_back(reply);
    }

    /// All requests served so far, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("requests").clone()
    }
}

impl Drop for SseStub {
    fn drop(&mut self) {
        self.server.abort();
    }
}

const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

/// Reads one request: head up to `\r\n\r\n`, then `content-length` body
/// bytes. Test requests are small JSON; anything else fails loudly.
async fn read_request(socket: &mut TcpStream) -> RecordedRequest {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8_192];
    let head_end = loop {
        if let Some(pos) = find(&buffer, b"\r\n\r\n") {
            break pos + 4;
        }
        let read = socket.read(&mut chunk).await.expect("read request head");
        assert!(read > 0, "connection closed before the request head");
        buffer.extend_from_slice(&chunk[..read]);
        assert!(buffer.len() <= MAX_REQUEST_BYTES, "request head too large");
    };

    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut request_line = head
        .lines()
        .next()
        .expect("request line")
        .split_whitespace();
    let method = request_line.next().unwrap_or("?").to_string();
    let path = request_line.next().unwrap_or("?").to_string();
    let content_length = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);

    let mut body = buffer[head_end..].to_vec();
    while body.len() < content_length {
        let read = socket.read(&mut chunk).await.expect("read request body");
        assert!(read > 0, "connection closed mid-body");
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);

    RecordedRequest {
        method,
        path,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

async fn write_reply(socket: &mut TcpStream, reply: &StubReply) {
    let reason = match reply.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let response = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        reply.status,
        reason,
        reply.content_type,
        reply.body.len()
    );
    socket
        .write_all(response.as_bytes())
        .await
        .expect("write reply head");
    socket
        .write_all(reply.body.as_bytes())
        .await
        .expect("write reply body");
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
