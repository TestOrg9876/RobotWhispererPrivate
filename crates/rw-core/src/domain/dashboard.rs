use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::DashboardId;

/// A named arrangement of live views.
///
/// A request is one target and its response; a dashboard is however many views
/// the user has put side by side, of whatever topics they like, arranged how
/// they like. The arrangement itself is opaque here — it is whatever the dock
/// serialised — because the shape of a layout is the UI's business and storing
/// it structurally would tie the schema to a component's internals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dashboard {
    pub id: DashboardId,
    pub name: String,
    /// `None` until the user has arranged something.
    pub layout: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn a_dashboard_round_trips_through_json() {
        let dashboard = Dashboard {
            id: 3,
            name: "Arm".into(),
            layout: Some("{\"panel_name\":\"TabPanel\"}".into()),
            created_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap(),
        };
        let json = serde_json::to_string(&dashboard).unwrap();
        assert_eq!(serde_json::from_str::<Dashboard>(&json).unwrap(), dashboard);
    }

    #[test]
    fn a_dashboard_that_has_never_been_arranged_has_no_layout() {
        let json = r#"{"id":1,"name":"New","layout":null,
            "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#;
        let dashboard: Dashboard = serde_json::from_str(json).unwrap();
        assert_eq!(dashboard.layout, None);
    }
}
