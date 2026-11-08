//! SPEC §5.4 `worker.limits`, §6 `GET /api/workers`: the usage limits a worker reports are
//! shown with it until it reports others.

mod common;

use common::*;
use serde_json::{json, Value};

const WS_PATH: &str = "/Users/home-mac/work/hqc";

async fn limits(hub: &TestHub) -> Value {
    let (s, v) = hub.get("/api/workers").await;
    assert_eq!(s, 200, "{v}");
    v["workers"][0]["limits"].clone()
}

#[tokio::test]
async fn a_workers_limits_are_listed_with_it() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    assert_eq!(limits(&hub).await, Value::Null);

    for five in [0.39, 0.41] {
        w.send(
            "worker.limits",
            json!({"five_hour": {"utilization": five, "resets_at": 1794150000},
                   "seven_day": {"utilization": 0.23, "resets_at": 1794283200}}),
        )
        .await;
        w.expect("ack").await;
    }
    let l = limits(&hub).await;
    assert_eq!(l["five_hour"], json!({"utilization": 0.41, "resets_at": 1794150000}));
    assert_eq!(l["seven_day"]["utilization"], 0.23);
    assert!(l["reported_at"].is_string(), "{l}");
    hub.finish().await;
}
