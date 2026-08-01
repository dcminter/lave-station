//! End-to-end read path against a real daemon: resolve, connect, probe, list, render.
//!
//! Off by default. Run with `cargo test -p lave-core --features live-docker -- --nocapture`
//! to see exactly what the detail pane will show.

#![cfg(feature = "live-docker")]
#![allow(clippy::expect_used)]

use lave_core::endpoint::{SystemEnv, SystemPaths, resolve};
use lave_core::engine::{ContainerEngine, bollard_engine::BollardEngine};
use lave_core::model::{detail, tree};

fn utc() -> chrono::FixedOffset {
    chrono::FixedOffset::east_opt(0).expect("UTC is a valid offset")
}

fn now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after 1970")
            .as_secs(),
    )
    .expect("the current time fits in i64")
}

fn print_page(page: &detail::DetailPage) {
    println!("\n== {} ==", page.title);
    if let Some(subtitle) = &page.subtitle {
        println!("   {subtitle}");
    }
    for group in &page.groups {
        println!("  [{}]", group.title);
        for row in &group.rows {
            println!("    {:<22} {}", row.label, row.value);
        }
    }
    if let Some(raw) = &page.raw {
        println!("  [raw inspect: {} bytes]", raw.len());
    }
}

#[tokio::test]
async fn the_whole_read_path_produces_sensible_output() {
    let resolved = resolve(None, &SystemEnv, &SystemPaths).expect("a daemon is reachable");
    let engine = BollardEngine::connect(resolved.endpoint.path())
        .await
        .expect("connects to the daemon");

    let environment = engine.probe().await.expect("probe succeeds");
    let images = engine.list_images().await.expect("images listed");
    let containers = engine.list_containers().await.expect("containers listed");

    // The tree the sidebar will show.
    let root = tree::build(Some(&environment), &images, &containers);
    println!(
        "\n{} ({})",
        root.label,
        root.detail.clone().unwrap_or_default()
    );
    for node in &root.children {
        println!(
            "  {} [{}]",
            node.label,
            node.detail.clone().unwrap_or_default()
        );
        for child in node.children.iter().take(3) {
            println!(
                "    {} ({})",
                child.label,
                child.detail.clone().unwrap_or_default()
            );
        }
        if node.children.len() > 3 {
            println!("    ... {} more", node.children.len() - 3);
        }
    }

    assert_eq!(root.children.len(), 2);
    assert_eq!(
        root.children[0].children.len(),
        images.len(),
        "every image should appear in the tree"
    );
    assert_eq!(
        root.children[1].children.len(),
        containers.len(),
        "every container should appear in the tree"
    );

    // Every page the pane can show, for the real data.
    print_page(&detail::environment(&environment, &resolved, None));
    print_page(&detail::images(&images));
    print_page(&detail::containers(&containers));

    if let Some(image) = images.first() {
        let raw = engine
            .inspect_image(&image.id)
            .await
            .expect("image inspect succeeds");
        print_page(&detail::image(image, Some(&raw), now(), utc()));
    }

    if let Some(container) = containers.first() {
        let raw = engine
            .inspect_container(&container.id)
            .await
            .expect("container inspect succeeds");
        print_page(&detail::container(container, Some(&raw), now(), utc()));
    }

    // No page should render a field as an empty string; absent values say so.
    for page in [
        detail::environment(&environment, &resolved, None),
        detail::images(&images),
        detail::containers(&containers),
    ] {
        for group in &page.groups {
            for row in &group.rows {
                assert!(
                    !row.value.trim().is_empty(),
                    "{} / {} rendered blank",
                    group.title,
                    row.label
                );
            }
        }
    }
}
