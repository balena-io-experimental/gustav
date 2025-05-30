use anyhow::{anyhow, Context as AnyhowCtx};
use bollard::container::{CreateContainerOptions, StartContainerOptions};
use bollard::secret::{ContainerInspectResponse, ContainerState, ContainerStateStatusEnum};
use futures_util::stream::StreamExt;
use mahler::worker::{Ready, Worker};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

use bollard::image::CreateImageOptions;
use bollard::Docker;

use mahler::extract::{Args, Pointer, Res, System, Target, View};
use mahler::task::prelude::*;

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone, Default)]
pub enum ServiceStatus {
    #[default]
    Created,
    Running,
    Stopped,
}

fn default_status() -> Option<ServiceStatus> {
    Some(ServiceStatus::default())
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TargetService {
    //// Image URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,

    /// Service status
    #[serde(skip_serializing_if = "Option::is_none", default = "default_status")]
    pub status: Option<ServiceStatus>,

    /// Container command configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmd: Option<Vec<String>>,
}

impl From<TargetService> for bollard::container::Config<String> {
    fn from(tgt: TargetService) -> bollard::container::Config<String> {
        bollard::container::Config {
            image: tgt.image,
            cmd: tgt.cmd,
            ..Default::default()
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Service {
    /// Container id for the service
    pub id: Option<String>,

    //// Image URL
    pub image: Option<String>,

    /// Service status
    pub status: Option<ServiceStatus>,

    /// Container creation date, not used as part
    /// of the target state
    pub created_at: Option<String>,

    /// Container last start date, not used as part
    pub started_at: Option<String>,

    /// Container last stop date, not used as part
    /// of the target state
    pub finished_at: Option<String>,

    /// Container command configuration
    pub cmd: Option<Vec<String>>,
}

#[derive(Debug, PartialEq, Eq)]
struct ServiceConfig {
    pub image: Option<String>,
    pub cmd: Option<Vec<String>>,
}

impl From<TargetService> for ServiceConfig {
    fn from(tgt: TargetService) -> Self {
        Self {
            image: tgt.image,
            cmd: tgt.cmd,
        }
    }
}

impl From<Service> for ServiceConfig {
    fn from(svc: Service) -> Self {
        Self {
            image: svc.image,
            cmd: svc.cmd,
        }
    }
}

impl From<Service> for TargetService {
    fn from(svc: Service) -> Self {
        Self {
            image: svc.image,
            status: svc.status,
            cmd: svc.cmd,
        }
    }
}

impl From<ContainerInspectResponse> for Service {
    fn from(info: ContainerInspectResponse) -> Self {
        let ContainerInspectResponse {
            id,
            image,
            created: created_at,
            state,
            config,
            ..
        } = info;

        // Get status info
        let (status, started_at, finished_at) = if let Some(ContainerState {
            status,
            started_at,
            finished_at,
            ..
        }) = state
        {
            let status = match status {
                Some(ContainerStateStatusEnum::RUNNING) => Some(ServiceStatus::Running),
                Some(ContainerStateStatusEnum::EXITED) => Some(ServiceStatus::Stopped),
                Some(ContainerStateStatusEnum::CREATED) => Some(ServiceStatus::Created),
                Some(_) => None, // ignore other statuses for now
                None => None,
            };

            (status, started_at, finished_at)
        } else {
            (None, None, None)
        };

        let cmd = config.and_then(|c| c.cmd);

        Self {
            id,
            image,
            status,
            created_at,
            started_at,
            finished_at,
            cmd,
        }
    }
}

impl From<TargetService> for Service {
    fn from(tgt: TargetService) -> Self {
        let TargetService {
            image,
            status,
            cmd: command,
        } = tgt;
        Self {
            id: None,
            image,
            status,
            created_at: None,
            started_at: None,
            finished_at: None,
            cmd: command,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Image {
    /// Image Id on the engine
    pub id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Project {
    /// Project name
    pub name: String,

    /// List of project services
    pub services: HashMap<String, Service>,

    /// List of managed images
    pub images: HashMap<String, Image>,
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            services: HashMap::new(),
            images: HashMap::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TargetProject {
    /// Project name
    pub name: String,

    /// List of target services
    pub services: HashMap<String, TargetService>,
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct FetchImageError(#[from] anyhow::Error);

/// Pull an image from the registry, this task is applicable to
/// the creation of a new image
///
/// Condition: the image is not already present in the device
/// Effect: add the image to the list of images
/// Action: pull the image from the registry and add it to the images local registry
fn fetch_image(
    mut image: Pointer<Image>,
    Args(image_name): Args<String>,
    docker: Res<Docker>,
) -> Create<Image, FetchImageError> {
    // Initialize the image if it doesn't exist
    if image.is_none() {
        image.zero();
    }

    with_io(image, |mut image| async move {
        // Check if the image exists first, we do this because
        // we don't know if the initial state is correct
        match docker.inspect_image(&image_name).await {
            Ok(img_info) => {
                if let Some(id) = img_info.id {
                    // If the image exists and has an id, skip
                    // download
                    image.replace(Image { id: Some(id) });
                    return Ok(image);
                }
            }
            Err(e) => {
                if let bollard::errors::Error::DockerResponseServerError { status_code, .. } = e {
                    if status_code != 404 {
                        return Err(e).with_context(|| {
                            format!("failed to read information for image {image_name}")
                        })?;
                    }
                } else {
                    return Err(e).with_context(|| {
                        format!("failed to read information for image {image_name}")
                    })?;
                }
            }
        }

        // Otherwise try to download the image
        let options = Some(CreateImageOptions {
            from_image: image_name.clone(),
            ..Default::default()
        });

        // Try to create the image
        let mut stream = docker.create_image(options, None, None);
        while let Some(progress) = stream.next().await {
            let _ = progress.with_context(|| format!("failed to download image {image_name}"))?;
        }

        // Check that the image
        let img_info = docker
            .inspect_image(&image_name)
            .await
            .with_context(|| format!("failed to read information for image {image_name}"))?;

        image.assign(Image { id: img_info.id });

        Ok(image)
    })
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct RemoveImageError(#[from] anyhow::Error);

/// Remove an image
///
/// Condition: the image exists (and there are no services referencing it?)
/// Effect: remove the image from the state
/// Action: remove the image from the engine
fn remove_image(
    img_ptr: Pointer<Image>,
    Args(image_name): Args<String>,
    System(project): System<Project>,
    docker: Res<Docker>,
) -> Delete<Image, RemoveImageError> {
    // only remove the image if it not being used by any service
    if project
        .services
        .values()
        .any(|s| s.image == Some(image_name.clone()))
    {
        return img_ptr.into();
    }

    with_io(img_ptr, |img_ptr| async move {
        docker
            .remove_image(&image_name, None, None)
            .await
            .with_context(|| format!("failed to remove image {image_name}"))?;

        Ok(img_ptr)
    })
    .map(|img_ptr| {
        // Delete the service if it is not running
        img_ptr.unassign()
    })
}

/// Pull an image from the registry, this task is applicable to
/// the creation of a service, as pulling an image is only needed
/// in that case.
fn fetch_service_image(Target(tgt): Target<TargetService>) -> Option<Task> {
    tgt.image.map(|img| fetch_image.with_arg("image_name", img))
}

async fn image_matches_with_target(
    docker: &Res<Docker>,
    tgt_img: &Option<String>,
    img_id: &Option<String>,
) -> bool {
    if let Some(img) = tgt_img {
        if let Ok(info) = docker.inspect_image(img).await {
            return &info.id == img_id;
        }
    }

    false
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct InstallServiceError(#[from] anyhow::Error);

/// Create a new service container from service data
///
/// Condition: the service is not already present in the `services` object and the service image has already been downloaded
/// Effect: add the service to the `services` object, with a `status` of `created`
/// Action: create a new container using the docker API and set the `containerId` property of the service in the `services` object
fn install_service(
    mut svc_ptr: Pointer<Service>,
    Args(service_name): Args<String>,
    System(project): System<Project>,
    Target(tgt): Target<TargetService>,
    docker: Res<Docker>,
) -> Create<Service, InstallServiceError> {
    let tgt_img = tgt.image.clone();
    let local_img = tgt_img.as_ref().and_then(|img| project.images.get(img));

    // If the image has already been downloaded then
    // simulate the service install
    if local_img.is_some() {
        svc_ptr.assign(Service {
            // The status should be 'Created' after install
            status: Some(ServiceStatus::Created),
            // The rest of the fields should be the same
            ..tgt.clone().into()
        });
    }

    with_io(svc_ptr, |mut svc_ptr| async move {
        let container_name = format!("{}_{}", project.name, service_name);
        match docker.inspect_container(&container_name, None).await {
            Ok(svc_info) => {
                let mut svc: Service = svc_info.into();

                // If the existing service has the same image id as the
                // locally stored image, then we replace it with the target img
                // name
                if image_matches_with_target(&docker, &tgt_img, &svc.image).await {
                    svc.image = tgt_img;
                }
                svc_ptr.assign(svc);

                return Ok(svc_ptr);
            }
            Err(e) => {
                if let bollard::errors::Error::DockerResponseServerError { status_code, .. } = e {
                    if status_code != 404 {
                        return Err(e).with_context(|| {
                            format!(
                                "failed to read container information for service {service_name}"
                            )
                        })?;
                    }
                } else {
                    return Err(e).with_context(|| {
                        format!("failed to read container information for service {service_name}")
                    })?;
                }
            }
        };

        let options = Some(CreateContainerOptions {
            name: container_name.clone(),
            platform: None,
        });

        docker
            .create_container(options, tgt.into())
            .await
            .with_context(|| format!("failed to create container for service {service_name}"))?;

        // Look for the new service
        let mut svc: Service = docker
            .inspect_container(&container_name, None)
            .await
            .with_context(|| {
                format!("failed to read container information for service {service_name}")
            })?
            .into();

        if image_matches_with_target(&docker, &tgt_img, &svc.image).await {
            svc.image = tgt_img;
        }

        // Assign the pointer to the new service info
        svc_ptr.assign(svc);

        Ok(svc_ptr)
    })
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct StartServiceError(#[from] anyhow::Error);

/// Start a service container
///
/// Condition: the service has been created and has the same configuration as the target
/// Effect: set the service status to Running
/// Action: start the container
fn start_service(
    mut svc_view: View<Service>,
    Args(service_name): Args<String>,
    Target(tgt): Target<TargetService>,
    docker: Res<Docker>,
) -> Update<Service, StartServiceError> {
    let svc_config = ServiceConfig::from(svc_view.clone());
    let tgt_img = tgt.image.clone();
    let tgt_config = ServiceConfig::from(tgt);

    // If configurations match, then update the service status
    if tgt_config == svc_config && !matches!(svc_view.status, Some(ServiceStatus::Running)) {
        svc_view.status = Some(ServiceStatus::Running);
    }

    with_io(svc_view, |mut svc_view| async move {
        if let Some(ref id) = svc_view.id {
            docker
                .start_container(id, None::<StartContainerOptions<String>>)
                .await
                .with_context(|| format!("failed to start container for {service_name}"))?;

            // Inspect the service
            let mut svc: Service = docker
                .inspect_container(id, None)
                .await
                .with_context(|| {
                    format!("failed to read container information for service {service_name}")
                })?
                .into();

            if image_matches_with_target(&docker, &tgt_img, &svc.image).await {
                svc.image = tgt_img;
            }
            *svc_view = svc;
        } else {
            Err(anyhow!("no container Id for {service_name}"))?;
        }

        Ok(svc_view)
    })
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct StopServiceError(#[from] anyhow::Error);

/// Stop a service container
///
/// Condition: the service exists and is running
/// Effect: set the service status to Stopped
/// Action: stop the container
fn stop_service(
    mut svc_view: View<Service>,
    Args(service_name): Args<String>,
    Target(tgt): Target<TargetService>,
    docker: Res<Docker>,
) -> Update<Service, StartServiceError> {
    let tgt_img = tgt.image.clone();

    if matches!(svc_view.status, Some(ServiceStatus::Running)) {
        svc_view.status = Some(ServiceStatus::Stopped);
    }

    with_io(svc_view, |mut svc_view| async move {
        if let Some(ref id) = svc_view.id {
            docker
                .stop_container(id, None)
                .await
                .with_context(|| format!("failed to stop container for {service_name}"))?;

            // Inspect the service
            let mut svc: Service = docker
                .inspect_container(id, None)
                .await
                .with_context(|| {
                    format!("failed to read container information for service {service_name}")
                })?
                .into();

            if image_matches_with_target(&docker, &tgt_img, &svc.image).await {
                svc.image = tgt_img;
            }
            *svc_view = svc;
        } else {
            // can this happen?
            Err(anyhow!("no container for {service_name}"))?;
        }

        Ok(svc_view)
    })
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct UninstallServiceError(#[from] anyhow::Error);

/// Remove a service container
///
/// Condition: the service exists and is not running
/// Effect: remove the service from the state
/// Action: remove the container
fn uninstall_service(
    svc_ptr: Pointer<Service>,
    Args(service_name): Args<String>,
    docker: Res<Docker>,
) -> Delete<Service, StartServiceError> {
    if svc_ptr
        .as_ref()
        .is_none_or(|svc| matches!(svc.status, Some(ServiceStatus::Running)))
    {
        // do nothing if the service is still running
        return svc_ptr.into();
    }

    with_io(svc_ptr, |svc_ptr| async move {
        if let Some(id) = svc_ptr.as_ref().and_then(|svc| svc.id.as_ref()) {
            docker
                .remove_container(
                    id,
                    Some(bollard::container::RemoveContainerOptions {
                        v: true,
                        ..Default::default()
                    }),
                )
                .await
                .with_context(|| format!("failed to remove container for {service_name}"))?;
        } else {
            Err(anyhow!("no container for {service_name}"))?;
        }

        Ok(svc_ptr)
    })
    .map(|svc_ptr| svc_ptr.unassign())
}

/// Recreate the service on configuration change
fn update_service(svc_view: View<Service>, Target(tgt): Target<TargetService>) -> Vec<Task> {
    let mut tasks = Vec::new();
    if svc_view.image != tgt.image {
        tasks.push(fetch_service_image.with_target(tgt.clone()));
        tasks.push(stop_and_uninstall_service.into_task());
        tasks.push(install_service.with_target(tgt));

        if let Some(img_name) = svc_view.image.as_ref() {
            // Remove the existing image
            tasks.push(remove_image.with_arg("image_name", img_name));
        }
    }
    tasks
}

/// Stop and remove a service container method
///
/// Condition: the service exists and is running
/// Effect: stop and remove the service from the state
/// Action: remove the container
fn stop_and_uninstall_service(svc_view: View<Service>) -> Vec<Task> {
    let mut actions = vec![];
    if matches!(svc_view.status, Some(ServiceStatus::Running)) {
        actions.push(stop_service.with_target(TargetService::from(svc_view.to_owned())));
    }

    actions.push(uninstall_service.into_task());

    actions
}

/// Remove the service and its image
fn purge_service(svc_view: View<Service>) -> Vec<Task> {
    let mut actions = vec![];
    actions.push(stop_and_uninstall_service.into_task());

    if let Some(ref image) = svc_view.image {
        actions.push(remove_image.with_arg("image_name", image));
    }

    actions
}

pub fn create_worker(project: Project) -> Worker<Project, Ready, TargetProject> {
    // Initialize the connection
    let docker = Docker::connect_with_defaults().unwrap();

    Worker::new()
        .jobs(
            "/images/{image_name}",
            [
                create(fetch_image).with_description(|Args(image_name): Args<String>| {
                    format!("pull image '{image_name}'")
                }),
                none(remove_image).with_description(|Args(image_name): Args<String>| {
                    format!("remove image '{image_name}'")
                }),
            ],
        )
        .jobs(
            "/services/{service_name}",
            [
                create(fetch_service_image),
                create(install_service).with_description(|Args(service_name): Args<String>| {
                    format!("install container for service '{service_name}'")
                }),
                update(start_service).with_description(|Args(service_name): Args<String>| {
                    format!("start container for service '{service_name}'")
                }),
                update(stop_service).with_description(|Args(service_name): Args<String>| {
                    format!("stop container for service '{service_name}'")
                }),
                // Use any for uninstall so it is applicable to configuration change
                any(uninstall_service).with_description(|Args(service_name): Args<String>| {
                    format!("remove container for service '{service_name}'")
                }),
                // give this method higher priority than stop and uninstall
                // service (default is 0)
                delete(purge_service).with_priority(8),
                delete(stop_and_uninstall_service),
                update(update_service),
            ],
        )
        .resource::<Docker>(docker)
        .initial_state(project)
        .unwrap()
}

#[cfg(test)]
mod tests {
    use bollard::container::{ListContainersOptions, RemoveContainerOptions};
    use mahler::worker::SeekStatus;
    use mahler::{seq, Dag};
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tracing_subscriber::fmt::format::FmtSpan;
    use tracing_subscriber::{prelude::*, EnvFilter};

    use super::*;

    fn before() {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .pretty()
                    .with_target(false)
                    .with_thread_names(true)
                    .with_thread_ids(true)
                    .with_line_number(true)
                    .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE),
            )
            .with(EnvFilter::from_default_env())
            .try_init()
            .unwrap_or(());
    }

    const PROJECT_NAME: &str = "my-project";

    async fn cleanup() {
        let docker = Docker::connect_with_defaults().unwrap();
        let containers = docker
            .list_containers(Some(ListContainersOptions::<String> {
                all: true,
                ..Default::default()
            }))
            .await
            .map(|containers| {
                containers
                    .into_iter()
                    .filter(|c| {
                        c.names.iter().any(|names| {
                            names
                                .iter()
                                .any(|name| name.starts_with(&format!("/{}_", PROJECT_NAME)))
                        })
                    })
                    .collect()
            })
            .unwrap_or(vec![]);

        // Delete containers
        for c in containers {
            if let Some(id) = c.id {
                docker
                    .remove_container(
                        &id,
                        Some(RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await
                    .unwrap();
            }
        }

        // Delete unused images
        let mut filters = HashMap::new();
        filters.insert("dangling", vec!["false"]);
        let _ = docker
            .prune_images(Some(bollard::image::PruneImagesOptions { filters }))
            .await;
    }

    #[tokio::test]
    async fn it_can_fetch_image() {
        before();

        let docker = Docker::connect_with_defaults().unwrap();
        let _ = docker.info().await.unwrap();

        let worker = create_worker(Project::new(PROJECT_NAME));
        let state = worker
            .run_task(fetch_image.with_arg("image_name", "alpine:3.18"))
            .await
            .unwrap();

        // The alpine image must exist now
        let img = docker.inspect_image("alpine:3.18").await.unwrap();
        assert_eq!(state.images.get("alpine:3.18").unwrap().id, img.id);

        // cleanup
        cleanup().await
    }

    #[tokio::test]
    async fn it_can_start_container() {
        before();

        let worker = create_worker(Project::new(PROJECT_NAME));
        let target = serde_json::from_value::<TargetProject>(json!({
            "name": "my-project",
            "services": {
                "my-service": {
                    "image": "alpine:3.18",
                    "cmd": ["sleep", "infinity"],
                    "status": "Running"
                }
            }
        }))
        .unwrap();

        // Seeking the target must succeed
        let worker = worker.seek_target(target.clone()).await.unwrap();
        assert_eq!(worker.status(), &SeekStatus::Success);

        // The alpine image must exist now
        let docker = Docker::connect_with_defaults().unwrap();
        let img = docker.inspect_image("alpine:3.18").await.unwrap();

        // The image ids should match
        let state = worker.state().await.unwrap();
        assert_eq!(state.images.get("alpine:3.18").unwrap().id, img.id);

        let container = docker
            .inspect_container(&format!("{}_my-service", PROJECT_NAME), None)
            .await
            .unwrap();
        assert_eq!(container.id, state.services.get("my-service").unwrap().id);
        assert_eq!(
            container.state.unwrap().status,
            Some(ContainerStateStatusEnum::RUNNING)
        );

        // cleanup
        cleanup().await
    }

    #[tokio::test]
    async fn it_finds_a_workflow_to_start_service() {
        before();

        let initial_state = serde_json::from_value::<Project>(json!({
            "name": "my-project",
            "images": {},
            "services": {}
        }))
        .unwrap();
        let worker = create_worker(initial_state);
        let target = serde_json::from_value::<TargetProject>(json!({
            "name": "my-project",
            "services": {
                "my-service": {
                    "image": "alpine:3.18",
                    "cmd": ["sleep", "infinity"],
                    "status": "Running"
                }
            }
        }))
        .unwrap();

        let workflow = worker.find_workflow(target).await.unwrap();
        let expected: Dag<&str> = seq!(
            "pull image 'alpine:3.18'",
            "install container for service 'my-service'",
            "start container for service 'my-service'"
        );
        assert_eq!(workflow.to_string(), expected.to_string());
    }

    #[tokio::test]
    async fn it_finds_a_workflow_to_recreate_service() {
        before();

        let initial_state = serde_json::from_value::<Project>(json!({
            "name": "my-project",
            "images": {
                "alpine:3.18": {}
            },
            "services": {
                "my-service": {
                    "image": "alpine:3.18",
                    "cmd": ["sleep", "infinity"],
                    "status": "Running"
                }
            }
        }))
        .unwrap();
        let worker = create_worker(initial_state);
        let target = serde_json::from_value::<TargetProject>(json!({
            "name": "my-project",
            "services": {
                "my-service": {
                    "image": "alpine:3.18",
                    "cmd": ["sleep", "30"],
                    "status": "Running"
                }
            }
        }))
        .unwrap();

        let workflow = worker.find_workflow(target).await.unwrap();
        let expected: Dag<&str> = seq!(
            "stop container for service 'my-service'",
            "remove container for service 'my-service'",
            "install container for service 'my-service'",
            "start container for service 'my-service'"
        );
        assert_eq!(workflow.to_string(), expected.to_string());
    }

    #[tokio::test]
    async fn it_finds_a_workflow_to_update_service() {
        before();

        let initial_state = serde_json::from_value::<Project>(json!({
            "name": "my-project",
            "images": {
                "alpine:3.18": {}
            },
            "services": {
                "my-service": {
                    "image": "alpine:3.18",
                    "cmd": ["sleep", "infinity"],
                    "status": "Running"
                }
            }
        }))
        .unwrap();
        let worker = create_worker(initial_state);
        let target = serde_json::from_value::<TargetProject>(json!({
            "name": "my-project",
            "services": {
                "my-service": {
                    "image": "alpine:3.20",
                    "cmd": ["sleep", "infinity"],
                    "status": "Running"
                }
            }
        }))
        .unwrap();

        let workflow = worker.find_workflow(target).await.unwrap();
        let expected: Dag<&str> = seq!(
            "pull image 'alpine:3.20'",
            "stop container for service 'my-service'",
            "remove container for service 'my-service'",
            "install container for service 'my-service'",
            "remove image 'alpine:3.18'",
            "start container for service 'my-service'",
        );
        assert_eq!(workflow.to_string(), expected.to_string());
    }

    #[tokio::test]
    async fn test_create_start_and_stop_container() {
        before();

        let worker = create_worker(Project::new(PROJECT_NAME));
        let target = serde_json::from_value::<TargetProject>(json!({
            "name": "my-project",
            "services": {
                "my-service": {
                    "image": "alpine:3.18",
                    "cmd": ["sleep", "infinity"]
                }
            }
        }))
        .unwrap();

        // Seeking the target must succeed
        let worker = worker.seek_target(target).await.unwrap();
        assert_eq!(worker.status(), &SeekStatus::Success);

        // The alpine image must exist now
        let docker = Docker::connect_with_defaults().unwrap();
        let img = docker.inspect_image("alpine:3.18").await.unwrap();

        // The image ids should match
        let state = worker.state().await.unwrap();
        assert_eq!(state.images.get("alpine:3.18").unwrap().id, img.id);

        let container = docker
            .inspect_container(&format!("{}_my-service", PROJECT_NAME), None)
            .await
            .unwrap();
        assert_eq!(container.id, state.services.get("my-service").unwrap().id);
        assert_eq!(
            container.state.unwrap().status,
            Some(ContainerStateStatusEnum::CREATED)
        );
        let old_container_id = container.id;

        let target = serde_json::from_value::<TargetProject>(json!({
            "name": "my-project",
            "services": {
                "my-service": {
                    "image": "alpine:3.18",
                    "cmd": ["sleep", "infinity"],
                    "status": "Running"
                }
            }
        }))
        .unwrap();

        let workflow = worker.find_workflow(target.clone()).await.unwrap();
        let expected: Dag<&str> = seq!("start container for service 'my-service'");
        assert_eq!(workflow.to_string(), expected.to_string());

        // Seeking the target must succeed
        let worker = worker.seek_target(target).await.unwrap();
        assert_eq!(worker.status(), &SeekStatus::Success);

        // The container ids should match
        let container = docker
            .inspect_container(&format!("{}_my-service", PROJECT_NAME), None)
            .await
            .unwrap();
        assert_eq!(old_container_id, container.id);
        assert_eq!(
            container.state.unwrap().status,
            Some(ContainerStateStatusEnum::RUNNING)
        );

        let target = serde_json::from_value::<TargetProject>(json!({
            "name": "my-project",
            "services": {
                "my-service": {
                    "image": "alpine:3.18",
                    "cmd": ["sleep", "infinity"],
                    "status": "Stopped"
                }
            }
        }))
        .unwrap();

        // Seeking the target must succeed
        let worker = worker.seek_target(target).await.unwrap();
        assert_eq!(worker.status(), &SeekStatus::Success);

        // The container ids should match
        let container = docker
            .inspect_container(&format!("{}_my-service", PROJECT_NAME), None)
            .await
            .unwrap();
        assert_eq!(old_container_id, container.id);
        assert_eq!(
            container.state.unwrap().status,
            Some(ContainerStateStatusEnum::EXITED)
        );

        let target = serde_json::from_value::<TargetProject>(json!({
            "name": "my-project",
            "services": {}
        }))
        .unwrap();

        // Seeking the target must succeed
        let worker = worker.seek_target(target).await.unwrap();
        assert_eq!(worker.status(), &SeekStatus::Success);

        // The container should no longer exist
        let container = docker
            .inspect_container(&format!("{}_my-service", PROJECT_NAME), None)
            .await;
        assert!(matches!(
            container,
            Err(bollard::errors::Error::DockerResponseServerError { .. })
        ));

        // The image should no longer exist
        let image = docker.inspect_image("alpine:3.18").await;
        assert!(matches!(
            image,
            Err(bollard::errors::Error::DockerResponseServerError { .. })
        ));

        // cleanup
        cleanup().await
    }
}
