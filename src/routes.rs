//
// AXUM ROUTES
//
use axum::extract::FromRef;
use axum::{Router, body::Body, http::{Request, Response, Uri}, response::IntoResponse};
use tower_http::services::ServeDir;
use tower::util::ServiceExt;
use std::convert::Infallible;
use crate::ImageOptimizer;
use crate::optimizer::{BlurEntry, CachedImage, CachedImageOption, CreateImageError};

/// Trait to attach our image cache route onto an Axum router.
pub trait ImageCacheRoute<S>
where
    S: Clone + Send + Sync + 'static,
    ImageOptimizer: FromRef<S>,
{
    fn image_cache_route(self, state: &S) -> Self;
}

impl<S> ImageCacheRoute<S> for Router<S>
where
    S: Clone + Send + Sync + 'static,
    ImageOptimizer: FromRef<S>,
{
    fn image_cache_route(self, state: &S) -> Self {
        let optimizer = ImageOptimizer::from_ref(state);
        let path = optimizer.api_handler_path.clone();
        let handler = move |req: Request<Body>| cache_handler(optimizer.clone(), req);

        self.route(&path, axum::routing::get(handler))
    }
}

async fn cache_handler(
    optimizer: ImageOptimizer,
    req: Request<Body>,
) -> impl IntoResponse {
    let root = optimizer.root_file_path.clone();
    let uri = req.uri().clone();

    match check_cache_image(&optimizer, uri).await {
        Ok(Some(u)) => {
            match serve_from_disk(&root, u).await {
                Ok(resp) => resp.into_response(),
                Err(_) => Response::builder()
                    .status(404)
                    .body("Cannot serve from disk".to_string()).unwrap().into_response(),
            }
        },
        Ok(None) => Response::builder()
            .status(404)
            .body("Invalid Image".to_string()).unwrap().into_response(),
        Err(e) => {
            tracing::error!("Failed to create image: {:?}", e);
            Response::builder()
                .status(500)
                .body("Error creating image".to_string()).unwrap().into_response()
        }
    }
}

/// If the user provided valid query parameters, we generate the image (if needed).
/// Then return a URI to serve from disk.
async fn check_cache_image(
    optimizer: &ImageOptimizer,
    uri: Uri,
) -> Result<Option<Uri>, CreateImageError> {
    let url = uri.to_string();
    let img = match CachedImage::from_url_encoded(&url) {
        Ok(ci) => ci,
        Err(_) => return Ok(None),
    };

    let newly_created = optimizer.create_image(&img).await?;
    if newly_created {
        tracing::info!("Created image: {img}");
    }

    let relative_path = img.get_file_path();
    // If it's a blur, we can store it in memory for next time
    if let CachedImageOption::Blur(_) = img.option {
        add_blur_to_cache(optimizer, &img).await;
    }

    // Build a local path URI, e.g. "/cache/image/base64stuff/img.png.webp"
    let disk_uri = format!("/{}", relative_path);
    let parsed = disk_uri.parse::<Uri>().ok();
    Ok(parsed)
}

/// For blurred SVG placeholders, read the file from disk and store it in memory.
async fn add_blur_to_cache(
    optimizer: &ImageOptimizer,
    image: &CachedImage,
) {
    // If it's already in memory, skip
    if optimizer.blur_cache.get(image).is_none() {
        let file_path = optimizer.get_file_path_from_root(image);
        match tokio::fs::read_to_string(&file_path).await {
            Ok(svg_data) => {
                optimizer.blur_cache.insert(
                    image.clone(),
                    BlurEntry {
                        svg_data,
                        created_at: chrono::Utc::now(),
                    },
                );
                tracing::debug!("Added blur to cache; total={}", optimizer.blur_cache.len());
            }
            Err(e) => {
                tracing::error!("Failed to read blur file: {:?} => {:?}", file_path, e);
            }
        }
    }
}

/// Serve the file from disk using `ServeDir` once we've got a URI like "/cache/image/...".
async fn serve_from_disk(
    root: &str,
    uri: Uri,
) -> Result<Response<tower_http::services::fs::ServeFileSystemResponseBody>, Infallible> {
    let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
    ServeDir::new(root).oneshot(req).await
}

// --------------
//   Unit tests
// --------------
#[cfg(test)]
mod tests {
    use crate::optimizer::{Blur, CachedImage, CachedImageOption};
    use super::*;

    #[test]
    fn test_roundtrip_file_path() {
        let ci = CachedImage {
            src: "imgs/foo.png".into(),
            option: CachedImageOption::Blur(Blur {
                width: 10,
                height: 10,
                svg_width: 100,
                svg_height: 50,
                sigma: 8,
            }),
        };
        let p = ci.get_file_path();
        println!("file path = {p}");
        let back = CachedImage::from_file_path(&p).unwrap();
        assert_eq!(back, ci);
    }
}
