use json_patch::{diff, Patch};
use log::{log, Level};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Once;
use tracing::field::{Field, Visit};
use tracing::span::Attributes;
use tracing::{Event, Id, Subscriber};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::{LookupSpan, Scope};
use tracing_subscriber::{layer::Context, Layer};

static INIT: Once = Once::new();

/// Register a [`tracing_subscriber`] to output human readable [`mod@log`] records
///
/// This can be combined with a preferred logging implementation to generate logs, for instance, if
/// using the [env_logger](https://docs.rs/env_logger/latest/env_logger/) crate.
///
/// ```rust,no_run
/// use mahler::worker::init_logging;
///
/// // Convert tracing logs into `log` crate logs
/// init_logging();
///
/// // Configure env_logger to show logs to stdout
/// env_logger::builder().format_target(false).init();
/// env_logger::init();
/// ```
///
/// Will generate an output like the following to stdout
///
/// ```sh
/// [2025-05-22T21:10:11Z INFO ] applying target state
/// [2025-05-22T21:10:11Z INFO ] searching workflow
/// [2025-05-22T21:10:11Z DEBUG] pending changes
/// [2025-05-22T21:10:11Z DEBUG] - {"op":"replace","path":"/a","value":1}
/// [2025-05-22T21:10:11Z DEBUG] - {"op":"replace","path":"/b","value":2}
/// [2025-05-22T21:10:11Z INFO ] searching workflow: success
/// [2025-05-22T21:10:11Z DEBUG] will execute the following tasks:
/// [2025-05-22T21:10:11Z DEBUG] + ~ - a++
/// [2025-05-22T21:10:11Z DEBUG]   ~ - b++
/// [2025-05-22T21:10:11Z DEBUG] - b++
/// [2025-05-22T21:10:11Z INFO ] a++: running ...
/// [2025-05-22T21:10:11Z INFO ] b++: running ...
/// [2025-05-22T21:10:11Z INFO ] a++: success
/// [2025-05-22T21:10:11Z INFO ] b++: success
/// [2025-05-22T21:10:11Z INFO ] b++: running ...
/// [2025-05-22T21:10:11Z INFO ] b++: success
/// [2025-05-22T21:10:11Z INFO ] plan executed successfully
/// [2025-05-22T21:10:11Z INFO ] searching workflow
/// [2025-05-22T21:10:11Z INFO ] nothing else to do: target state reached
/// [2025-05-22T21:10:11Z INFO ] target state applied
/// ```
pub fn init() {
    INIT.call_once(|| tracing_subscriber::registry().with(ToLogLayer).init());
}

#[derive(Default)]
pub struct ToLogLayer;

impl<S> Layer<S> for ToLogLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut map = HashMap::new();
            let mut visitor = FieldMapVisitor::from_map(&mut map);
            attrs.record(&mut visitor);

            // Store the initial fields in span extensions
            span.extensions_mut().insert(map);

            let meta = span.metadata();
            let name = meta.name();
            // Report opening events
            match name {
                "seek_target" => {
                    log!(target: meta.target(), Level::Info, "applying target state");
                }
                "find_workflow" => {
                    log!(target: meta.target(), Level::Info, "searching workflow");
                    if !log::log_enabled!(Level::Debug) {
                        return;
                    }
                    let ext = span.extensions();
                    if let Some(fields) = ext.get::<HashMap<String, String>>() {
                        if let (Some(Ok(ini)), Some(Ok(tgt))) = (
                            fields
                                .get("ini")
                                .map(|s| serde_json::from_str::<Value>(s.as_str())),
                            fields
                                .get("tgt")
                                .map(|s| serde_json::from_str::<Value>(s.as_str())),
                        ) {
                            let Patch(changes) = diff(&ini, &tgt);
                            if !changes.is_empty() {
                                log!(target: meta.target(), Level::Debug, "pending changes");
                                for change in changes {
                                    log!(target: meta.target(), Level::Debug, "- {}", change);
                                }
                            }
                        }
                    }
                }
                "run_task" => {
                    let ext = span.extensions();
                    if let Some(fields) = ext.get::<HashMap<String, String>>() {
                        if let Some(task) = fields.get("task") {
                            log!(target: meta.target(), Level::Info, "{}: running ...", task);
                        }
                    }
                }
                _ => {}
            };
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(&id) {
            let meta = span.metadata();
            let ext = span.extensions();

            let name = meta.name();
            let fields = ext.get::<HashMap<String, String>>();
            match name {
                "seek_target" => {
                    if let Some(map) = fields {
                        if let Some(result) = map.get("return") {
                            match result.as_str() {
                                "success" => {
                                    log!(target: meta.target(), Level::Info, "target state applied")
                                }
                                "interrupted" => {
                                    log!(target: meta.target(), Level::Warn, "target state apply interrupted by user request")
                                }
                                "aborted" => {
                                    if let Some(err) = map.get("error") {
                                        log!(target: meta.target(), Level::Warn, "target state apply interrupted due to error: {err}")
                                    } else {
                                        log!(target: meta.target(), Level::Warn, "target state apply interrupted due to error")
                                    }
                                }
                                _ => {}
                            }
                        } else if let Some(err) = map.get("error") {
                            log!(target: meta.target(), Level::Error, "target state apply failed: {err}");
                        }
                    }
                }
                "find_workflow" => {
                    if let Some(map) = fields {
                        if let Some(result) = map.get("return") {
                            if result.is_empty() {
                                log!(target: meta.target(), Level::Info, "nothing else to do: target state reached");
                            } else {
                                log!(target: meta.target(), Level::Info, "searching workflow: success");
                                if !log::log_enabled!(Level::Debug) {
                                    return;
                                }
                                log!(target: meta.target(), Level::Debug, "will execute the following tasks:");
                                for action in result.split("\n") {
                                    log!(target: meta.target(), Level::Debug, "{}", action);
                                }
                            }
                        }
                        if let Some(error) = map.get("error") {
                            log!(target: meta.target(), Level::Warn, "searching workflow: failed - {}", error)
                        }
                    }
                }
                "run_task" => {
                    if let Some(map) = fields {
                        match (map.get("task"), map.get("error")) {
                            (Some(task), Some(error)) => {
                                log!(target: meta.target(), Level::Warn, "{}: failed - {}", task, error)
                            }
                            (Some(task), None) => {
                                log!(target: meta.target(), Level::Info, "{}: success", task)
                            }
                            _ => {}
                        }
                    }
                }
                "run_workflow" => {
                    if let Some(map) = fields {
                        if map.get("error").is_none() {
                            log!(target: meta.target(), Level::Info, "plan executed successfully")
                        }
                    }
                }
                _ => {}
            };
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx
            .event_scope(event)
            .into_iter()
            .flat_map(Scope::from_root)
            .last()
        {
            let meta = event.metadata();
            let level = map_level(meta.level());
            let mut exts = span.extensions_mut();

            // Get the existing field map (from on_new_span)
            if let Some(fields) = exts.get_mut::<HashMap<String, String>>() {
                let mut visitor = FieldMapVisitor::from_map(fields);
                event.record(&mut visitor);

                if let Some(message) = fields.get("message") {
                    if span.name() == "find_workflow" {
                        log!(target: meta.target(), level, "searching workflow: {} ", message)
                    }

                    if span.name() == "run_task" {
                        return;
                    }

                    // If the current event is below the `run_task` scope, look for the
                    // task id in the run_task scope and if found, convert it to a log
                    if let Some(run_task) = ctx
                        .event_scope(event)
                        .into_iter()
                        .flat_map(Scope::from_root)
                        .find(|s| s.name() == "run_task")
                    {
                        let ext = run_task.extensions();
                        if let Some(task_fields) = ext.get::<HashMap<String, String>>() {
                            if let (Some(id), Some(task)) =
                                (task_fields.get("id"), task_fields.get("task"))
                            {
                                let task_id = format!("{}::{}", meta.target(), span.name());
                                if &task_id == id {
                                    log!(target: meta.target(), level, "{}: {} ", task, message)
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn on_record(&self, id: &Id, values: &tracing::span::Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut exts = span.extensions_mut();

            // Get the existing field map (from on_new_span)
            if let Some(fields) = exts.get_mut::<HashMap<String, String>>() {
                let mut visitor = FieldMapVisitor::from_map(fields);
                values.record(&mut visitor);
            }
        }
    }
}

#[derive(Default)]
pub struct FieldMapVisitor<'a> {
    fields: Option<&'a mut HashMap<String, String>>,
}

impl<'a> FieldMapVisitor<'a> {
    pub fn from_map(map: &'a mut HashMap<String, String>) -> Self {
        Self { fields: Some(map) }
    }
}

impl Visit for FieldMapVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if let Some(ref mut map) = self.fields {
            map.insert(field.name().into(), format!("{:?}", value));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if let Some(ref mut map) = self.fields {
            map.insert(field.name().into(), value.to_string());
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if let Some(ref mut map) = self.fields {
            map.insert(field.name().into(), value.to_string());
        }
    }
}

fn map_level(level: &tracing::Level) -> Level {
    match *level {
        tracing::Level::TRACE => Level::Trace,
        tracing::Level::DEBUG => Level::Debug,
        tracing::Level::INFO => Level::Info,
        tracing::Level::WARN => Level::Warn,
        tracing::Level::ERROR => Level::Error,
    }
}
