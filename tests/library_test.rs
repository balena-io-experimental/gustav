use gustav::extract::{Target, Update};
use gustav::system::System;
use gustav::task::*;
use json_patch::Patch;
use serde_json::{from_value, json};

fn my_task_effect(mut counter: Update<i32>, tgt: Target<i32>) -> Effect<Update<i32>> {
    if *counter < *tgt {
        *counter += 1;
    }

    // This is not necessary, just returning the view
    // works
    Effect::of(counter)
}

// This test replicates a test inside the task module, it is here just as a
// placeholder to ensure the library is exporting everything needed to use the API
#[test]
fn it_allows_to_dry_run_tasks() {
    let system = System::from(0);
    let job = my_task_effect.into_job();
    let action = job.into_task(Context::new().target(1));

    // Get the list of changes that the action performs
    let changes = action.dry_run(&system).unwrap();
    assert_eq!(
        changes,
        from_value::<Patch>(json!([
          { "op": "replace", "path": "", "value": 1 },
        ]))
        .unwrap()
    );
}
