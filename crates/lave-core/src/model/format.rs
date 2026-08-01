//! Turning daemon values into text a person reads.
//!
//! Every function here is total and takes its notion of "now" as an argument, so the
//! rendering of any given value is deterministic and testable.

use chrono::{DateTime, FixedOffset, TimeZone, Utc};

use crate::engine::{ContainerSummary, ImageSummary, PortMapping};

/// What Docker shows for an image with no tags.
pub const UNTAGGED: &str = "<none>:<none>";

/// Decimal units, matching what the Docker CLI reports.
#[must_use]
pub fn bytes(value: i64) -> String {
    if value < 0 {
        return "unknown".to_owned();
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "sizes are displayed to one decimal place"
    )]
    let mut size = value as f64;
    let units = ["B", "kB", "MB", "GB", "TB", "PB"];

    let mut unit = 0;
    while size >= 1000.0 && unit < units.len() - 1 {
        size /= 1000.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{size:.1} {}", units[unit])
    }
}

/// Coarse relative time, as in "3 days ago".
#[must_use]
pub fn age(instant: i64, now: i64) -> String {
    if instant <= 0 {
        return "unknown".to_owned();
    }

    let seconds = now - instant;
    if seconds < 0 {
        return "in the future".to_owned();
    }

    let (count, unit) = match seconds {
        0..=44 => return "just now".to_owned(),
        45..=5399 => (seconds / 60, "minute"),
        5400..=129_599 => (seconds / 3600, "hour"),
        129_600..=1_209_599 => (seconds / 86400, "day"),
        1_209_600..=5_183_999 => (seconds / 604_800, "week"),
        5_184_000..=62_207_999 => (seconds / 2_592_000, "month"),
        _ => (seconds / 31_536_000, "year"),
    };

    let count = count.max(1);
    if count == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{count} {unit}s ago")
    }
}

/// Wall-clock time in the supplied zone. The offset is injected so tests do not
/// depend on the machine's timezone.
#[must_use]
pub fn timestamp(instant: i64, offset: FixedOffset) -> String {
    let Some(utc) = DateTime::<Utc>::from_timestamp(instant, 0) else {
        return "unknown".to_owned();
    };
    offset
        .from_utc_datetime(&utc.naive_utc())
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// Both renderings together, which is what the detail pane shows.
#[must_use]
pub fn instant(value: i64, now: i64, offset: FixedOffset) -> String {
    if value <= 0 {
        return "unknown".to_owned();
    }
    format!("{} ({})", timestamp(value, offset), age(value, now))
}

/// The 12 hex characters Docker itself displays, without the digest algorithm.
#[must_use]
pub fn short_id(id: &str) -> String {
    let bare = id.split_once(':').map_or(id, |(_, rest)| rest);
    bare.chars().take(12).collect()
}

/// A published port, as in `0.0.0.0:8080 -> 80/tcp`.
#[must_use]
pub fn port(mapping: &PortMapping) -> String {
    let private = format!("{}/{}", mapping.private_port, mapping.protocol);
    match (mapping.public_port, mapping.ip.as_deref()) {
        (Some(public), Some(ip)) => format!("{ip}:{public} \u{2192} {private}"),
        (Some(public), None) => format!("{public} \u{2192} {private}"),
        (None, _) => private,
    }
}

/// The tag an image is filed under: the alphabetically first real one, so a
/// multi-tagged image sorts and titles predictably rather than by the daemon's order.
#[must_use]
pub fn primary_tag(image: &ImageSummary) -> Option<String> {
    image
        .repo_tags
        .iter()
        .filter(|tag| !tag.is_empty() && tag.as_str() != UNTAGGED)
        .min_by_key(|tag| tag.to_lowercase())
        .cloned()
}

/// How an image is named in the tree: its tag, or its short ID when it has none.
#[must_use]
pub fn image_label(image: &ImageSummary) -> String {
    primary_tag(image).unwrap_or_else(|| short_id(&image.id))
}

/// Whether an image has no usable tag. Such images are usually the residue of a tag
/// being moved by a later pull or build.
#[must_use]
pub fn is_untagged(image: &ImageSummary) -> bool {
    primary_tag(image).is_none()
}

/// How a container is named in the tree. Docker allows a container to have no name.
#[must_use]
pub fn container_label(container: &ContainerSummary) -> String {
    container
        .names
        .iter()
        .find(|name| !name.is_empty())
        .cloned()
        .unwrap_or_else(|| short_id(&container.id))
}

/// Joins a list for display, or says so when it is empty.
#[must_use]
pub fn list_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

/// Non-empty text, or a placeholder. Daemons omit fields freely.
#[must_use]
pub fn text_or_unknown(value: &str) -> String {
    if value.trim().is_empty() {
        "unknown".to_owned()
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::engine::ContainerState;

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).expect("UTC is a valid offset")
    }

    #[test]
    fn byte_sizes_step_through_decimal_units() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1000), "1.0 kB");
        assert_eq!(bytes(164_231_172), "164.2 MB");
        assert_eq!(bytes(1_500_000_000), "1.5 GB");
        assert_eq!(bytes(64_960_110_592), "65.0 GB");
    }

    #[test]
    fn a_negative_size_is_reported_as_unknown_rather_than_rendered() {
        assert_eq!(bytes(-1), "unknown");
    }

    #[test]
    fn ages_are_coarse_and_singular_where_appropriate() {
        let now = 1_000_000_000;
        assert_eq!(age(now, now), "just now");
        assert_eq!(age(now - 44, now), "just now");
        assert_eq!(age(now - 60, now), "1 minute ago");
        assert_eq!(age(now - 600, now), "10 minutes ago");
        assert_eq!(age(now - 7200, now), "2 hours ago");
        assert_eq!(age(now - 172_800, now), "2 days ago");
        assert_eq!(age(now - 1_209_600, now), "2 weeks ago");
        assert_eq!(age(now - 7_776_000, now), "3 months ago");
        assert_eq!(age(now - 63_072_000, now), "2 years ago");
    }

    #[test]
    fn boundaries_never_report_zero_units() {
        let now = 1_000_000_000;
        for seconds in [45, 5400, 129_600, 1_209_600, 5_184_000, 62_208_000] {
            let rendered = age(now - seconds, now);
            assert!(
                !rendered.starts_with('0'),
                "{seconds}s rendered as {rendered}"
            );
        }
    }

    #[test]
    fn clock_skew_is_stated_rather_than_rendered_as_a_huge_age() {
        assert_eq!(age(2_000, 1_000), "in the future");
    }

    #[test]
    fn a_missing_creation_time_is_unknown() {
        assert_eq!(age(0, 1_000_000_000), "unknown");
        assert_eq!(instant(0, 1_000_000_000, utc()), "unknown");
    }

    #[test]
    fn timestamps_render_in_the_supplied_zone() {
        let instant_secs = 1_782_058_645;

        assert_eq!(timestamp(instant_secs, utc()), "2026-06-21 16:17:25");

        let east = FixedOffset::east_opt(2 * 3600).expect("valid offset");
        assert_eq!(timestamp(instant_secs, east), "2026-06-21 18:17:25");
    }

    #[test]
    fn the_detail_pane_shows_wall_clock_and_age_together() {
        let created = 1_782_058_645;
        let rendered = instant(created, created + 172_800, utc());

        assert_eq!(rendered, "2026-06-21 16:17:25 (2 days ago)");
    }

    #[test]
    fn short_ids_drop_the_digest_algorithm() {
        assert_eq!(
            short_id("sha256:dff9997d956e5b7117ff96819a213cc4f80754c8"),
            "dff9997d956e"
        );
        assert_eq!(short_id("13ef39df585fa5ea8df9325dffdc7c18"), "13ef39df585f");
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id(""), "");
    }

    #[test]
    fn ports_render_published_and_unpublished_forms() {
        let published = PortMapping {
            ip: Some("0.0.0.0".to_owned()),
            private_port: 80,
            public_port: Some(8080),
            protocol: "tcp".to_owned(),
        };
        assert_eq!(port(&published), "0.0.0.0:8080 \u{2192} 80/tcp");

        let internal = PortMapping {
            ip: None,
            private_port: 5432,
            public_port: None,
            protocol: "tcp".to_owned(),
        };
        assert_eq!(port(&internal), "5432/tcp");

        let no_ip = PortMapping {
            ip: None,
            private_port: 53,
            public_port: Some(5353),
            protocol: "udp".to_owned(),
        };
        assert_eq!(port(&no_ip), "5353 \u{2192} 53/udp");
    }

    #[test]
    fn an_image_is_labelled_by_its_alphabetically_first_tag() {
        // The daemon's ordering is not dependable, so the choice must not depend on it.
        let tagged = ImageSummary {
            repo_tags: vec!["nginx:latest".to_owned(), "nginx:1.27".to_owned()],
            ..ImageSummary::default()
        };
        assert_eq!(image_label(&tagged), "nginx:1.27");
        assert_eq!(primary_tag(&tagged).as_deref(), Some("nginx:1.27"));
    }

    #[test]
    fn tag_choice_ignores_case_so_capitals_do_not_sort_first() {
        let tagged = ImageSummary {
            repo_tags: vec!["Zebra:latest".to_owned(), "alpine:3.20".to_owned()],
            ..ImageSummary::default()
        };
        assert_eq!(image_label(&tagged), "alpine:3.20");
    }

    #[test]
    fn an_untagged_image_is_labelled_by_its_short_id() {
        let untagged = ImageSummary {
            id: "sha256:abcdef1234567890".to_owned(),
            repo_tags: vec![],
            ..ImageSummary::default()
        };
        assert_eq!(image_label(&untagged), "abcdef123456");
        assert!(is_untagged(&untagged));

        // Docker's own placeholder is not a tag.
        let placeholder = ImageSummary {
            id: "sha256:abcdef1234567890".to_owned(),
            repo_tags: vec![UNTAGGED.to_owned()],
            ..ImageSummary::default()
        };
        assert_eq!(image_label(&placeholder), "abcdef123456");
        assert!(is_untagged(&placeholder));
    }

    #[test]
    fn a_tagged_image_is_not_reported_as_untagged() {
        let tagged = ImageSummary {
            repo_tags: vec!["nginx:1.27".to_owned()],
            ..ImageSummary::default()
        };
        assert!(!is_untagged(&tagged));
    }

    #[test]
    fn a_container_without_a_name_falls_back_to_its_short_id() {
        let unnamed = ContainerSummary {
            id: "13ef39df585fa5ea8df9325dffdc7c18".to_owned(),
            names: vec![],
            state: ContainerState::Exited,
            ..ContainerSummary::default()
        };
        assert_eq!(container_label(&unnamed), "13ef39df585f");

        let named = ContainerSummary {
            names: vec!["web".to_owned()],
            ..ContainerSummary::default()
        };
        assert_eq!(container_label(&named), "web");
    }

    #[test]
    fn empty_lists_and_blank_text_say_so() {
        assert_eq!(list_or_none(&[]), "none");
        assert_eq!(list_or_none(&["a".to_owned(), "b".to_owned()]), "a, b");
        assert_eq!(text_or_unknown("   "), "unknown");
        assert_eq!(text_or_unknown("overlayfs"), "overlayfs");
    }
}
