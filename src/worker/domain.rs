use matchit::Router;
use std::collections::{BTreeSet, HashMap};

use super::intent::{Intent, Operation};
use crate::path::PathArgs;

#[derive(Debug, PartialEq)]
pub(crate) enum DomainSearchError {
    JobNotFound,
    MissingArgs(Vec<String>),
}

#[derive(Default, Debug)]
pub struct Domain {
    // The router stores a list of intents matching a route
    router: Router<BTreeSet<Intent>>,
    // The index stores the reverse relation of job id to a route
    index: HashMap<Box<str>, String>,
}

impl Domain {
    pub fn new() -> Self {
        Self {
            router: Router::new(),
            index: HashMap::new(),
        }
    }

    /// Add a job to a domain
    ///
    /// # Panics
    ///
    /// This function will panic if the route is not a valid path
    /// or if a job is assigned to multiple routes
    pub fn job(self, route: &'static str, intent: Intent) -> Self {
        // TODO: it would be great to figure out a way to validate
        // that the pointer is valid for the parent state at compile time
        let Self {
            mut router,
            mut index,
        } = self;

        let job_id = String::from(intent.job.id());
        let operation = intent.operation.clone();

        // Remove the route from the router if it exists or create
        // a new set if it doesn't
        let mut queue = router.remove(route).unwrap_or_default();

        // Do not allow the same job to be assigned to
        // multiple operations. This could cause problems at
        // runtime
        if queue.iter().any(|i| i.job.id() == job_id) {
            panic!(
                "cannot assign job '{}' to operation '{:?}', a previous assignment exists",
                job_id, operation
            )
        }

        // Insert the route to the queue
        let updated = queue.insert(intent);

        // (re)insert the queue to the router, we should not have
        // conflicts here
        router.insert(route, queue).expect("route should be valid");

        // Only allow one assignment of a job to a route
        if updated {
            if let Some(oldroute) =
                index.insert(job_id.clone().into_boxed_str(), String::from(route))
            {
                panic!(
                    "cannot assign job '{}' to route '{}', a previous assignment exists to '{}'",
                    job_id, route, oldroute
                )
            }
        }

        Self { router, index }
    }

    // This allows to find the path that a task relates to from the
    // job it belongs to and the arguments given by the user as part
    // of the context.
    pub(crate) fn get_path(
        &self,
        job_id: &str,
        args: &PathArgs,
    ) -> Result<String, DomainSearchError> {
        if let Some(route) = self.index.get(job_id) {
            let mut route = route.clone();
            let mut replacements = Vec::new();

            // Step 1: Replace `{param}` and `{*param}` placeholders
            for (k, v) in args.iter() {
                let param = format!("{{{}}}", k);
                let wildcard_param = format!("{{*{}}}", k);
                let escaped_param = format!("{{{{{}}}}}", k);

                // Collect replacements
                replacements.push((param, v.clone()));
                replacements.push((wildcard_param, v.clone()));

                // Temporarily replace escaped parameters with a placeholder
                let placeholder = format!("__ESCAPED_{}__", k);
                route = route.replace(&escaped_param, &placeholder);
            }

            // Apply `{param}` and `{*param}` replacements
            for (param, value) in replacements {
                route = route.replace(&param, &value);
            }

            // Convert `{{param}}` → `{param}`
            let mut final_route = String::new();
            let mut chars = route.chars().peekable();
            let mut missing_args = Vec::new();

            while let Some(c) = chars.next() {
                if c == '{' && chars.peek() == Some(&'{') {
                    // Handle escaped placeholders `{{param}}`
                    chars.next(); // Skip second '{'
                    final_route.push('{');
                } else if c == '}' && chars.peek() == Some(&'}') {
                    chars.next(); // Skip second '}'
                    final_route.push('}');
                } else if c == '{' {
                    // Detect `{param}` placeholders
                    let mut placeholder = String::from("{");

                    while let Some(&next) = chars.peek() {
                        placeholder.push(next);
                        chars.next();
                        if next == '}' {
                            break;
                        }
                    }

                    // If still contains `{param}`, add to missing list
                    if placeholder.ends_with('}') {
                        missing_args.push(placeholder.clone());
                    }

                    final_route.push_str(&placeholder);
                } else {
                    final_route.push(c);
                }
            }

            // If there are missing placeholders, return an error
            if !missing_args.is_empty() {
                return Err(DomainSearchError::MissingArgs(missing_args));
            }

            // Restore escaped parameters `{{param}}` → `{param}`
            for (k, _) in args.iter() {
                let placeholder = format!("__ESCAPED_{}__", k);
                let param = format!("{{{}}}", k);
                final_route = final_route.replace(&placeholder, &param);
            }

            Ok(final_route)
        } else {
            Err(DomainSearchError::JobNotFound)
        }
    }

    /// Find matches for the given path in the domain
    /// the matches are sorted in order that they should be
    /// tested
    pub(crate) fn at(&self, path: &str) -> Option<(PathArgs, impl Iterator<Item = &Intent>)> {
        self.router
            .at(path)
            .map(|matched| {
                (
                    PathArgs::from(matched.params),
                    matched
                        .value
                        .iter()
                        .filter(|i| i.operation != Operation::None),
                )
            })
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::extract::{Target, Update};
    use crate::path::PathArgs;
    use crate::task::*;
    use crate::worker::*;

    fn plus_one(mut counter: Update<i32>, tgt: Target<i32>) -> Update<i32> {
        if *counter < *tgt {
            *counter += 1;
        }

        // Update implements IntoResult
        counter
    }

    fn plus_two(counter: Update<i32>, tgt: Target<i32>) -> Vec<Task> {
        if *tgt - *counter < 2 {
            // Returning an empty result tells the planner
            // the task is not applicable to reach the target
            return vec![];
        }

        vec![plus_one.with_target(*tgt), plus_one.with_target(*tgt)]
    }

    #[test]
    fn it_finds_jobs_ordered_by_degree() {
        let domain = Domain::new()
            .job("/counters/{counter}", update(plus_one))
            .job("/counters/{counter}", update(plus_two));

        let jobs: Vec<&str> = domain
            .at("/counters/{counter}")
            .map(|(_, iter)| iter.map(|i| i.job.id()).collect())
            .unwrap();

        // It should return compound jobs first
        assert_eq!(
            jobs,
            vec![plus_two.into_job().id(), plus_one.into_job().id()]
        );
    }

    #[test]
    fn it_ignores_none_jobs() {
        let domain = Domain::new()
            .job("/counters/{counter}", none(plus_one))
            .job("/counters/{counter}", update(plus_two));

        let jobs: Vec<&str> = domain
            .at("/counters/{counter}")
            .map(|(_, iter)| iter.map(|i| i.job.id()).collect())
            .unwrap();

        // It should not return jobs for None operations
        assert_eq!(jobs, vec![plus_two.into_job().id()]);
    }

    #[test]
    #[should_panic]
    fn it_fails_if_assigning_the_same_job_to_multiple_ops() {
        Domain::new()
            .job("/counters/{counter}", update(plus_one))
            .job("/counters/{counter}", update(plus_one));
    }

    #[test]
    #[should_panic]
    fn it_fails_if_assigning_the_same_job_to_multiple_routes() {
        Domain::new()
            .job("/counters/{counter}", update(plus_one))
            .job("/numbers/{counter}", create(plus_one));
    }

    #[test]
    fn it_constructs_a_path_given_arguments() {
        let domain = Domain::new()
            .job("/counters/{counter}", none(plus_one))
            .job("/counters/{counter}", update(plus_two));

        let args = PathArgs(vec![(Arc::from("counter"), String::from("one"))]);
        let path = domain.get_path(plus_one.into_job().id(), &args).unwrap();
        assert_eq!(path, String::from("/counters/one"))
    }

    #[test]
    fn test_wildcard_parameter_replacement() {
        let func = |file: Update<()>| file;
        let domain = Domain::new().job("/files/{*path}", update(func));

        let args = PathArgs(vec![(
            Arc::from("path"),
            "documents/report.pdf".to_string(),
        )]);
        let result = domain.get_path(func.into_job().id(), &args);

        assert_eq!(result, Ok("/files/documents/report.pdf".to_string()));
    }

    #[test]
    fn test_escaped_parameters_remain() {
        let func = |file: Update<()>| file;
        let domain = Domain::new().job("/data/{{counter}}/edit", update(func));

        let args = PathArgs(vec![(Arc::from("counter"), "456".to_string())]);
        let result = domain.get_path(func.into_job().id(), &args);

        assert_eq!(result, Ok("/data/{counter}/edit".to_string())); // Escaped `{counter}` remains unchanged
    }

    #[test]
    fn test_mixed_placeholders() {
        let func = |file: Update<()>| file;
        let domain = Domain::new().job("/users/{id}/files/{{file}}/{*path}", update(func));

        let args = PathArgs(vec![
            (Arc::from("id"), "42".to_string()),
            (Arc::from("path"), "reports/january.csv".to_string()),
        ]);
        let result = domain.get_path(func.into_job().id(), &args);

        assert_eq!(
            result,
            Ok("/users/42/files/{file}/reports/january.csv".to_string())
        );
    }

    #[test]
    fn test_no_replacement_if_job_not_found() {
        let func = |file: Update<()>| file;
        let domain = Domain::new();

        let args = PathArgs(vec![(Arc::from("counter"), "999".to_string())]);

        let result = domain.get_path(func.into_job().id(), &args);
        assert_eq!(result, Err(DomainSearchError::JobNotFound));
    }

    #[test]
    fn test_error_if_unmatched_placeholders_remain() {
        let func = |file: Update<()>| file;
        let domain = Domain::new().job("/tasks/{task_id}/check", update(func));

        let args = PathArgs(vec![]); // No arguments provided
        let result = domain.get_path(func.into_job().id(), &args);

        assert_eq!(
            result,
            Err(DomainSearchError::MissingArgs(
                vec!["{task_id}".to_string()]
            ))
        ); // Unmatched placeholders remain
    }
}
