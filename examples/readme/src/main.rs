use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;

use mahler::extract::{Args, Target, View};
use mahler::task::prelude::*;
use mahler::worker::{init_logging, Worker};

// The state model needs to be Serializable and Deserializable
// since the library uses JSON internally to access parts
// of the state
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Counters(HashMap<String, i32>);

// `plus_one` defines a job that updates a counter if it is below some target.
// The job makes use of two extractors:
// - `View`, that provides a mutable view into the system state. By modifying the view,
// the job task can affect the global state
// - `Target`, providing a read only view to the target being seeked by the planner
fn plus_one(mut counter: View<i32>, Target(tgt): Target<i32>) -> Update<i32> {
    if *counter < tgt {
        // Modify the counter value if we are below the target
        *counter += 1;
    }

    // The task is called at planning and at runtime, the `with_io` function
    // allows us to define what is returned by the function at each context.
    // The first argument of the function is what the planner receives,
    // the right side of the call is what will be executed at runtime if the
    // task is selected.
    with_io(counter, |counter| async {
        // The async call can be used to actually make changes to the underlying system.
        // It could be writing the counter to a database or a file. In this
        // case we just add some timer
        sleep(Duration::from_millis(10)).await;
        Ok(counter)
    })
}

// `plus_two` is a compound job. Compound job do not modify the state directly
// but return combination of sub-tasks that are applicable to a certain target
fn plus_two(counter: View<i32>, Target(tgt): Target<i32>) -> Vec<Task> {
    // If the difference between the current state and target is >1
    if tgt - *counter > 1 {
        // Then return a sequence of two tasks with the same target
        return vec![plus_one.with_target(tgt), plus_one.with_target(tgt)];
    }

    // Otherwise do nothing
    vec![]
}

#[tokio::main]
async fn main() -> Result<()> {
    // Convert tracing logs into `log` crate logs
    init_logging();

    // Show logs on stdout
    env_logger::builder().format_target(false).init();

    let worker = Worker::new()
        // The jobs are applicable to `UPDATE` operations
        // on individual counters
        .job(
            "/{counter}",
            update(plus_one)
                // we can add a description to the job for the logs
                .with_description(|Args(counter): Args<String>| format!("{counter}++")),
        )
        .job("/{counter}", update(plus_two))
        // We initialize the worker with two counters
        // `a` and `b` with value 0
        .initial_state(Counters(HashMap::from([
            ("a".to_string(), 0),
            ("b".to_string(), 0),
        ])))
        .with_context(|| "failed to serialize initial state")?;

    // Tell the worker to find a plan from the initial state (a:0, b:0)
    // to the target state (a:1, b:2) and execute it
    let worker = worker
        .seek_target(Counters(HashMap::from([
            ("a".to_string(), 1),
            ("b".to_string(), 2),
        ])))
        .await
        .with_context(|| "failed to reach target state")?;

    // Get the internal state from the Worker. The worker
    // is idle but the state may not be static so we need
    // to use an await to get the current state.
    let state = worker
        .state()
        .await
        .with_context(|| "failed to deserialize state")?;

    assert_eq!(
        state,
        Counters(HashMap::from([("a".to_string(), 1), ("b".to_string(), 2),]))
    );

    println!("The system state is now {:?}", state);
    Ok(())
}
