//! A node that has parameters, so the parameter editor has something to talk to.
//!
//! ROS 2 parameters are not a protocol of their own: a node that declares one
//! answers three ordinary services for it. This implements those three the way
//! a node does — including refusing a name it never declared and refusing a
//! value of the wrong type — because a parameter editor that has only ever been
//! pointed at a node that says yes to everything has not been tested.
//!
//! The encoding here is written out by hand rather than shared with the
//! editor's decoder: the two agreeing is the thing worth checking, and they
//! cannot agree by construction if they are the same code.

use std::collections::BTreeMap;
use std::sync::Mutex;

use rw_canonical::CanonicalValue;

/// The node that has them. Namespaced, so the editor's "everything before the
/// service name" rule is exercised by more than one segment.
pub(crate) const NODE: &str = "/dummy/planner";

/// `ParameterType` codes, as `rcl_interfaces/msg/ParameterType` numbers them.
const NOT_SET: u64 = 0;
const BOOL: u64 = 1;
const INTEGER: u64 = 2;
const DOUBLE: u64 = 3;
const STRING: u64 = 4;
const DOUBLE_ARRAY: u64 = 8;
const STRING_ARRAY: u64 = 9;

/// One declared parameter: what it was declared as, and what it holds now.
///
/// The type is stored separately from the value rather than read off it,
/// because a parameter declared `double` and holding `2.0` must still refuse an
/// integer — which is exactly the case a value-shaped guess gets wrong.
#[derive(Debug, Clone)]
struct Param {
    kind: u64,
    value: CanonicalValue,
}

/// The node's parameter store.
#[derive(Debug)]
pub(crate) struct Params {
    declared: Mutex<BTreeMap<String, Param>>,
}

impl Params {
    pub(crate) fn new() -> Self {
        let declared = [
            ("controller_frequency", DOUBLE, CanonicalValue::F64(20.0)),
            ("max_velocity", DOUBLE, CanonicalValue::F64(0.55)),
            ("robot_radius", DOUBLE, CanonicalValue::F64(0.22)),
            ("retries", INTEGER, CanonicalValue::Int(3)),
            ("use_sim_time", BOOL, CanonicalValue::Bool(false)),
            (
                "planner_plugin",
                STRING,
                CanonicalValue::String("navfn".into()),
            ),
            (
                "footprint",
                DOUBLE_ARRAY,
                CanonicalValue::Array(vec![
                    CanonicalValue::F64(0.2),
                    CanonicalValue::F64(0.2),
                    CanonicalValue::F64(-0.2),
                    CanonicalValue::F64(-0.2),
                ]),
            ),
            (
                "recovery_behaviours",
                STRING_ARRAY,
                CanonicalValue::Array(vec![
                    CanonicalValue::String("spin".into()),
                    CanonicalValue::String("back_up".into()),
                ]),
            ),
            // Declared and never given a value, which is a state `ros2 param
            // get` reports every day and which reads as nothing rather than as
            // an empty string.
            ("log_directory", NOT_SET, CanonicalValue::Null),
        ];

        Params {
            declared: Mutex::new(
                declared
                    .into_iter()
                    .map(|(name, kind, value)| (name.to_string(), Param { kind, value }))
                    .collect(),
            ),
        }
    }

    /// `ListParameters`: every name the node declares.
    pub(crate) fn list(&self) -> CanonicalValue {
        let names = self
            .declared
            .lock()
            .expect("parameters")
            .keys()
            .map(|name| CanonicalValue::String(name.clone()))
            .collect();

        struct_of([(
            "result",
            struct_of([
                ("names", CanonicalValue::Array(names)),
                ("prefixes", CanonicalValue::Array(Vec::new())),
            ]),
        )])
    }

    /// `GetParameters`: the values, in the order they were asked for.
    ///
    /// A name the node does not have still gets an answer — a `NOT_SET` value —
    /// because the response is positional and dropping one would silently shift
    /// every value after it onto the wrong name.
    pub(crate) fn get(&self, request: &CanonicalValue) -> CanonicalValue {
        let declared = self.declared.lock().expect("parameters");
        let values = names_of(request, "names")
            .into_iter()
            .map(|name| match declared.get(&name) {
                Some(param) => encode(param.kind, &param.value),
                None => encode(NOT_SET, &CanonicalValue::Null),
            })
            .collect();

        struct_of([("values", CanonicalValue::Array(values))])
    }

    /// `SetParameters`: one result per parameter, in the order they were sent.
    pub(crate) fn set(&self, request: &CanonicalValue) -> CanonicalValue {
        let Some(CanonicalValue::Array(parameters)) = request.get_path("parameters") else {
            return struct_of([("results", CanonicalValue::Array(Vec::new()))]);
        };
        let mut declared = self.declared.lock().expect("parameters");

        let results = parameters
            .iter()
            .map(|parameter| {
                let Some(CanonicalValue::String(name)) = parameter.get_path("name") else {
                    return refused("a parameter was sent with no name");
                };
                let Some(param) = declared.get_mut(name) else {
                    return refused(format!("{name} was never declared"));
                };
                let Some(value) = parameter.get_path("value") else {
                    return refused(format!("{name} was sent with no value"));
                };
                let Some(kind) = code_of(value) else {
                    return refused(format!("{name} was sent with no type"));
                };
                // A parameter with no value has no type to violate: giving it
                // one is how it stops being unset, and the node takes the type
                // that came with it. ROS 2 calls this dynamic typing, and a
                // parameter declared without a value has it.
                if param.kind != NOT_SET && kind != param.kind {
                    return refused(format!(
                        "{name} is a {}, not a {}",
                        type_name(param.kind),
                        type_name(kind)
                    ));
                }
                if kind == NOT_SET {
                    return refused(format!("{name} was sent with no type"));
                }
                match value.get_path(field_of(kind)) {
                    Some(inner) => {
                        param.kind = kind;
                        param.value = inner.clone();
                        accepted()
                    }
                    None => refused(format!("{name} was sent without its {}", field_of(kind))),
                }
            })
            .collect();

        struct_of([("results", CanonicalValue::Array(results))])
    }
}

/// A `ParameterValue`, with every field present.
///
/// A real bridge encodes a message field by field from its schema, so it sends
/// all of them whatever the type says; one that carried only the field in use
/// would let a decoder pass here and fail against a robot.
fn encode(kind: u64, value: &CanonicalValue) -> CanonicalValue {
    let mut fields: BTreeMap<String, CanonicalValue> = [
        ("type", CanonicalValue::Uint(kind)),
        ("bool_value", CanonicalValue::Bool(false)),
        ("integer_value", CanonicalValue::Int(0)),
        ("double_value", CanonicalValue::F64(0.0)),
        ("string_value", CanonicalValue::String(String::new())),
        ("byte_array_value", CanonicalValue::Bytes(Vec::new())),
        ("bool_array_value", CanonicalValue::Array(Vec::new())),
        ("integer_array_value", CanonicalValue::Array(Vec::new())),
        ("double_array_value", CanonicalValue::Array(Vec::new())),
        ("string_array_value", CanonicalValue::Array(Vec::new())),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value))
    .collect();

    if kind != NOT_SET {
        fields.insert(field_of(kind).to_string(), value.clone());
    }
    CanonicalValue::Struct(fields)
}

/// Which field of a `ParameterValue` a type code reads from.
fn field_of(kind: u64) -> &'static str {
    match kind {
        BOOL => "bool_value",
        INTEGER => "integer_value",
        DOUBLE => "double_value",
        STRING => "string_value",
        5 => "byte_array_value",
        6 => "bool_array_value",
        7 => "integer_array_value",
        DOUBLE_ARRAY => "double_array_value",
        STRING_ARRAY => "string_array_value",
        _ => "type",
    }
}

/// What a node calls the type when it refuses one.
fn type_name(kind: u64) -> &'static str {
    match kind {
        BOOL => "bool",
        INTEGER => "integer",
        DOUBLE => "double",
        STRING => "string",
        5 => "byte array",
        6 => "bool array",
        7 => "integer array",
        DOUBLE_ARRAY => "double array",
        STRING_ARRAY => "string array",
        _ => "unset parameter",
    }
}

fn code_of(value: &CanonicalValue) -> Option<u64> {
    match value.get_path("type")? {
        CanonicalValue::Uint(code) => Some(*code),
        CanonicalValue::Int(code) => u64::try_from(*code).ok(),
        _ => None,
    }
}

fn names_of(request: &CanonicalValue, field: &str) -> Vec<String> {
    let Some(CanonicalValue::Array(names)) = request.get_path(field) else {
        return Vec::new();
    };
    names
        .iter()
        .filter_map(|name| match name {
            CanonicalValue::String(name) => Some(name.clone()),
            _ => None,
        })
        .collect()
}

fn accepted() -> CanonicalValue {
    struct_of([
        ("successful", CanonicalValue::Bool(true)),
        ("reason", CanonicalValue::String(String::new())),
    ])
}

fn refused(reason: impl Into<String>) -> CanonicalValue {
    struct_of([
        ("successful", CanonicalValue::Bool(false)),
        ("reason", CanonicalValue::String(reason.into())),
    ])
}

fn struct_of<const N: usize>(fields: [(&str, CanonicalValue); N]) -> CanonicalValue {
    CanonicalValue::Struct(
        fields
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        let listed = Params::new().list();
        let Some(CanonicalValue::Array(names)) = listed.get_path("result.names") else {
            panic!("a list of names");
        };
        names
            .iter()
            .map(|name| match name {
                CanonicalValue::String(name) => name.clone(),
                other => panic!("a name, not {other:?}"),
            })
            .collect()
    }

    fn asking_for(names: &[&str]) -> CanonicalValue {
        struct_of([(
            "names",
            CanonicalValue::Array(
                names
                    .iter()
                    .map(|name| CanonicalValue::String((*name).to_string()))
                    .collect(),
            ),
        )])
    }

    fn setting(name: &str, kind: u64, value: CanonicalValue) -> CanonicalValue {
        struct_of([(
            "parameters",
            CanonicalValue::Array(vec![struct_of([
                ("name", CanonicalValue::String(name.to_string())),
                ("value", encode(kind, &value)),
            ])]),
        )])
    }

    fn first_result(response: &CanonicalValue) -> (bool, String) {
        let Some(CanonicalValue::Array(results)) = response.get_path("results") else {
            panic!("a list of results");
        };
        let result = results.first().expect("one result");
        let successful = matches!(
            result.get_path("successful"),
            Some(CanonicalValue::Bool(true))
        );
        let reason = match result.get_path("reason") {
            Some(CanonicalValue::String(reason)) => reason.clone(),
            _ => String::new(),
        };
        (successful, reason)
    }

    #[test]
    fn the_node_lists_every_parameter_it_declares() {
        let listed = names();
        assert!(listed.contains(&"max_velocity".to_string()));
        assert!(listed.contains(&"log_directory".to_string()));
        assert_eq!(listed.len(), 9);
    }

    #[test]
    fn a_value_comes_back_in_the_field_its_type_names() {
        let params = Params::new();
        let got = params.get(&asking_for(&["max_velocity"]));
        let Some(CanonicalValue::Array(values)) = got.get_path("values") else {
            panic!("a list of values");
        };
        assert_eq!(
            values[0].get_path("type"),
            Some(&CanonicalValue::Uint(DOUBLE))
        );
        assert_eq!(
            values[0].get_path("double_value"),
            Some(&CanonicalValue::F64(0.55))
        );
    }

    /// The response is positional, so it has to be the same length as the
    /// request whatever the node knows.
    #[test]
    fn a_name_the_node_never_had_still_gets_a_place_in_the_answer() {
        let params = Params::new();
        let got = params.get(&asking_for(&["max_velocity", "invented", "retries"]));
        let Some(CanonicalValue::Array(values)) = got.get_path("values") else {
            panic!("a list of values");
        };
        assert_eq!(values.len(), 3);
        assert_eq!(
            values[1].get_path("type"),
            Some(&CanonicalValue::Uint(NOT_SET))
        );
        assert_eq!(
            values[2].get_path("integer_value"),
            Some(&CanonicalValue::Int(3))
        );
    }

    #[test]
    fn a_parameter_that_was_never_given_a_value_reports_itself_unset() {
        let params = Params::new();
        let got = params.get(&asking_for(&["log_directory"]));
        let Some(CanonicalValue::Array(values)) = got.get_path("values") else {
            panic!("a list of values");
        };
        assert_eq!(
            values[0].get_path("type"),
            Some(&CanonicalValue::Uint(NOT_SET))
        );
    }

    #[test]
    fn a_value_that_was_set_is_the_value_that_comes_back() {
        let params = Params::new();
        let response = params.set(&setting("max_velocity", DOUBLE, CanonicalValue::F64(1.25)));
        assert_eq!(first_result(&response), (true, String::new()));

        let got = params.get(&asking_for(&["max_velocity"]));
        let Some(CanonicalValue::Array(values)) = got.get_path("values") else {
            panic!("a list of values");
        };
        assert_eq!(
            values[0].get_path("double_value"),
            Some(&CanonicalValue::F64(1.25))
        );
    }

    #[test]
    fn a_value_of_the_wrong_type_is_refused_and_says_which_type_it_wanted() {
        let params = Params::new();
        let response = params.set(&setting("retries", DOUBLE, CanonicalValue::F64(2.5)));
        let (successful, reason) = first_result(&response);

        assert!(!successful);
        assert!(reason.contains("retries"), "{reason}");
        assert!(reason.contains("integer"), "{reason}");
    }

    /// ROS 2 calls this dynamic typing: a parameter declared without a value
    /// has no type to violate, and giving it one is how it stops being unset.
    #[test]
    fn a_parameter_with_no_value_takes_the_type_it_is_given() {
        let params = Params::new();
        let response = params.set(&setting(
            "log_directory",
            STRING,
            CanonicalValue::String("/tmp/planner".into()),
        ));
        assert_eq!(first_result(&response), (true, String::new()));

        let got = params.get(&asking_for(&["log_directory"]));
        let Some(CanonicalValue::Array(values)) = got.get_path("values") else {
            panic!("a list of values");
        };
        assert_eq!(
            values[0].get_path("type"),
            Some(&CanonicalValue::Uint(STRING))
        );
        assert_eq!(
            values[0].get_path("string_value"),
            Some(&CanonicalValue::String("/tmp/planner".into()))
        );
    }

    /// Having taken a type, it keeps it — the next write is checked against it
    /// like any other parameter's.
    #[test]
    fn a_parameter_that_has_taken_a_type_holds_the_node_to_it() {
        let params = Params::new();
        params.set(&setting(
            "log_directory",
            STRING,
            CanonicalValue::String("/tmp/planner".into()),
        ));

        let response = params.set(&setting("log_directory", INTEGER, CanonicalValue::Int(4)));
        let (successful, reason) = first_result(&response);
        assert!(!successful);
        assert!(reason.contains("string"), "{reason}");
    }

    #[test]
    fn a_parameter_the_node_never_declared_is_refused_by_name() {
        let params = Params::new();
        let response = params.set(&setting("invented", INTEGER, CanonicalValue::Int(1)));
        let (successful, reason) = first_result(&response);

        assert!(!successful);
        assert!(reason.contains("invented"), "{reason}");
    }

    /// A refused write must not half-land: the store keeps what it had.
    #[test]
    fn a_refused_write_changes_nothing() {
        let params = Params::new();
        params.set(&setting(
            "retries",
            STRING,
            CanonicalValue::String("lots".into()),
        ));

        let got = params.get(&asking_for(&["retries"]));
        let Some(CanonicalValue::Array(values)) = got.get_path("values") else {
            panic!("a list of values");
        };
        assert_eq!(
            values[0].get_path("integer_value"),
            Some(&CanonicalValue::Int(3))
        );
    }

    #[test]
    fn every_declared_parameter_can_be_read_and_written_as_the_type_it_declares() {
        let params = Params::new();
        for name in names() {
            let got = params.get(&asking_for(&[name.as_str()]));
            let Some(CanonicalValue::Array(values)) = got.get_path("values") else {
                panic!("a list of values");
            };
            let Some(CanonicalValue::Uint(kind)) = values[0].get_path("type") else {
                panic!("a type code");
            };
            if *kind == NOT_SET {
                continue;
            }
            let held = values[0]
                .get_path(field_of(*kind))
                .expect("the field its type names")
                .clone();
            let response = params.set(&setting(&name, *kind, held));
            assert_eq!(first_result(&response), (true, String::new()), "{name}");
        }
    }
}
