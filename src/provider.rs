use leptos::logging::log;
use crate::optimizer::CachedImage;
use leptos::prelude::*;

/// Provides Image Cache Context so that Images can use their blur placeholders if they exist.
///
/// This should go in the base of your Leptos <App/>.
///
/// Example
///
/// ```
/// use leptos::*;
///
/// #[component]
/// pub fn App() -> impl IntoView {
///     leptos_image::provide_image_context();
///
///     view!{
///       <div/>
///     }
/// }
///
/// ```
pub fn provide_image_context() {
    let resource: Resource<ImageConfig> = new_image_resource();
    leptos::prelude::provide_context(resource);
}

pub fn new_image_resource() -> Resource<ImageConfig> {
    Resource::new_blocking(
        || (),
        |_| async {
            log!("Calling");
            get_image_config()
                .await
                .unwrap_or_default()
                // .expect("Failed to retrieve image cache")
        },
    )
}

type ImageResource = Resource<ImageConfig>;

#[doc(hidden)]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ImageConfig {
    pub(crate) api_handler_path: String,
    pub(crate) cache: Vec<(CachedImage, String)>,
}

pub(crate) fn use_image_cache_resource() -> Resource<ImageConfig> {
    use_context::<Resource<ImageConfig>>().expect("Missing Image Resource")
}

#[server(GetImageCache)]
pub(crate) async fn get_image_config() -> Result<ImageConfig, ServerFnError> {
    tracing::info!("1");
    let optimizer = use_optimizer()?;
    tracing::info!("2");

    let cache = optimizer
        .cache
        .iter()
        .map(|entry| (entry.key().clone(), entry.value().clone()))
        .collect();

    let api_handler_path = optimizer.api_handler_path.clone();

    Ok(ImageConfig {
        api_handler_path,
        cache,
    })
}

#[cfg(feature = "ssr")]
pub(crate) fn use_optimizer() -> Result<crate::ImageOptimizer, ServerFnError> {
    //use axum::{extract::Query, http::Method};
    //use leptos_axum::extract;
    tracing::debug!("Calling use_optimizer");
    use_context::<crate::ImageOptimizer>()
        .ok_or_else(|| ServerFnError::ServerError("Image Optimizer Missing.".into()))
}
