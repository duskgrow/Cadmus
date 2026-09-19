//! Real serial file tools through the same protocol as the TUI, with no provider IO.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cadmus_contract::{
    Approval, ChatRequest, Command, EventKind, FinishReason, LiveItem, LiveKind, LiveSink,
    StreamChunk,
};
use cadmus_core::testing::{test_context, test_telemetry};
use cadmus_core::{AgentLoop, ClientProtocol, ReplayProvider, replay_trace};
use cadmus_transport::{Broadcaster, command_channel};
use serde_json::json;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("cadmus-approval-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("scratch");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Observed {
    broadcaster: Arc<Broadcaster>,
    tx: tokio::sync::mpsc::UnboundedSender<LiveItem>,
}

impl LiveSink for Observed {
    fn publish(&self, item: &LiveItem) {
        self.broadcaster.publish(item);
        let _ = self.tx.send(item.clone());
    }
}

fn scripted_provider() -> ReplayProvider {
    let mut script = Vec::new();
    for (index, name, args) in [
        (
            0,
            "write_file",
            json!({"path": "denied.txt", "content": "must not exist"}),
        ),
        (
            1,
            "edit_file",
            json!({"path": "edited.txt", "edits": [
                {"old_string": "before", "new_string": "after"}
            ]}),
        ),
    ] {
        script.extend([
            StreamChunk::ToolCallStart {
                index,
                id: format!("call-{index}"),
                name: name.into(),
            },
            StreamChunk::ToolArgsDelta {
                index,
                fragment: args.to_string(),
            },
            StreamChunk::ToolCallEnd { index },
        ]);
    }
    script.push(StreamChunk::Done {
        finish: FinishReason::ToolCalls,
    });
    ReplayProvider::new([
        ReplayProvider::script(script),
        ReplayProvider::script(vec![
            StreamChunk::TextDelta("done".into()),
            StreamChunk::Done {
                finish: FinishReason::Stop,
            },
        ]),
    ])
}

#[tokio::test]
async fn a_real_edit_executes_before_an_earlier_write_is_decided() {
    let scratch = Scratch::new();
    fs::write(scratch.0.join("edited.txt"), "before").expect("seed file");
    let broadcaster = Arc::new(Broadcaster::new());
    let (tx, mut live) = tokio::sync::mpsc::unbounded_channel();
    let (sender, commands) = command_channel();
    let (telemetry, log) = test_telemetry("tr-real-approval");
    let agent = AgentLoop::new(
        Arc::new(scripted_provider()),
        cadmus::coding_tools(scratch.0.clone(), Vec::new()),
        test_context(),
        ClientProtocol {
            live: Arc::new(Observed {
                broadcaster: broadcaster.clone(),
                tx,
            }),
            commands: Arc::new(commands),
        },
        3,
        telemetry,
    );
    let request = ChatRequest::user_text("edit the files", 1024);
    let client = async {
        let request_id = loop {
            if let LiveKind::ApprovalRequested {
                request_id, calls, ..
            } = live.recv().await.expect("request").kind
            {
                assert_eq!(calls.len(), 2);
                break request_id;
            }
        };
        sender
            .send(Command::ResolveApprovalCall {
                command_id: "approve-second".into(),
                request_id: request_id.clone(),
                call_index: 1,
                decision: Approval::Approved,
            })
            .expect("approve edit");
        let completion = loop {
            if let LiveKind::ToolCompleted { completion } =
                live.recv().await.expect("completion").kind
            {
                break completion;
            }
        };
        assert_eq!(completion.message_call_index, 1);
        assert_eq!(completion.name, "edit_file");
        assert!(completion.error.is_none());
        assert_eq!(
            fs::read_to_string(scratch.0.join("edited.txt")).unwrap(),
            "after"
        );
        assert!(!scratch.0.join("denied.txt").exists());
        let sync = broadcaster.attach().sync;
        assert_eq!(
            sync.in_flight.pending_approvals[0].decisions,
            vec![None, Some(Approval::Approved)]
        );
        assert_eq!(sync.in_flight.completed_tools, vec![completion]);
        assert!(
            log.events()
                .iter()
                .all(|event| !matches!(event.kind, EventKind::ToolResult { .. }))
        );
        sender
            .send(Command::ResolveApprovalCall {
                command_id: "deny-first".into(),
                request_id,
                call_index: 0,
                decision: Approval::Rejected { comment: None },
            })
            .expect("reject write");
    };
    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(agent.run(&request), client)
    })
    .await
    .expect("approval rendezvous must not hang");
    let outcome = outcome.expect("run");
    assert!(!scratch.0.join("denied.txt").exists());
    assert_eq!(
        fs::read_to_string(scratch.0.join("edited.txt")).unwrap(),
        "after"
    );
    assert_eq!(outcome.messages[2].tool_call_id.as_deref(), Some("call-0"));
    assert!(outcome.messages[2].is_error);
    assert_eq!(outcome.messages[3].tool_call_id.as_deref(), Some("call-1"));
    assert!(!outcome.messages[3].is_error);
    assert_eq!(replay_trace(&log.events()).messages, outcome.messages);
    let sync = broadcaster.attach().sync;
    assert!(sync.in_flight.pending_approvals.is_empty());
    assert!(sync.in_flight.completed_tools.is_empty());
    assert_eq!(
        sync.settled_approvals[0].decisions,
        vec![Approval::Rejected { comment: None }, Approval::Approved]
    );
}
