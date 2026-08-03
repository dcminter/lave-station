//! The Images and Containers overview pages, as tables.
//!
//! A vertical stack of rows wastes the width these pages have. Each cell carries both
//! its rendering and a sort key, so a column of sizes sorts by bytes and a column of
//! ages sorts by timestamp rather than by the text a person reads.

use crate::engine::{ContainerSummary, ImageSummary};

use super::format::{
    age, bytes, container_label, image_label, list_or_none, port, short_id, text_or_unknown,
};
use super::relations;
use super::tree::{NodeId, Tone, sorted_containers, sorted_images, state_icon, state_tone};

/// What a cell sorts by, which is rarely the text it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sort {
    Text(String),
    Number(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub sort: Sort,
}

impl Cell {
    fn text(value: impl Into<String>) -> Self {
        let text = value.into();
        // Case-insensitive, so a capitalised tag does not sort above every lowercase one.
        let sort = Sort::Text(text.to_lowercase());
        Self { text, sort }
    }

    fn number(text: impl Into<String>, value: i64) -> Self {
        Self {
            text: text.into(),
            sort: Sort::Number(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub title: String,
    /// Right-aligned, as numbers should be.
    pub numeric: bool,
    /// Takes the slack when the window is wider than the columns need.
    pub expand: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// What selecting this row should show.
    pub key: NodeId,
    /// Icon and tone for the first cell, matching the sidebar.
    pub icon: &'static str,
    pub tone: Tone,
    pub cells: Vec<Cell>,
}

/// How a table opens, before the user sorts it themselves.
///
/// A user's own sort lasts for the session and no longer: it is not written to the
/// settings store, so every launch starts from this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortSpec {
    pub column: &'static str,
    pub descending: bool,
}

/// Identifies a table across renders, so column widths can be stored against it.
/// Stable strings, not positions: a table that moves keeps its widths.
pub const IMAGES: &str = "images";
pub const CONTAINERS: &str = "containers";
pub const PROCESS_LIST: &str = "process-list";

/// Newest first, as `docker ps` itself lists them.
const NEWEST_FIRST: SortSpec = SortSpec {
    column: "Created",
    descending: true,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// Stable identity, for storing column widths against.
    pub id: &'static str,
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
    /// The order it opens in. A column that is not in this table is ignored, leaving the
    /// rows in the order they were built.
    pub default_sort: Option<SortSpec>,
}

impl Table {
    /// A row's cell by column title. Convenience for callers and tests.
    #[must_use]
    pub fn cell(&self, row: usize, column: &str) -> Option<&str> {
        let index = self
            .columns
            .iter()
            .position(|candidate| candidate.title == column)?;
        Some(self.rows.get(row)?.cells.get(index)?.text.as_str())
    }

    #[must_use]
    pub fn column_titles(&self) -> Vec<&str> {
        self.columns
            .iter()
            .map(|column| column.title.as_str())
            .collect()
    }
}

fn column(title: &str) -> Column {
    Column {
        title: title.to_owned(),
        numeric: false,
        expand: false,
    }
}

fn numeric_column(title: &str) -> Column {
    Column {
        numeric: true,
        ..column(title)
    }
}

fn wide_column(title: &str) -> Column {
    Column {
        expand: true,
        ..column(title)
    }
}

/// Every image on the device, in the same order the sidebar lists them.
#[must_use]
pub fn images(images: &[ImageSummary], containers: &[ContainerSummary], now: i64) -> Table {
    let rows = sorted_images(images)
        .into_iter()
        .map(|image| Row {
            key: NodeId::Image(image.id.clone()),
            icon: "media-optical-symbolic",
            tone: if super::format::is_untagged(image) {
                Tone::Warn
            } else {
                Tone::Neutral
            },
            cells: vec![
                Cell::text(image_label(image)),
                Cell::text(short_id(&image.id)),
                Cell::number(bytes(image.size), image.size),
                Cell::number(age(image.created, now), image.created),
                {
                    // Counted from the container listing: the daemon reports -1 in the
                    // image listing unless it was asked to compute it.
                    let count = relations::containers_of(image, containers).len();
                    Cell::number(count.to_string(), i64::try_from(count).unwrap_or(i64::MAX))
                },
            ],
        })
        .collect();

    Table {
        id: IMAGES,
        columns: vec![
            wide_column("Image"),
            column("ID"),
            numeric_column("Size"),
            numeric_column("Created"),
            numeric_column("Containers"),
        ],
        rows,
        default_sort: Some(NEWEST_FIRST),
    }
}

/// Fewest rows the environment table is given, so a quiet machine still looks like a
/// table rather than a stripe.
pub const MIN_VISIBLE_ROWS: usize = 3;
/// Most rows it will claim unasked. Beyond this the user drags for more, rather than
/// having the daemon metadata pushed off the screen by a busy host.
pub const MAX_VISIBLE_ROWS: usize = 20;

/// How many rows the environment table should show without being dragged.
///
/// Sized to the *running* containers: those are what the panel is for, and the stopped
/// ones are revealed by dragging the divider below it.
#[must_use]
pub fn visible_rows(running: usize) -> usize {
    running.clamp(MIN_VISIBLE_ROWS, MAX_VISIBLE_ROWS)
}

/// The container table shown on the environment page, with the columns `docker ps`
/// itself reports.
///
/// `include_stopped` corresponds to `docker ps -a`: without it, only containers that
/// are actually executing are listed, which is what plain `docker ps` shows.
#[must_use]
pub fn process_list(containers: &[ContainerSummary], now: i64, include_stopped: bool) -> Table {
    let rows = sorted_containers(containers)
        .into_iter()
        .filter(|container| include_stopped || container.state.is_active())
        .map(|container| {
            let ports: Vec<String> = container.ports.iter().map(port).collect();
            Row {
                key: NodeId::Container(container.id.clone()),
                icon: state_icon(&container.state),
                tone: state_tone(&container.state),
                cells: vec![
                    Cell::text(short_id(&container.id)),
                    Cell::text(if container.image.is_empty() {
                        short_id(&container.image_id)
                    } else {
                        container.image.clone()
                    }),
                    Cell::text(text_or_unknown(&container.command)),
                    Cell::number(age(container.created, now), container.created),
                    Cell::text(text_or_unknown(&container.status)),
                    Cell::text(list_or_none(&ports)),
                    Cell::text(container_label(container)),
                ],
            }
        })
        .collect();

    Table {
        id: PROCESS_LIST,
        columns: vec![
            column("Container ID"),
            wide_column("Image"),
            wide_column("Command"),
            numeric_column("Created"),
            column("Status"),
            column("Ports"),
            wide_column("Names"),
        ],
        rows,
        default_sort: Some(NEWEST_FIRST),
    }
}

/// Every container on the device, running or not.
#[must_use]
pub fn containers(containers: &[ContainerSummary], now: i64) -> Table {
    let rows = sorted_containers(containers)
        .into_iter()
        .map(|container| {
            let ports: Vec<String> = container.ports.iter().map(port).collect();
            Row {
                key: NodeId::Container(container.id.clone()),
                icon: state_icon(&container.state),
                tone: state_tone(&container.state),
                cells: vec![
                    Cell::text(container_label(container)),
                    Cell::text(container.state.label()),
                    Cell::text(if container.image.is_empty() {
                        short_id(&container.image_id)
                    } else {
                        container.image.clone()
                    }),
                    Cell::text(list_or_none(&ports)),
                    Cell::number(age(container.created, now), container.created),
                ],
            }
        })
        .collect();

    Table {
        id: CONTAINERS,
        columns: vec![
            wide_column("Container"),
            column("State"),
            wide_column("Image"),
            column("Ports"),
            numeric_column("Created"),
        ],
        rows,
        default_sort: Some(NEWEST_FIRST),
    }
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::engine::{ContainerState, PortMapping};

    const NOW: i64 = 1_782_231_445;

    fn image(id: &str, tag: &str, size: i64, created: i64) -> ImageSummary {
        ImageSummary {
            id: format!("sha256:{id}"),
            repo_tags: if tag.is_empty() {
                vec![]
            } else {
                vec![tag.to_owned()]
            },
            size,
            created,
            ..ImageSummary::default()
        }
    }

    fn container(name: &str, image_id: &str, state: ContainerState) -> ContainerSummary {
        ContainerSummary {
            id: format!("id-{name}"),
            names: vec![name.to_owned()],
            image: "nginx:1.27".to_owned(),
            image_id: format!("sha256:{image_id}"),
            created: NOW - 7200,
            state,
            ..ContainerSummary::default()
        }
    }

    #[test]
    fn the_images_table_has_the_columns_the_width_is_for() {
        let table = images(&[], &[], NOW);

        assert_eq!(
            table.column_titles(),
            vec!["Image", "ID", "Size", "Created", "Containers"]
        );
    }

    #[test]
    fn image_rows_render_the_values_a_person_reads() {
        let table = images(
            &[image("aaa", "nginx:1.27", 164_231_172, NOW - 172_800)],
            &[],
            NOW,
        );

        assert_eq!(table.cell(0, "Image"), Some("nginx:1.27"));
        assert_eq!(table.cell(0, "ID"), Some("aaa"));
        assert_eq!(table.cell(0, "Size"), Some("164.2 MB"));
        assert_eq!(table.cell(0, "Created"), Some("2 days ago"));
    }

    #[test]
    fn sizes_and_dates_sort_by_value_not_by_their_rendering() {
        // "9 B" reads after "10.0 kB" alphabetically but must sort before it.
        let table = images(
            &[
                image("big", "b:1", 10_000, 100),
                image("small", "a:1", 9, 200),
            ],
            &[],
            NOW,
        );

        let size = |row: usize| table.rows[row].cells[2].sort.clone();
        assert_eq!(size(0), Sort::Number(9), "a:1 sorts first by tag");
        assert_eq!(size(1), Sort::Number(10_000));

        let created = |row: usize| table.rows[row].cells[3].sort.clone();
        assert_eq!(created(0), Sort::Number(200));
        assert_eq!(created(1), Sort::Number(100));
    }

    #[test]
    fn the_container_count_comes_from_the_listing_not_the_daemons_placeholder() {
        // Docker reports Containers: -1 in the image listing unless asked to compute it.
        let mut uncounted = image("aaa", "nginx:1.27", 100, 100);
        uncounted.containers = -1;

        let table = images(
            &[uncounted],
            &[
                container("web", "aaa", ContainerState::Running),
                container("api", "aaa", ContainerState::Exited),
                container("db", "other", ContainerState::Running),
            ],
            NOW,
        );

        assert_eq!(table.cell(0, "Containers"), Some("2"));
    }

    #[test]
    fn the_table_lists_images_in_the_same_order_as_the_sidebar() {
        let table = images(
            &[
                image("u1", "", 1, 1_000),
                image("t2", "zebra:1", 1, 0),
                image("u2", "", 1, 3_000),
                image("t1", "alpine:1", 1, 0),
            ],
            &[],
            NOW,
        );

        let first_column: Vec<&str> = (0..table.rows.len())
            .filter_map(|row| table.cell(row, "Image"))
            .collect();

        assert_eq!(first_column, vec!["alpine:1", "zebra:1", "u2", "u1"]);
    }

    #[test]
    fn every_image_row_points_at_the_node_that_selects_it() {
        let table = images(&[image("aaa", "nginx:1.27", 1, 1)], &[], NOW);

        assert_eq!(table.rows[0].key, NodeId::Image("sha256:aaa".to_owned()));
    }

    #[test]
    fn an_untagged_image_row_is_toned_like_its_sidebar_entry() {
        let table = images(
            &[image("aaa", "", 1, 1), image("bbb", "nginx:1.27", 1, 1)],
            &[],
            NOW,
        );

        assert_eq!(table.rows[0].tone, Tone::Neutral, "nginx sorts first");
        assert_eq!(table.rows[1].tone, Tone::Warn);
    }

    #[test]
    fn the_containers_table_shows_state_image_and_ports() {
        let mut published = container("web", "aaa", ContainerState::Running);
        published.ports = vec![PortMapping {
            ip: Some("0.0.0.0".to_owned()),
            private_port: 80,
            public_port: Some(8080),
            protocol: "tcp".to_owned(),
        }];

        let table = containers(&[published], NOW);

        assert_eq!(
            table.column_titles(),
            vec!["Container", "State", "Image", "Ports", "Created"]
        );
        assert_eq!(table.cell(0, "Container"), Some("web"));
        assert_eq!(table.cell(0, "State"), Some("running"));
        assert_eq!(table.cell(0, "Image"), Some("nginx:1.27"));
        assert_eq!(table.cell(0, "Ports"), Some("0.0.0.0:8080 \u{2192} 80/tcp"));
        assert_eq!(table.cell(0, "Created"), Some("2 hours ago"));
    }

    #[test]
    fn a_container_publishing_nothing_says_none_rather_than_leaving_a_gap() {
        let table = containers(&[container("web", "aaa", ContainerState::Exited)], NOW);

        assert_eq!(table.cell(0, "Ports"), Some("none"));
    }

    #[test]
    fn a_container_created_from_a_bare_id_shows_the_short_id() {
        let mut anonymous = container("web", "aaa", ContainerState::Running);
        anonymous.image = String::new();

        let table = containers(&[anonymous], NOW);

        assert_eq!(table.cell(0, "Image"), Some("aaa"));
    }

    #[test]
    fn container_rows_carry_the_state_tone_so_the_table_matches_the_tree() {
        let table = containers(
            &[
                container("alpha", "aaa", ContainerState::Running),
                container("beta", "aaa", ContainerState::Exited),
            ],
            NOW,
        );

        assert_eq!(table.rows[0].tone, Tone::Good);
        assert_eq!(table.rows[1].tone, Tone::Bad);
    }

    #[test]
    fn the_process_list_has_exactly_the_columns_docker_ps_reports() {
        let table = process_list(&[], NOW, true);

        assert_eq!(
            table.column_titles(),
            vec![
                "Container ID",
                "Image",
                "Command",
                "Created",
                "Status",
                "Ports",
                "Names"
            ]
        );
    }

    #[test]
    fn the_process_list_renders_a_container_the_way_docker_ps_would() {
        let mut running = container("web", "aaa", ContainerState::Running);
        running.id = "13ef39df585fa5ea8df9325dffdc7c18".to_owned();
        running.command = "nginx -g daemon off;".to_owned();
        running.status = "Up 2 hours".to_owned();
        running.ports = vec![PortMapping {
            ip: Some("0.0.0.0".to_owned()),
            private_port: 80,
            public_port: Some(8080),
            protocol: "tcp".to_owned(),
        }];

        let table = process_list(&[running], NOW, true);

        assert_eq!(table.cell(0, "Container ID"), Some("13ef39df585f"));
        assert_eq!(table.cell(0, "Image"), Some("nginx:1.27"));
        assert_eq!(table.cell(0, "Command"), Some("nginx -g daemon off;"));
        assert_eq!(table.cell(0, "Created"), Some("2 hours ago"));
        assert_eq!(table.cell(0, "Status"), Some("Up 2 hours"));
        assert_eq!(table.cell(0, "Ports"), Some("0.0.0.0:8080 \u{2192} 80/tcp"));
        assert_eq!(table.cell(0, "Names"), Some("web"));
    }

    #[test]
    fn the_process_list_hides_stopped_containers_when_asked_to() {
        let containers = [
            container("runner", "aaa", ContainerState::Running),
            container("sleeper", "aaa", ContainerState::Exited),
            container("held", "aaa", ContainerState::Paused),
            container("born", "aaa", ContainerState::Created),
            container("broken", "aaa", ContainerState::Dead),
        ];

        let running_only = process_list(&containers, NOW, false);
        let names: Vec<&str> = (0..running_only.rows.len())
            .filter_map(|row| running_only.cell(row, "Names"))
            .collect();

        // Paused counts as executing, as it does for docker ps; created does not.
        assert_eq!(names, vec!["held", "runner"]);
        assert_eq!(process_list(&containers, NOW, true).rows.len(), 5);
    }

    #[test]
    fn the_process_list_sorts_creation_dates_by_value_so_newest_first_is_possible() {
        let mut old = container("old", "aaa", ContainerState::Running);
        old.created = 1_000;
        let mut new = container("new", "aaa", ContainerState::Running);
        new.created = 9_000;

        let table = process_list(&[old, new], NOW, true);
        let created = |row: usize| table.rows[row].cells[3].sort.clone();

        // Rows arrive in name order; the view sorts them, so only the keys matter here.
        assert_eq!(table.cell(0, "Names"), Some("new"));
        assert_eq!(created(0), Sort::Number(9_000));
        assert_eq!(created(1), Sort::Number(1_000));
    }

    #[test]
    fn every_process_list_row_carries_a_state_icon_and_navigates_to_its_container() {
        let table = process_list(
            &[
                container("runner", "aaa", ContainerState::Running),
                container("sleeper", "aaa", ContainerState::Exited),
            ],
            NOW,
            true,
        );

        assert_eq!(table.rows[0].tone, Tone::Good, "runner sorts first");
        assert_eq!(table.rows[1].tone, Tone::Bad);
        assert_ne!(table.rows[0].icon, table.rows[1].icon);
        assert_eq!(table.rows[0].key, NodeId::Container("id-runner".to_owned()));
    }

    #[test]
    fn a_container_with_nothing_to_report_still_fills_every_cell() {
        let mut bare = container("bare", "aaa", ContainerState::Exited);
        bare.command = String::new();
        bare.status = String::new();
        bare.ports.clear();

        let table = process_list(&[bare], NOW, true);

        for (index, cell) in table.rows[0].cells.iter().enumerate() {
            assert!(
                !cell.text.trim().is_empty(),
                "column {} rendered blank",
                table.columns[index].title
            );
        }
    }

    #[test]
    fn the_table_grows_to_fit_the_running_containers() {
        assert_eq!(visible_rows(5), 5);
        assert_eq!(visible_rows(19), 19);
        assert_eq!(visible_rows(MAX_VISIBLE_ROWS), MAX_VISIBLE_ROWS);
    }

    #[test]
    fn a_busy_host_stops_at_the_cap_and_leaves_the_rest_to_dragging() {
        assert_eq!(visible_rows(21), MAX_VISIBLE_ROWS);
        assert_eq!(visible_rows(500), MAX_VISIBLE_ROWS);
    }

    #[test]
    fn a_quiet_host_still_gets_a_table_shaped_table() {
        assert_eq!(visible_rows(0), MIN_VISIBLE_ROWS);
        assert_eq!(visible_rows(1), MIN_VISIBLE_ROWS);
    }

    #[test]
    fn every_table_names_itself_so_its_column_widths_can_be_stored() {
        let ids = [
            images(&[], &[], NOW).id,
            containers(&[], NOW).id,
            process_list(&[], NOW, true).id,
        ];

        assert_eq!(ids, [IMAGES, CONTAINERS, PROCESS_LIST]);
        assert_eq!(
            ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
            ids.len(),
            "two tables sharing an id would share each other's column widths"
        );
    }

    #[test]
    fn every_table_opens_newest_first_on_a_column_it_actually_has() {
        for table in [
            images(&[], &[], NOW),
            containers(&[], NOW),
            process_list(&[], NOW, true),
        ] {
            let sort = table.default_sort.expect("every table opens sorted");

            assert!(sort.descending, "newest first means descending");
            assert!(
                table.column_titles().contains(&sort.column),
                "{} sorts by a column it does not have",
                table.id
            );
        }
    }

    #[test]
    fn empty_listings_produce_a_table_with_headings_and_no_rows() {
        assert!(images(&[], &[], NOW).rows.is_empty());
        assert!(containers(&[], NOW).rows.is_empty());
        assert_eq!(containers(&[], NOW).columns.len(), 5);
    }
}
