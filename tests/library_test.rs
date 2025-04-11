use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;

use gustav::extract::{Args, Target, View};
use gustav::task::prelude::*;
use gustav::worker::{prelude::*, Status as WorkerStatus, Worker};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Counters(HashMap<String, i32>);

fn init() {
    #[cfg(feature = "logging")]
    gustav::worker::init_logging();
    env_logger::builder()
        .format_target(false)
        .try_init()
        .unwrap_or(());
}

fn plus_one(mut counter: View<i32>, Target(tgt): Target<i32>) -> IO<i32> {
    if *counter < tgt {
        // Modify the counter if we are below target
        *counter += 1;
    }

    // Return the updated counter. The I/O part of the
    // effect will only be called if the job is chosen
    // in the workflow which will only happens if there are
    // changes
    with_io(counter, |counter| async {
        sleep(Duration::from_millis(10)).await;
        Ok(counter)
    })
}

#[tokio::test]
// this is just a placeholder test to check that library modules
// are accessible
async fn test_worker() {
    init();
    let worker = Worker::new()
        .job((
            "/{counter}",
            update(plus_one)
                .with_description(|Args(counter): Args<String>| format!("{counter} + 1")),
        ))
        .initial_state(Counters(HashMap::from([
            ("a".to_string(), 0),
            ("b".to_string(), 0),
        ])))
        .seek_target(Counters(HashMap::from([
            ("a".to_string(), 2),
            ("b".to_string(), 0),
        ])))
        .unwrap();

    let worker = worker.wait(None).await.unwrap_idle();
    let state = worker.state().unwrap();
    assert_eq!(worker.status(), &WorkerStatus::Success);
    assert_eq!(
        state,
        Counters(HashMap::from([("a".to_string(), 2), ("b".to_string(), 0),]))
    );
}
