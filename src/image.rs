use leptos::logging;
use leptos::prelude::*;
use leptos_meta::Link;
use base64::{engine::general_purpose, Engine as _};

// Make sure to import your updated ImageOptimizer structs/types from wherever they live:
use crate::optimizer::{ImageOptimizer, CachedImage, CachedImageOption, Blur, Resize};

/**
 * Renders an optimized static image with an optional blur placeholder and preload.
 *
 * The width/height props reserve layout space, preventing shift when the image or placeholder loads.
 */
#[component]
pub fn Image(
    /// Image source. Path relative to the public root (unless it's an external URL).
    #[prop(into)]
    src: String,

    /// Final image width in pixels.
    width: u32,

    /// Final image height in pixels.
    height: u32,

    /// Image quality (0-100) for the resized WebP.
    #[prop(default = 75_u8)]
    quality: u8,

    /// Whether to add a blur placeholder before the real image loads.
    #[prop(default = true)]
    blur: bool,

    /// Whether to add a `<link rel="preload" ...>` for this image.
    #[prop(default = false)]
    priority: bool,

    /// If `true`, use `loading="lazy"` for the final `<img>`.
    #[prop(default = true)]
    lazy: bool,

    /// The `<img>` alt text.
    #[prop(into, optional)]
    alt: String,

    /// Additional CSS classes for the `<img>`.
    #[prop(into, optional)]
    class: MaybeProp<String>,

    /// An optional fallback view while loading (inside `<Suspense>`).
    #[prop(into, optional)]
    fallback: Option<ViewFn>,
) -> impl IntoView {
    // If the user gave an external URL, skip optimization:
    if src.starts_with("http") {
        logging::debug_warn!("Image component only supports static images for SSR optimization.");
        let loading = if lazy { "lazy" } else { "eager" };
        return view! {
            <img
                src=src
                alt=alt
                class=class.get()
                width=width
                height=height
                decoding="async"
                loading=loading
            />
        }.into_any();
    }

    // Create descriptors for blur & optimized variants.
    let blur_image = StoredValue::new(CachedImage {
        src: src.clone(),
        option: CachedImageOption::Blur(Blur {
            width: 20,
            height: 20,
            svg_width: 100,
            svg_height: 100,
            sigma: 15,
        }),
    });

    let opt_image = StoredValue::new(CachedImage {
        src: src.clone(),
        option: CachedImageOption::Resize(Resize {
            quality,
            width,
            height,
        }),
    });

    // We'll get the global optimizer (or however your code obtains it).
    // For example, you might have a resource or a context accessor:
    let optimizer_resource = crate::use_image_cache_resource();

    let alt_stored = StoredValue::new(alt);

    // Build a fallback <div> to reserve space if none is provided.
    let fallback_view = move || {
        match fallback {
            Some(ref fallback_fn) => view! {
                <div style=move || format!("width: {width}px; height: {height}px;")>
                    {fallback_fn.run()}
                </div>
            }.into_any(),
            None => view! {
                <div style=move || {
                    format!("width: {width}px; height: {height}px; background-color: #f0f0f0;")
                } />
            }.into_any(),
        }
    };

    view! {
        <Suspense fallback=fallback_view>
            // Once our resource is ready, we either render a blurred placeholder or just the final <img>.
            {move || {
                optimizer_resource
                    .get()
                    .map(|optimizer| {
                        // We construct a final .webp route from the CachedImage -> `get_url_encoded(...)`
                        let opt_url = opt_image.get_value().get_url_encoded(&optimizer.api_handler_path);

                        if blur {
                            // Ask the optimizer if it has a blur SVG in memory:
                            let maybe_svg = optimizer.get_blur(&blur_image.get_value());
                            let svg_image = match maybe_svg {
                                Some(svg_data) => SvgImage::InMemory(svg_data),
                                None => {
                                    // Fallback: request from the server route.
                                    let placeholder_url = blur_image.get_value()
                                        .get_url_encoded(&optimizer.api_handler_path);
                                    SvgImage::Request(placeholder_url)
                                }
                            };

                            view! {
                                <CacheImage
                                    svg=svg_image
                                    opt_image=opt_url
                                    alt=alt_stored.get_value()
                                    class=class
                                    priority=priority
                                    lazy=lazy
                                    width=width
                                    height=height
                                />
                            }.into_any()

                        } else {
                            // No blur => just show the final <img>.
                            let loading = if lazy { "lazy" } else { "eager" };
                            view! {
                                <img
                                    src=opt_url
                                    alt=alt_stored.get_value()
                                    class=move || class.get()
                                    width=width
                                    height=height
                                    decoding="async"
                                    loading=loading
                                />
                            }.into_any()
                        }
                    })
            }}
        </Suspense>
    }.into_any()
}

/// Used internally for the blurred placeholder logic.
enum SvgImage {
    /// We already have an in‐memory SVG string.
    InMemory(String),
    /// We’ll request it from `/{handler_path}?...`.
    Request(String),
}

/// Internal subcomponent that shows an `<img>` with a blurred background (SVG).
#[component]
fn CacheImage(
    svg: SvgImage,
    #[prop(into)]
    opt_image: String,
    #[prop(into, optional)]
    alt: String,
    #[prop(into, optional)]
    class: MaybeProp<String>,
    priority: bool,
    lazy: bool,
    width: u32,
    height: u32,
) -> impl IntoView {
    let background_image = match svg {
        SvgImage::InMemory(svg_data) => {
            // Convert the raw SVG text into a data: URL so it can be used as CSS background-image.
            let encoded = general_purpose::STANDARD.encode(svg_data.as_bytes());
            format!("url('data:image/svg+xml;base64,{encoded}')")
        }
        SvgImage::Request(url) => {
            // We'll let the client request the `.svg` from the server route.
            format!("url('{url}')")
        }
    };

    // We apply the blur as a background while the final <img> is loading.
    // This ensures a low-res preview behind it.
    let style = format!(
        "color: transparent;\
         background-size: cover;\
         background-position: 50% 50%;\
         background-repeat: no-repeat;\
         background-image: {background_image};"
    );

    let loading = if lazy { "lazy" } else { "eager" };

    view! {
        // If priority is true, add a preload hint for the final image.
        {move || if priority {
            view! {
                <Link rel="preload" as_="image" href=opt_image.clone() />
            }.into_any()
        } else {
            ().into_any()
        }}

        <img
            src=opt_image
            alt=alt.clone()
            class=move || class.get()
            decoding="async"
            loading=loading
            width=width
            height=height
            style=style
        />
    }
}
