//! A workspace as a file: exporting it, and reading one back.
//!
//! The point is sharing — a set of requests checked into a repository beside
//! the robot they drive, or handed to a colleague. So the document holds no
//! database ids and no timestamps: a request names its connection by *name*,
//! because an id means nothing in anybody else's workspace.

use serde::{Deserialize, Serialize};

use crate::domain::{
    Collection, Connection, Dashboard, Request, RequestKind, SchemaRef, TransportConfig, Value,
};
use crate::{CoreError, CoreResult};

/// Bumped when the shape changes in a way an older reader cannot handle.
pub const VERSION: u32 = 1;

/// An exported workspace.
///
/// `Default` is the empty one at the current version, which is also what a file
/// missing a section deserialises to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// Refused rather than guessed at if it is from the future.
    pub version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<PortableConnection>,
    /// Collections, as paths. A parent id would be as meaningless here as a
    /// connection id, and a path is also what a person reads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collections: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<PortableRequest>,
    /// Dashboards, with their arrangement as the dock wrote it. A pane inside
    /// names its connection by id, which means nothing on another machine —
    /// the panes come back pointing at nothing until they are re-targeted, and
    /// that is better than losing the arrangement itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dashboards: Vec<PortableDashboard>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortableDashboard {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortableConnection {
    pub name: String,
    pub config: TransportConfig,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_connect: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortableRequest {
    pub name: String,
    pub kind: RequestKind,
    pub target: String,
    /// The connection's *name*. An id would be meaningless anywhere else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    /// The collection's path, `"Robot/Arm"`, or absent for the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaRef>,
    #[serde(default, skip_serializing_if = "is_empty_value")]
    pub input: Value,
}

fn is_empty_value(value: &Value) -> bool {
    matches!(value, Value::Null) || matches!(value, Value::Struct(fields) if fields.is_empty())
}

/// The `Robot/Arm` path of a collection, or `None` if it cannot be resolved.
///
/// Bounded rather than recursive-until-done: stored parents can form a cycle,
/// and a path is not worth hanging the app for.
fn path_of(collections: &[Collection], id: i64) -> Option<String> {
    const MAX_DEPTH: usize = 12;

    let mut parts = Vec::new();
    let mut current = Some(id);
    while let Some(at) = current {
        if parts.len() >= MAX_DEPTH {
            return None;
        }
        let collection = collections.iter().find(|entry| entry.id == at)?;
        parts.push(collection.name.clone());
        current = collection.parent_id;
    }
    parts.reverse();
    Some(parts.join("/"))
}

/// Builds a document from what a workspace currently holds.
pub fn export(
    connections: &[Connection],
    collections: &[Collection],
    requests: &[Request],
    dashboards: &[Dashboard],
) -> Document {
    let name_of = |id| {
        connections
            .iter()
            .find(|connection| connection.id == id)
            .map(|connection| connection.name.clone())
    };

    // Every collection, so an empty one survives the trip: somebody made it on
    // purpose, and it is where the next request is meant to go.
    let mut paths: Vec<String> = collections
        .iter()
        .filter_map(|collection| path_of(collections, collection.id))
        .collect();
    paths.sort();
    paths.dedup();

    Document {
        version: VERSION,
        collections: paths,
        connections: connections
            .iter()
            .map(|connection| PortableConnection {
                name: connection.name.clone(),
                config: connection.config.clone(),
                auto_connect: connection.auto_connect,
            })
            .collect(),
        requests: requests
            .iter()
            .map(|request| PortableRequest {
                name: request.name.clone(),
                kind: request.kind,
                target: request.target.clone(),
                connection: request.connection_id.and_then(name_of),
                collection: request
                    .collection_id
                    .and_then(|id| path_of(collections, id)),
                schema: request.schema.clone(),
                input: request.input.clone(),
            })
            .collect(),
        dashboards: dashboards
            .iter()
            .map(|dashboard| PortableDashboard {
                name: dashboard.name.clone(),
                layout: dashboard.layout.clone(),
            })
            .collect(),
    }
}

/// Renders a document as the JSON that goes in the file.
///
/// Pretty-printed on purpose: these end up in version control, and a one-line
/// file makes every change look like every other change.
impl Default for Document {
    fn default() -> Self {
        Self {
            version: VERSION,
            connections: Vec::new(),
            collections: Vec::new(),
            requests: Vec::new(),
            dashboards: Vec::new(),
        }
    }
}

pub fn to_json(document: &Document) -> CoreResult<String> {
    serde_json::to_string_pretty(document)
        .map(|json| json + "\n")
        .map_err(CoreError::Serde)
}

/// Reads a document, refusing one this build does not understand.
pub fn from_json(json: &str) -> CoreResult<Document> {
    let document: Document = serde_json::from_str(json).map_err(|error| {
        CoreError::InvalidArgument(format!("this is not a workspace file: {error}"))
    })?;

    if document.version > VERSION {
        return Err(CoreError::InvalidArgument(format!(
            "this file is version {} and this build understands up to {VERSION}",
            document.version
        )));
    }
    Ok(document)
}

/// What importing a document would do to a workspace.
///
/// Worked out before anything is written, so the caller can say what will
/// happen and so a half-applied import is not a state the workspace can reach.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    /// Connections in the document that the workspace does not have.
    pub new_connections: Vec<PortableConnection>,
    /// Connections whose name already exists here. Left alone: the local one
    /// may point at a different robot on the same name, and silently
    /// overwriting a URL somebody is using is worse than skipping it.
    pub existing_connections: Vec<String>,
    /// Requests to create, with the local connection id resolved.
    pub new_requests: Vec<(PortableRequest, Option<i64>)>,
    /// Collection paths the workspace does not have, parents before children so
    /// they can be created in order.
    pub new_collections: Vec<String>,
    /// Dashboards in the document the workspace does not already have by name.
    pub new_dashboards: Vec<PortableDashboard>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.new_connections.is_empty()
            && self.new_requests.is_empty()
            && self.new_collections.is_empty()
            && self.new_dashboards.is_empty()
    }

    /// A one-line summary, for a confirmation.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.new_connections.is_empty() {
            parts.push(plural(self.new_connections.len(), "connection"));
        }
        if !self.new_collections.is_empty() {
            parts.push(plural(self.new_collections.len(), "collection"));
        }
        if !self.new_requests.is_empty() {
            parts.push(plural(self.new_requests.len(), "request"));
        }
        if !self.new_dashboards.is_empty() {
            parts.push(plural(self.new_dashboards.len(), "dashboard"));
        }
        if parts.is_empty() {
            return "Nothing new to import.".to_string();
        }
        let mut summary = format!("Import {}", parts.join(" and "));
        if !self.existing_connections.is_empty() {
            summary.push_str(&format!(
                ", keeping {} already here",
                plural(self.existing_connections.len(), "connection")
            ));
        }
        summary.push('.');
        summary
    }
}

/// `"a/b/c"` becomes `["a", "a/b", "a/b/c"]`.
fn ancestors(path: &str) -> Vec<String> {
    let mut built = String::new();
    path.split('/')
        .filter(|part| !part.is_empty())
        .map(|part| {
            if !built.is_empty() {
                built.push('/');
            }
            built.push_str(part);
            built.clone()
        })
        .collect()
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Works out what importing `document` into this workspace would do.
///
/// Requests are always added rather than matched by name: two requests can
/// legitimately share a name, and an import that silently replaced one would
/// lose work that was never offered up.
pub fn plan(
    document: &Document,
    connections: &[Connection],
    collections: &[Collection],
    dashboards: &[Dashboard],
) -> Plan {
    let mut plan = Plan::default();

    // Every collection mentioned anywhere, including the ancestors of a path
    // whose own collection was not listed — a document naming `Robot/Arm` and
    // nothing else still needs `Robot` to exist.
    let mut wanted: Vec<String> = document
        .collections
        .iter()
        .chain(
            document
                .requests
                .iter()
                .filter_map(|r| r.collection.as_ref()),
        )
        .flat_map(|path| ancestors(path))
        .collect();
    // Sorted by depth so a parent is always created before its child.
    wanted.sort_by_key(|path| (path.matches('/').count(), path.clone()));
    wanted.dedup();

    let here: Vec<String> = collections
        .iter()
        .filter_map(|collection| path_of(collections, collection.id))
        .collect();
    plan.new_collections = wanted
        .into_iter()
        .filter(|path| !here.contains(path))
        .collect();

    for connection in &document.connections {
        if connections
            .iter()
            .any(|existing| existing.name == connection.name)
        {
            plan.existing_connections.push(connection.name.clone());
        } else {
            plan.new_connections.push(connection.clone());
        }
    }

    for request in &document.requests {
        // Resolved against what is already here; a connection the document
        // brings with it is resolved after it has been created, by the caller.
        let id = request.connection.as_ref().and_then(|name| {
            connections
                .iter()
                .find(|existing| existing.name == *name)
                .map(|existing| existing.id)
        });
        plan.new_requests.push((request.clone(), id));
    }

    // Matched by name, like connections: importing the same file twice should
    // not leave two copies of every dashboard.
    for dashboard in &document.dashboards {
        if !dashboards.iter().any(|here| here.name == dashboard.name) {
            plan.new_dashboards.push(dashboard.clone());
        }
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone as _, Utc};
    use std::collections::BTreeMap;

    fn connection(id: i64, name: &str) -> Connection {
        Connection {
            id,
            name: name.into(),
            config: TransportConfig::Rosbridge {
                url: "ws://robot:9090".into(),
            },
            auto_connect: false,
            color: Some("#ff0000".into()),
            created_at: Utc.timestamp_opt(0, 0).unwrap(),
            updated_at: Utc.timestamp_opt(0, 0).unwrap(),
        }
    }

    fn collection(id: i64, name: &str, parent: Option<i64>) -> Collection {
        Collection {
            id,
            parent_id: parent,
            name: name.into(),
            created_at: Utc.timestamp_opt(0, 0).unwrap(),
        }
    }

    fn request(id: i64, name: &str, connection_id: Option<i64>) -> Request {
        Request {
            id,
            collection_id: None,
            connection_id,
            name: name.into(),
            kind: RequestKind::Topic,
            target: "/chatter".into(),
            schema: None,
            input: Value::Null,
            visualization: None,
            created_at: Utc.timestamp_opt(0, 0).unwrap(),
            updated_at: Utc.timestamp_opt(0, 0).unwrap(),
        }
    }

    #[test]
    fn a_request_refers_to_its_connection_by_name() {
        // An id means nothing in anybody else's workspace.
        let document = export(
            &[connection(7, "Robot")],
            &[],
            &[request(1, "Chatter", Some(7))],
            &[],
        );
        assert_eq!(document.requests[0].connection.as_deref(), Some("Robot"));
    }

    #[test]
    fn a_request_pointing_at_nothing_exports_as_pointing_at_nothing() {
        let document = export(&[], &[], &[request(1, "Chatter", None)], &[]);
        assert_eq!(document.requests[0].connection, None);
    }

    #[test]
    fn a_dangling_connection_id_does_not_invent_a_name() {
        // The workspace should never be in this state, but a file claiming a
        // connection that is not in it would be worse than one claiming none.
        let document = export(
            &[connection(7, "Robot")],
            &[],
            &[request(1, "Chatter", Some(99))],
            &[],
        );
        assert_eq!(document.requests[0].connection, None);
    }

    #[test]
    fn importing_into_an_empty_workspace_brings_everything() {
        let document = export(
            &[connection(7, "Robot")],
            &[],
            &[request(1, "Chatter", Some(7))],
            &[],
        );
        let plan = plan(&document, &[], &[], &[]);

        assert_eq!(plan.new_connections.len(), 1);
        assert_eq!(plan.new_requests.len(), 1);
        assert!(plan.existing_connections.is_empty());
        // Nothing here to resolve against yet; the caller links it once the
        // connection has been created.
        assert_eq!(plan.new_requests[0].1, None);
    }

    #[test]
    fn a_connection_that_is_already_here_is_left_alone() {
        // The local one may point at a different robot under the same name, and
        // silently overwriting a URL somebody is using is worse than skipping.
        let document = export(&[connection(7, "Robot")], &[], &[], &[]);
        let plan = plan(&document, &[connection(1, "Robot")], &[], &[]);

        assert!(plan.new_connections.is_empty());
        assert_eq!(plan.existing_connections, ["Robot"]);
    }

    #[test]
    fn a_request_binds_to_the_local_connection_of_the_same_name() {
        let document = export(
            &[connection(7, "Robot")],
            &[],
            &[request(1, "Chatter", Some(7))],
            &[],
        );
        let plan = plan(&document, &[connection(42, "Robot")], &[], &[]);
        assert_eq!(plan.new_requests[0].1, Some(42));
    }

    #[test]
    fn requests_are_added_rather_than_matched_by_name() {
        // Two requests can legitimately share a name, and an import that
        // replaced one would lose work nobody offered up.
        let document = export(&[], &[], &[request(1, "Chatter", None)], &[]);
        let plan = plan(&document, &[], &[], &[]);
        assert_eq!(plan.new_requests.len(), 1);
    }

    #[test]
    fn the_summary_says_what_will_happen() {
        let document = export(
            &[connection(7, "Robot")],
            &[],
            &[request(1, "A", Some(7)), request(2, "B", Some(7))],
            &[],
        );
        assert_eq!(
            plan(&document, &[], &[], &[]).summary(),
            "Import 1 connection and 2 requests."
        );
        assert_eq!(
            plan(&document, &[connection(1, "Robot")], &[], &[]).summary(),
            "Import 2 requests, keeping 1 connection already here."
        );
        assert_eq!(
            plan(&Document::default(), &[], &[], &[]).summary(),
            "Nothing new to import."
        );
    }

    fn dashboard(id: i64, name: &str) -> Dashboard {
        Dashboard {
            id,
            name: name.into(),
            layout: Some(format!("{{\"panel_name\":\"TabPanel\",\"of\":{id}}}")),
            created_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        }
    }

    #[test]
    fn a_dashboard_travels_with_its_arrangement() {
        let document = export(&[], &[], &[], &[dashboard(1, "Arm")]);
        assert_eq!(document.dashboards.len(), 1);
        assert_eq!(document.dashboards[0].name, "Arm");
        assert!(document.dashboards[0].layout.is_some());

        let back = from_json(&to_json(&document).expect("written")).expect("read");
        assert_eq!(back.dashboards, document.dashboards);
    }

    #[test]
    fn importing_the_same_dashboard_twice_does_not_duplicate_it() {
        let document = export(&[], &[], &[], &[dashboard(1, "Arm")]);
        assert_eq!(plan(&document, &[], &[], &[]).new_dashboards.len(), 1);
        assert!(
            plan(&document, &[], &[], &[dashboard(9, "Arm")])
                .new_dashboards
                .is_empty(),
            "a dashboard of that name is already here"
        );
    }

    #[test]
    fn a_document_with_only_a_dashboard_is_still_worth_importing() {
        let document = export(&[], &[], &[], &[dashboard(1, "Arm")]);
        let plan = plan(&document, &[], &[], &[]);
        assert!(!plan.is_empty());
        assert_eq!(plan.summary(), "Import 1 dashboard.");
    }

    #[test]
    fn a_workspace_with_no_dashboards_writes_none_rather_than_an_empty_list() {
        let json = to_json(&export(&[], &[], &[], &[])).expect("written");
        assert!(
            !json.contains("dashboards"),
            "an absent section should not appear at all:\n{json}"
        );
    }

    #[test]
    fn a_collection_travels_as_a_path() {
        // A parent id is as meaningless in somebody else's workspace as a
        // connection id, and a path is also what a person reads.
        let collections = [
            collection(10, "Robot", None),
            collection(11, "Arm", Some(10)),
        ];
        let document = export(&[], &collections, &[request(1, "lift", None)], &[]);
        assert_eq!(document.collections, ["Robot", "Robot/Arm"]);

        let mut inside = request(2, "grip", None);
        inside.collection_id = Some(11);
        let document = export(&[], &collections, &[inside], &[]);
        assert_eq!(
            document.requests[0].collection.as_deref(),
            Some("Robot/Arm")
        );
    }

    #[test]
    fn an_empty_collection_survives_the_trip() {
        // Somebody made it on purpose, and it is where the next request goes.
        let document = export(&[], &[collection(10, "Arm", None)], &[], &[]);
        assert_eq!(document.collections, ["Arm"]);
        assert_eq!(plan(&document, &[], &[], &[]).new_collections, ["Arm"]);
    }

    #[test]
    fn importing_creates_parents_before_children() {
        let document = Document {
            collections: vec!["Robot/Arm/Wrist".into()],
            ..Document::default()
        };
        // Only the leaf is listed, so the ancestors have to be inferred — and
        // created in an order where each one's parent already exists.
        assert_eq!(
            plan(&document, &[], &[], &[]).new_collections,
            ["Robot", "Robot/Arm", "Robot/Arm/Wrist"]
        );
    }

    #[test]
    fn a_collection_already_here_is_not_created_again() {
        let here = [collection(10, "Robot", None)];
        let document = Document {
            collections: vec!["Robot".into(), "Robot/Arm".into()],
            ..Document::default()
        };
        assert_eq!(
            plan(&document, &[], &here, &[]).new_collections,
            ["Robot/Arm"]
        );
    }

    #[test]
    fn a_cycle_in_stored_collections_does_not_hang_the_export() {
        let collections = [collection(10, "A", Some(11)), collection(11, "B", Some(10))];
        // Unresolvable, so left out rather than looped over forever.
        assert!(export(&[], &collections, &[], &[]).collections.is_empty());
    }

    #[test]
    fn the_summary_counts_collections_too() {
        let document = Document {
            collections: vec!["Arm".into()],
            requests: vec![PortableRequest {
                name: "lift".into(),
                kind: RequestKind::Topic,
                target: "/lift".into(),
                connection: None,
                collection: Some("Arm".into()),
                schema: None,
                input: Value::Null,
            }],
            ..Document::default()
        };
        assert_eq!(
            plan(&document, &[], &[], &[]).summary(),
            "Import 1 collection and 1 request."
        );
    }

    #[test]
    fn a_document_round_trips_through_json() {
        let document = export(
            &[connection(7, "Robot")],
            &[],
            &[request(1, "Chatter", Some(7)), request(2, "Other", None)],
            &[],
        );
        let json = to_json(&document).expect("written");
        assert_eq!(from_json(&json).expect("read"), document);
    }

    #[test]
    fn the_file_is_pretty_printed_and_ends_in_a_newline() {
        // These land in version control, where a one-line file makes every
        // change look like every other change.
        let json = to_json(&export(&[connection(7, "Robot")], &[], &[], &[])).expect("written");
        assert!(json.contains('\n'));
        assert!(json.ends_with('\n'));
    }

    #[test]
    fn a_newer_version_is_refused_rather_than_half_read() {
        let json = r#"{"version": 99, "requests": []}"#;
        let error = from_json(json).expect_err("refused");
        assert!(error.to_string().contains("99"), "{error}");
    }

    #[test]
    fn an_older_version_is_accepted() {
        let json = r#"{"version": 0, "requests": []}"#;
        assert_eq!(from_json(json).expect("read").version, 0);
    }

    #[test]
    fn nonsense_is_rejected_with_something_readable() {
        let error = from_json("not json at all").expect_err("refused");
        assert!(
            error.to_string().contains("not a workspace file"),
            "{error}"
        );
    }

    #[test]
    fn an_empty_payload_is_left_out_of_the_file() {
        // A file full of `"input": null` is noise in a diff.
        let mut only = request(1, "Chatter", None);
        only.input = Value::Struct(BTreeMap::new());
        let json = to_json(&export(&[], &[], &[only], &[])).expect("written");
        assert!(!json.contains("input"), "{json}");
    }

    #[test]
    fn a_payload_that_was_filled_in_survives() {
        let mut filled = request(1, "Add", None);
        filled.input = Value::Struct(BTreeMap::from([("a".into(), Value::Int(2))]));
        let document = export(&[], &[], &[filled], &[]);
        let read = from_json(&to_json(&document).expect("written")).expect("read");
        assert_eq!(read.requests[0].input, document.requests[0].input);
    }
}
