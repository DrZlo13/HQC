//! A scriptable adapter for tests: records every call and lets the test emit events,
//! accept inputs and end turns by hand. It never spawns anything.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hqc_proto::event::{Author, ClarifyAnswer, Event, EventKind};
use hqc_proto::frame::{Runtime, SessionInput};
use uuid::Uuid;

use super::{Adapter, AdapterEvent, AdapterFactory, EventSink};
use crate::config::StartSpec;
use crate::inbox::InboxFile;

#[derive(Debug, Clone, PartialEq)]
pub enum FakeCall {
    Start(Uuid),
    Resume(Uuid),
    Input {
        session_id: Uuid,
        event_id: Uuid,
    },
    Stop(Uuid),
    Kill(Uuid),
    Approval {
        request_id: String,
        option_id: String,
        message: Option<String>,
    },
    Clarify {
        request_id: String,
        answers: usize,
    },
}

#[derive(Default)]
struct State {
    calls: Vec<FakeCall>,
    created: usize,
    fail_start: bool,
    sinks: HashMap<Uuid, EventSink>,
    inputs: Vec<(SessionInput, Vec<InboxFile>)>,
    /// What [`Adapter::pid`] reports; the test process itself unless a test set another.
    pid: Option<Option<u32>>,
}

#[derive(Clone, Default)]
pub struct FakeAdapters {
    state: Arc<Mutex<State>>,
}

impl FakeAdapters {
    pub fn new() -> Self {
        Self::default()
    }

    /// The process the agents of this factory report as theirs ([`Adapter::pid`]); by
    /// default the test process, from which the MCP servers a test starts descend.
    pub fn set_pid(&self, pid: Option<u32>) {
        self.lock().pid = Some(pid);
    }

    /// Make every following `start`/`resume` fail.
    pub fn set_fail_start(&self, fail: bool) {
        self.lock().fail_start = fail;
    }

    pub fn calls(&self) -> Vec<FakeCall> {
        self.lock().calls.clone()
    }

    /// Every input as the adapter got it (text with the attachment block) and its files.
    pub fn inputs(&self) -> Vec<(SessionInput, Vec<InboxFile>)> {
        self.lock().inputs.clone()
    }

    /// Number of adapters created, i.e. sessions the supervisor tried to run.
    pub fn created(&self) -> usize {
        self.lock().created
    }

    /// Waits until `pred(calls)` holds; panics after 5 s.
    pub async fn wait_for(&self, what: &str, pred: impl Fn(&[FakeCall]) -> bool) -> Vec<FakeCall> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let calls = self.calls();
            if pred(&calls) {
                return calls;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("timed out waiting for {what}; calls: {calls:?}");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Emits `ev` as if the runtime of `session_id` produced it.
    pub async fn emit(&self, session_id: Uuid, ev: AdapterEvent) {
        let sink = self
            .lock()
            .sinks
            .get(&session_id)
            .cloned()
            .expect("no adapter for this session");
        sink.send(ev).await.expect("session task is gone");
    }

    pub async fn accept_input(&self, session_id: Uuid, event_id: Uuid) {
        self.emit(session_id, AdapterEvent::InputAccepted { event_id })
            .await;
    }

    pub async fn end_turn(&self, session_id: Uuid) {
        self.emit(session_id, AdapterEvent::TurnEnded).await;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// An `agent.message` event of `session_id`, for tests.
pub fn agent_message(session_id: Uuid, text: &str) -> Event {
    agent_event(
        session_id,
        EventKind::AgentMessage {
            text: text.into(),
            r#final: false,
            in_reply_to: None,
        },
    )
}

pub fn agent_event(session_id: Uuid, kind: EventKind) -> Event {
    super::new_event(session_id, Author::Agent { id: session_id }, kind)
}

impl AdapterFactory for FakeAdapters {
    fn supports(&self, runtime: Runtime) -> bool {
        runtime == Runtime::ClaudeCode
    }

    fn create(&self, spec: &StartSpec, events: EventSink) -> Box<dyn Adapter> {
        let mut st = self.lock();
        st.created += 1;
        st.sinks.insert(spec.session_id, events);
        Box::new(FakeAdapter {
            session_id: spec.session_id,
            shared: self.clone(),
        })
    }
}

struct FakeAdapter {
    session_id: Uuid,
    shared: FakeAdapters,
}

impl FakeAdapter {
    fn record(&self, c: FakeCall) {
        self.shared.lock().calls.push(c);
    }

    fn spawn(&self, c: FakeCall) -> anyhow::Result<()> {
        let fail = self.shared.lock().fail_start;
        self.record(c);
        if fail {
            anyhow::bail!("fake: runtime binary not found");
        }
        Ok(())
    }
}

#[async_trait]
impl Adapter for FakeAdapter {
    async fn start(&mut self, _: &StartSpec) -> anyhow::Result<()> {
        self.spawn(FakeCall::Start(self.session_id))
    }

    async fn resume(&mut self, _: &StartSpec) -> anyhow::Result<()> {
        self.spawn(FakeCall::Resume(self.session_id))
    }

    async fn input(&mut self, input: &SessionInput) -> anyhow::Result<()> {
        self.input_with(input, &[]).await
    }

    async fn input_with(
        &mut self,
        input: &SessionInput,
        files: &[InboxFile],
    ) -> anyhow::Result<()> {
        self.shared
            .lock()
            .inputs
            .push((input.clone(), files.to_vec()));
        self.record(FakeCall::Input {
            session_id: self.session_id,
            event_id: input.event_id,
        });
        Ok(())
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        self.record(FakeCall::Stop(self.session_id));
        Ok(())
    }

    async fn kill(&mut self) -> anyhow::Result<()> {
        self.record(FakeCall::Kill(self.session_id));
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        self.shared.lock().pid.unwrap_or(Some(std::process::id()))
    }

    async fn answer_approval(
        &mut self,
        request_id: &str,
        option_id: &str,
        message: Option<&str>,
    ) -> anyhow::Result<()> {
        self.record(FakeCall::Approval {
            request_id: request_id.into(),
            option_id: option_id.into(),
            message: message.map(Into::into),
        });
        Ok(())
    }

    async fn answer_clarify(
        &mut self,
        request_id: &str,
        answers: &BTreeMap<String, ClarifyAnswer>,
    ) -> anyhow::Result<()> {
        self.record(FakeCall::Clarify {
            request_id: request_id.into(),
            answers: answers.len(),
        });
        Ok(())
    }
}
