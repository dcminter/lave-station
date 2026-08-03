//! End-to-end read path against a real daemon: resolve, connect, probe, list, render.
//!
//! Off by default. Run with `cargo test -p lave-core --features live-docker -- --nocapture`
//! to see exactly what the detail pane will show.

#![cfg(feature = "live-docker")]
#![allow(clippy::expect_used)]

use lave_core::endpoint::{SystemEnv, SystemPaths, resolve};
use lave_core::engine::{ContainerEngine, bollard_engine::BollardEngine};
use lave_core::model::relations::{self, LayerIndex};
use lave_core::model::tree::NodeId;
use lave_core::model::{detail, dockerfile, tree};

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

fn print_tree(root: &tree::TreeNode) {
    let spoken = |node: &tree::TreeNode| node.description.clone().unwrap_or_default();

    println!("\n{} ({})", root.label, spoken(root));
    for node in &root.children {
        println!("  {} [{}]", node.label, spoken(node));
        for child in node.children.iter().take(3) {
            println!("    {} ({})", child.label, spoken(child));
        }
        if node.children.len() > 3 {
            println!("    ... {} more", node.children.len() - 3);
        }
    }
}

fn print_page(page: &detail::DetailPage) {
    println!("\n== {} ==", page.title);
    if let Some(subtitle) = &page.subtitle {
        println!("   {subtitle}");
    }
    for group in &page.groups {
        println!("  [{}]", group.title);
        for row in &group.rows {
            let arrow = if row.link.is_some() { "->" } else { "  " };
            println!("  {arrow}{:<22} {}", row.label, row.value);
        }
    }
    if let Some(table) = &page.table {
        println!("  [table: {}]", table.column_titles().join(" | "));
        for index in 0..table.rows.len().min(5) {
            let cells: Vec<String> = table.rows[index]
                .cells
                .iter()
                .map(|cell| cell.text.clone())
                .collect();
            println!("    {}", cells.join(" | "));
        }
        if table.rows.len() > 5 {
            println!("    ... {} more", table.rows.len() - 5);
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

    let mut layers = LayerIndex::new();
    for image in &images {
        let digests = engine
            .image_layers(&image.id)
            .await
            .expect("image layers read");
        layers.insert(&image.id, digests);
    }

    let cx = detail::Context {
        images: &images,
        containers: &containers,
        layers: &layers,
        raw: None,
        now: now(),
        offset: utc(),
        show_stopped: true,
        show_untagged: true,
    };

    // The tree the sidebar will show.
    let root = tree::build(Some(&environment), &images, &containers);
    print_tree(&root);

    assert_eq!(root.children.len(), 2);
    // By identity, not by position: which of the two comes first is `tree`'s decision and
    // is asserted there.
    assert_eq!(
        root.child(&NodeId::Images).map(|node| node.children.len()),
        Some(images.len()),
        "every image should appear in the tree"
    );
    assert_eq!(
        root.child(&NodeId::Containers)
            .map(|node| node.children.len()),
        Some(containers.len()),
        "every container should appear in the tree"
    );

    // Every page the pane can show, for the real data.
    print_page(&detail::environment(&environment, &resolved, &cx));
    print_page(&detail::images(&cx));
    print_page(&detail::containers(&cx));

    if let Some(image) = images.first() {
        let raw = engine
            .inspect_image(&image.id)
            .await
            .expect("image inspect succeeds");
        let cx = detail::Context {
            raw: Some(&raw),
            ..cx
        };
        print_page(&detail::image(image, &cx));
    }

    if let Some(container) = containers.first() {
        let raw = engine
            .inspect_container(&container.id)
            .await
            .expect("container inspect succeeds");
        let cx = detail::Context {
            raw: Some(&raw),
            ..cx
        };
        print_page(&detail::container(container, &cx));
    }

    // No page should render a field as an empty string; absent values say so.
    for page in [
        detail::environment(&environment, &resolved, &cx),
        detail::images(&cx),
        detail::containers(&cx),
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

/// The relationships Version 2 adds, against whatever the daemon actually holds.
#[tokio::test]
async fn relationships_hold_together_on_real_data() {
    let resolved = resolve(None, &SystemEnv, &SystemPaths).expect("a daemon is reachable");
    let engine = BollardEngine::connect(resolved.endpoint.path())
        .await
        .expect("connects to the daemon");

    let images = engine.list_images().await.expect("images listed");
    let containers = engine.list_containers().await.expect("containers listed");

    let mut layers = LayerIndex::new();
    for image in &images {
        let digests = engine
            .image_layers(&image.id)
            .await
            .expect("image layers read");
        layers.insert(&image.id, digests);
    }

    println!("\n== derivation, reconstructed from shared layer prefixes ==");
    let mut derivations = 0;
    for image in &images {
        if let Some(base) = relations::base_of(image, &images, &layers) {
            derivations += 1;
            let shared = layers.get(&base.id).map_or(0, <[String]>::len);
            let own = layers.get(&image.id).map_or(0, <[String]>::len);
            println!(
                "  {} <- FROM {} ({shared} of {own} layers)",
                lave_core::model::format::image_label(image),
                lave_core::model::format::image_label(base)
            );
        }
    }
    println!(
        "  {derivations} derivations found among {} images",
        images.len()
    );

    // base_of and derived_from must be exact duals, whatever the real data looks like.
    for image in &images {
        for child in relations::derived_from(image, &images, &layers) {
            assert_eq!(
                relations::base_of(child, &images, &layers).map(|base| &base.id),
                Some(&image.id),
                "{} listed {} as derived, but that is not its base",
                image.id,
                child.id
            );
        }
        // Nothing is its own ancestor.
        assert!(
            relations::base_of(image, &images, &layers).is_none_or(|base| base.id != image.id),
            "{} was reported as its own base",
            image.id
        );
    }

    println!("\n== containers and the images they run ==");
    for container in &containers {
        let running = relations::running_image(container, &images);
        let moved = relations::tag_has_moved(container, &images);
        println!(
            "  {:<28} {:<34} {}",
            lave_core::model::format::container_label(container),
            container.image,
            match (running, moved) {
                (Some(_), false) => "image present, tag agrees".to_owned(),
                (Some(_), true) => "TAG HAS MOVED since this container was created".to_owned(),
                (None, _) => "image no longer present".to_owned(),
            }
        );

        // Whenever a tag has moved, both images must be distinct and both reachable.
        if moved {
            let tagged = relations::tagged_image(container, &images).expect("tag resolves");
            assert_ne!(
                tagged.id, container.image_id,
                "a moved tag must name a different image"
            );
        }
    }

    // Every container the daemon lists is accounted for by exactly one image, or none.
    for image in &images {
        for container in relations::containers_of(image, &containers) {
            assert_eq!(container.image_id, image.id);
        }
    }
}

/// Reconstruct a Dockerfile from whatever the daemon actually holds.
///
/// The fixtures in `model::dockerfile` cover the two recorded forms; this checks the
/// whole chain — history, base resolution by layer prefix, and the boundary between the
/// base's records and the image's own — against real images.
#[tokio::test]
async fn dockerfiles_reconstruct_from_real_images() {
    let resolved = resolve(None, &SystemEnv, &SystemPaths).expect("a daemon is reachable");
    let engine = BollardEngine::connect(resolved.endpoint.path())
        .await
        .expect("connects to the daemon");

    let images = engine.list_images().await.expect("images listed");
    let mut layers = LayerIndex::new();
    for image in &images {
        let digests = engine
            .image_layers(&image.id)
            .await
            .expect("image layers read");
        layers.insert(&image.id, digests);
    }

    let mut reconstructed = 0;

    for image in images.iter().take(6) {
        let Ok(history) = engine.image_history(&image.id).await else {
            continue;
        };

        let base = relations::base_of(image, &images, &layers);
        let base_history = match base {
            Some(base) => engine.image_history(&base.id).await.unwrap_or_default(),
            None => Vec::new(),
        };
        let base_label = base.map(lave_core::model::format::image_label);

        let result = dockerfile::reconstruct(&history, base_label.as_deref(), &base_history);

        println!(
            "\n== {} ==\n{}",
            lave_core::model::format::image_label(image),
            result.render()
        );

        assert!(
            result.caveats[0].contains("not the original"),
            "the disclaimer must lead every reconstruction"
        );
        assert!(
            !result.instructions.is_empty(),
            "an image with history should yield instructions"
        );

        // The base's own records must not be re-attributed to the derived image.
        if base.is_some() {
            assert!(
                result.instructions.len() <= history.len() + 1,
                "instructions should not exceed history plus the FROM line"
            );
        }

        reconstructed += 1;
    }

    assert!(
        reconstructed > 0,
        "no image on this host had readable history"
    );
}
