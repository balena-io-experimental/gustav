# gustav

An automated job orchestration library to build and execute dynamic workflows.

Gustav uses [automated planning](https://en.wikipedia.org/wiki/Automated_planning_and_scheduling) (heavily based on [HTNs](https://en.wikipedia.org/wiki/Hierarchical_task_network)) to compose user defined jobs into a task workflow (represented as a [DAG](https://en.wikipedia.org/wiki/Directed_acyclic_graph)) to achieve a desired target system state. If failures happen during execution it can adapt the workflow to perform corrective actions or automatically retry certain operations. It is meant for more than job orchestration, but as a library to build autonomous system agents in Rust.

NOTE: this project is still in very early stage, but its design is guided by [Mahler](https://github.com/balena-io-modules/mahler), it's NodeJS counterpart, and will share many of the same design concepts, with an API more suited for Rust that is heavily inspired by [Axum](https://github.com/tokio-rs/axum).

## Why build this?

In the tech industry we are increasingly reliant on automation for deployment and configuration of systems. As infrastructures grows, so does the need for intelligence in these automated workflows to reduce downtime and the need of human intervention. In the context of the Internet of Things, the scale (thousands of devices) means systems need to be able operate with little to no human intervention: self-heal, self-configure and self-protect (see [autonomic computing](https://en.wikipedia.org/wiki/Autonomic_computing)). When human intervention is needed, fixes should propagate to the fleet so devices can self-correct the problem next time it happens.

Static workflow definition tools do not scale well for this purpose as the diversity of conditions makes modelling too complex making the workflow harder and harder to maintain and test. We believe automated planning is a tool best suited for this problem: define a job and constraints for executing it and a planner decides if the job is applicable to a desired system state.

While the ideas behind planning systems go back to the 1970s, they have seen very little usage outside of academia. One possible reason for this is that planning systems usually require some domain specific languages, or some variant of Lisp to program them, making them less appealing in the mainstream software industry, that generally has favored imperative programming languages. We expect that reducing this barrier of entry may make the use of automated planning a viable option for writing automated workflows.

## Features

- Simple API. Just write your jobs as Rust functions and link them to a worker. The planner will determine the best way to compose them into a workflow to achieve a specific system state. Jobs can be referenced by compound jobs to guide the planner towards a desired behavior.
- Observable runtime. The runtime state of the agent can be monitored at all times with different levels of detail. Human readable metadata for indidividual tasks is generated from the function names and execution context.
- Parallel execution of tasks. The planner automatically detects when operations can be performed in parallel and creates branches in the plan to tell the agent to run concurrent operations.
- Easy to debug. Agent observable state and known goals allow easy replicability when issues occur. The planning decision tree and resulting plans can be diagrammed to visually inspect where planning is failing.

# Core concepts

- **Job** a repeatable operation defined as a function that operates on the system state and may or may not perform IO operations. Jobs are generic, and depend on a context to be selected by the planner. Jobs can be _atomic_, meaning they define a concrete action (e.g. download an artifact), or _compound_, meaning they require to execute several tasks to be achieved.
- **Task** is a concrete operation to be used for planning or as part of a workflow. When a Job is given a context, it becomes a task and can be used for planning.
- **Effect** is the outcome on the system that will happen after executing a task. It is used by the planner to safely find paths between the current state and the target state.
- **Context** is metadata about the execution environment where the Task is being executed, the current state of the system, the target, the result from a previous run, etc., can be used in the Job definition to tell the planner whether or not to chose it for the workflow.
- **Workflow** encodes a plan of what actions need to be executed by the worker in order to reach a certain targets. Workflows are represented as Directed Acyclic Graphs (DAG).
- **Target** is a desired system state, e.g. "temperature of the room should be 25 degrees celsius".
- **Worker** is an autonomous agent that can perform changes on a system using the knowledge encoded as jobs and a given target. A worker will keep trying to reach the target until the target changes or some other exit conditions are met.
- **Sensor** is an observer of the system state, the worker can subscribe to one or more sensors in order to keep its local view of the state up-to-date and trigger re-planning if necessary.

## Basic Usage

Let's create a system controller for a simple counting system. Let's define a Job that operates on i32

```rust
use gustav::task::*;
use gustav::extract::{Target, Update};

/// Plus one is a job that updates a counter if it is below some target
fn plus_one(mut counter: Update<i32>, tgt: Target<i32>) -> Update<i32> {
    if *counter < *tgt {
        // Modify the counter value if we are below the target
        *counter += 1;
    }

    // We need to return the modified value so the planner
    // can identify the changes to the state. If there are no
    // changes made, then the job will not be used
    counter
}
```

The job above updates the counter if it is below the target, otherwise it returs the same value that it currently has. When planning, a task that perform no changes on the system state is not selected, which allows us to control when jobs are applicable. To use the task, we need to create a worker.

```rust
#[tokio::main]
async fn main() {
    // build our agent using the plus one job
    let agent = Worker::new()
        // The plus_one job is applicable to an UPDATE operation
        // for global state
        .job("/", update(plus_one))
        // The initial state of the worker
        .with_state(0)


    // Tell the agent to find a plan from the initial state (0)
    // to the target state (3) and execute it
    agent.seek(3);

    // Wait for the agent to return a result
    let res = agent.wait(0).await;


    if let Some(state) = res {
      println!("The system state is now {}", state);
    }
}
```

When receiving a call to `seek`, the worker looks for a plan to get the system to a state equal to 3 target and then executes it.
In this case it will identify that 3 sequential calls to `plus_one` are necessary to reach the target.

## Performing IO

The code above is a bit too simple, however. It only operates on the Worker in-memory state but does not perform any IO. In order to interact with the actual system state, we need to use an `Effect`.

```rust
use tokio::fs;
use tokio::io::Error;

/// Plus one is a job that updates a counter if it is below some target
fn plus_one(mut counter: Update<i32>, tgt: Target<i32>) -> Effect<Update<i32>, Error>  {
    if *counter < *tgt {
        // Modify the counter value if we are below the target
        *counter += 1;
    }

    // An Effect type allows us to isolate IO from the outcome
    // that the job will have on the system. It allows the planner
    // to test tasks safely
    Effect.of(counter)
          // This will not be executed at planning
          .with_io(|counter| async move {
              fs::write("/etc/counter", &counter.to_le_bytes()).await?;
              Ok(counter)
          })
}
```

As in the previous example, the job above will update the counter if it is below the target, however when executed as part of a workflow, it will update the `/etc/counter` file on disk with the updated counter value. This encapsulation of effects allows the job to be executed safely during planning to determine if it is applicable to reaching the desired state.

> [!NOTE]  
> There is nothing that prevents a Job function from performing IO outside of an Effect, e.g. by using synchronous calls, however, doing so will most likely cause unexpected effects on the underlying system as the function may be called multiple times as part of the planning process.

## Interacting with complex state

When interacting with a system, we will usually want to keep track of multiple pieces of state. Let's say in our previous example, that we want to keep track of multiple counters. We need first to model our system, we'll use a `struct` for modelling the system and a `HashMap` to keep track of the state of each counter.

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// The state model
#[derive(Serialize, Deserialize, Debug, Clone)]
struct State {
    counters: HashMap<String, i32>,
}
```

Note that when defining the state model we need to make it a [Serialize](https://serde.rs/impl-serialize.html) and [Deserialize](https://serde.rs/impl-deserialize.html), this is a requirement of the library for convenience. We'll see later that we can reference parts of the state via [JSON pointers](https://www.rfc-editor.org/rfc/rfc6901) (e.g. `/counters`). In order to do that, the library serializes the state into a [serde_json::Value](https://docs.rs/serde_json/latest/serde_json/value/enum.Value.html) and operates internally using this tree-like structure, converting to the appropriate type when calling each job.

Now that we have the model, we can define the `plus_one` job again.

```rust
use tokio::fs;
use tokio::io::Error;

/// We need to declare both the parent and the child type inside the extractors
fn plus_one(mut counter: Update<State, i32>, tgt: Target<State, i32>, Path(name): Path<String>) -> Effect<Update<i32>, Error>  {
    if *counter < *tgt {
        // Modify the counter value if we are below the target
        *counter += 1;
    }

    Effect.of(counter)
          .with_io(|counter| async move {
              // Use the counter name to write to the right file
              let path = format!("/etc/counters/{}", name);
              fs::write(path.as_ref(), &counter.to_le_bytes()).await?;
              Ok(counter)
          })
}
```

And finally create the worker

```rust
#[tokio::main]
async fn main() {
    // build our agent using the plus one job
    let agent = Worker::new()
        // The plus_one job is applicable to an UPDATE operation
        // on any given counter
        .job("/counters/{name}", update(plus_one))
        // Initialize two counters "a" and "b" to 0
        .with_state(State {counters: HashMap::from([("a".to_string(), 0), ("b".to_string(), 0)])})


    // Tell the agent to find a plan from the initial state
    // to the target state and execute it
    agent.seek(State {counters: HashMap::from([("a".to_string(), 3), ("b".to_string(), 2)])});

    // Wait for the agent to return a result
    let res = agent.wait(0).await;


    if let Some(state) = res {
      println!("The system state is now {}", state);
    }
}
```

On the above example, the job definition is practically identical to the one in the previous example. The only differences are on the types passed to the extractor arguments, which now need to include the system model, and the `Path` type, which may not always be needed. Using JSON pointer notation, we can assign the job to be relevant to some part of the state, and even re-use if we wanted to.

## Compound Jobs

As programmers, we want to be able to build code by composing simpler behaviors into more complex ones. We might want to guide the planner towards a specific solution, using the primitives we already have. For instance, let's say we want to help the planner get to a solution faster as adding tasks one by one takes too much time. We want to define a `plus_two` task, that increases the counter by 2. We could create another primitive task to update the counter by two, but as programmers, we would like to reuse the code we have already defined. We can do that using methods.

```rust
fn plus_two(counter: Update<State, i32>, tgt: Target<State, i32>, Path(name): Path<String>) -> Vec<Task<i32>> {
    if *tgt - *counter < 2 {
        // Returning an empty result tells the planner
        // the task is not applicable to reach the target
        return vec![];
    }

    // A compound job returns a list of tasks that need to be executed
    // to achieve a certain goal
    vec![
        plus_one.into_task(Context::new().target(*tgt).arg("name", &name)),
        plus_one.into_task(Context::new().target(*tgt).arg("name", &name)),
    ]
}

#[tokio::main]
async fn main() {
    // build our agent using the plus one job
    let agent = Worker::new()
        .job("/counters/{name}", update(plus_one))
        .job("/counters/{name}", update(plus_two))
        // Initialize two counters "a" and "b" to 0
        .with_state(State {counters: HashMap::from([("a".to_string(), 0), ("b".to_string(), 0)])})

    // Seek some state, etc
}
```

A compound job cannot directly modify the state, but it returns a sequence of tasks that are applicable to achieve a goal. Note that job functions implement the `into_task` method, allowing to create a task instance by passing a `Context`. This also happens as part of the planning process but in this case it allows us to reuse to have jobs that call to other jobs.

> [!NOTE] Jobs can also call themselves recursively, and we implement protections against infinite recursion that could lead to the worker stuck planning forever.

Compound jobs are useful for tweaking the plans under certain conditions. They also help reduce the search space. When looking for a plan, the planner will try compound jobs first, and only when these fail fail, proceed to look for atomic ones. During planning, compound jobs are expanded recursively into its component actions, so they won't appear on the final workflow.

## Parallelism

WIP: compound jobs will be tried in parallel first and revert to sequential execution if conflicts are found. You can force sequential execution by returning a `Sequence` value from a compound job

## Operations

### Initializing data (create)

### Deleting data (delete)

### Wildcard jobs

## Testing and diagramming generated workflows

WIP: Workflows are deterministic, meaning that given an initial state and a target state, the generated workflow should always be the same. Gustav provides tools for validating the expected worflow given a job configuration.

## Sensors

## Agent Observability
