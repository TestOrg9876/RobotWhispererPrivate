//! ROS 2 parameters, over the services every node already exposes.
//!
//! There is no parameter *protocol*: `ros2 param` is a client that calls
//! `rcl_interfaces` services on the node it is asking about. Which means this
//! needs no native transport and no new plumbing — it works over rosbridge and
//! Foxglove today, through the same `call_service` path a service request
//! already uses.
//!
//! The shape it hands back is deliberately an ordinary message: a struct of
//! parameter name to value. Once it is that, the raw view, the field table,
//! the plot and the diff all work on parameters for free, and "what did that
//! node's gains just change to" is the freeze-and-diff question again.
//!
//! Pure and tested; nothing here opens a socket.

use std::collections::BTreeMap;

use rw_canonical::CanonicalValue;
// The schema types are `rw-core`'s rather than `rw-canonical`'s because the
// form renderer this feeds takes those, and there is only one form renderer.
use rw_core::schema::{ArrayLength, FieldDef, FieldType, MessageDef, PrimitiveType};

/// The services a node exposes for its parameters.
pub const LIST: &str = "list_parameters";
pub const GET: &str = "get_parameters";
pub const SET: &str = "set_parameters";

/// `rcl_interfaces/msg/ParameterType`, by its own numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    NotSet,
    Bool,
    Integer,
    Double,
    String,
    ByteArray,
    BoolArray,
    IntegerArray,
    DoubleArray,
    StringArray,
}

impl Kind {
    pub fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            0 => Self::NotSet,
            1 => Self::Bool,
            2 => Self::Integer,
            3 => Self::Double,
            4 => Self::String,
            5 => Self::ByteArray,
            6 => Self::BoolArray,
            7 => Self::IntegerArray,
            8 => Self::DoubleArray,
            9 => Self::StringArray,
            _ => return None,
        })
    }

    pub fn code(self) -> u64 {
        match self {
            Self::NotSet => 0,
            Self::Bool => 1,
            Self::Integer => 2,
            Self::Double => 3,
            Self::String => 4,
            Self::ByteArray => 5,
            Self::BoolArray => 6,
            Self::IntegerArray => 7,
            Self::DoubleArray => 8,
            Self::StringArray => 9,
        }
    }

    /// The field of `ParameterValue` this kind's payload lives in.
    pub fn field(self) -> &'static str {
        match self {
            // Nothing holds a NOT_SET; the name is here so the mapping is total
            // and a caller never has to special-case it before asking.
            Self::NotSet => "bool_value",
            Self::Bool => "bool_value",
            Self::Integer => "integer_value",
            Self::Double => "double_value",
            Self::String => "string_value",
            Self::ByteArray => "byte_array_value",
            Self::BoolArray => "bool_array_value",
            Self::IntegerArray => "integer_array_value",
            Self::DoubleArray => "double_array_value",
            Self::StringArray => "string_array_value",
        }
    }

    /// How the type reads beside a field's name in the form.
    pub fn type_name(self) -> &'static str {
        match self {
            Self::NotSet => "unset",
            Self::Bool => "bool",
            Self::Integer => "int64",
            Self::Double => "double",
            Self::String => "string",
            Self::ByteArray => "byte[]",
            Self::BoolArray => "bool[]",
            Self::IntegerArray => "int64[]",
            Self::DoubleArray => "double[]",
            Self::StringArray => "string[]",
        }
    }

    /// The kind a canonical value would be written back as.
    ///
    /// The way round that matters: the form gives back whatever the user typed,
    /// and a node will refuse a `double` where it declared an `integer`. The
    /// kind the parameter was *read* as is therefore what a write uses, and
    /// this is only the fallback for a value with no history.
    pub fn of(value: &CanonicalValue) -> Self {
        match value {
            CanonicalValue::Bool(_) => Self::Bool,
            CanonicalValue::Int(_) | CanonicalValue::Uint(_) => Self::Integer,
            CanonicalValue::F32(_) | CanonicalValue::F64(_) => Self::Double,
            CanonicalValue::String(_) => Self::String,
            CanonicalValue::Bytes(_) => Self::ByteArray,
            CanonicalValue::Array(items) => match items.first() {
                Some(CanonicalValue::Bool(_)) => Self::BoolArray,
                Some(CanonicalValue::Int(_) | CanonicalValue::Uint(_)) => Self::IntegerArray,
                Some(CanonicalValue::F32(_) | CanonicalValue::F64(_)) => Self::DoubleArray,
                Some(CanonicalValue::String(_)) => Self::StringArray,
                // An empty array says nothing about what it would hold, and a
                // node will not accept a guess. Strings are the one kind every
                // node can parse back into whatever it wanted.
                _ => Self::StringArray,
            },
            _ => Self::NotSet,
        }
    }
}

/// The node a `…/get_parameters` service belongs to.
///
/// Node names are namespaced, so the node is everything before the last
/// segment — `/ns/planner/get_parameters` is `/ns/planner`, not `/planner`.
pub fn node_of(service: &str) -> Option<String> {
    let (node, tail) = service.rsplit_once('/')?;
    (tail == GET && !node.is_empty()).then(|| node.to_string())
}

/// The service name to call on a node.
pub fn service(node: &str, which: &str) -> String {
    format!("{}/{which}", node.trim_end_matches('/'))
}

/// Every node in a discovery that has parameters to offer.
///
/// Found the way `ros2 param list` finds them: a node that answers
/// `get_parameters` has parameters, and one that does not cannot be asked.
pub fn nodes<'a>(services: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut nodes: Vec<String> = services.into_iter().filter_map(node_of).collect();
    nodes.sort();
    nodes.dedup();
    nodes
}

/// A `ListParameters` request for everything the node has.
pub fn list_request() -> CanonicalValue {
    struct_of([
        ("prefixes", CanonicalValue::Array(Vec::new())),
        // Depth 0 is `ListParameters::DEPTH_RECURSIVE`: every parameter, not
        // only the ones at the top of the tree.
        ("depth", CanonicalValue::Uint(0)),
    ])
}

/// The names a `ListParameters` response carries.
pub fn decode_list(response: &CanonicalValue) -> Option<Vec<String>> {
    let CanonicalValue::Array(names) = response.get_path("result.names")? else {
        return None;
    };
    Some(
        names
            .iter()
            .filter_map(|name| match name {
                CanonicalValue::String(name) => Some(name.clone()),
                _ => None,
            })
            .collect(),
    )
}

/// A `GetParameters` request for these names.
pub fn get_request(names: &[String]) -> CanonicalValue {
    struct_of([(
        "names",
        CanonicalValue::Array(
            names
                .iter()
                .map(|name| CanonicalValue::String(name.clone()))
                .collect(),
        ),
    )])
}

/// The values a `GetParameters` response carries, as an ordinary message.
///
/// Keyed by name and paired positionally with the names that were asked for,
/// because the response carries only values — the correspondence is the
/// request's order, and a response of the wrong length is a node that cannot
/// be trusted to have answered the question asked.
pub fn decode_values(names: &[String], response: &CanonicalValue) -> Option<CanonicalValue> {
    let CanonicalValue::Array(values) = response.get_path("values")? else {
        return None;
    };
    if values.len() != names.len() {
        return None;
    }
    let mut out = BTreeMap::new();
    for (name, value) in names.iter().zip(values) {
        out.insert(
            name.clone(),
            read_value(value).unwrap_or(CanonicalValue::Null),
        );
    }
    Some(CanonicalValue::Struct(out))
}

/// One `ParameterValue`, as the scalar it holds.
pub fn read_value(value: &CanonicalValue) -> Option<CanonicalValue> {
    let kind = Kind::from_code(whole(value.get_path("type")?)?)?;
    if kind == Kind::NotSet {
        return Some(CanonicalValue::Null);
    }
    value.get_path(kind.field()).cloned()
}

/// The kinds of every parameter in a `GetParameters` response, by name.
///
/// Kept alongside the values so a write can send each parameter back as the
/// type it was declared with rather than as whatever the form's text parsed
/// into — a node will refuse a `double` where it wanted an `integer`.
pub fn read_kinds(names: &[String], response: &CanonicalValue) -> BTreeMap<String, Kind> {
    let Some(CanonicalValue::Array(values)) = response.get_path("values") else {
        return BTreeMap::new();
    };
    names
        .iter()
        .zip(values)
        .filter_map(|(name, value)| {
            let kind = Kind::from_code(whole(value.get_path("type")?)?)?;
            Some((name.clone(), kind))
        })
        .collect()
}

/// A `SetParameters` request for every name in `values`.
///
/// `kinds` says what each parameter was declared as; anything not in it — and
/// anything the node reported as not set — is written as whatever its value
/// looks like. A parameter with no value has no declared type to honour, and
/// writing back the `NOT_SET` it was read as would throw away what was typed
/// into it, which is the one thing setting it was for.
pub fn set_request(values: &CanonicalValue, kinds: &BTreeMap<String, Kind>) -> CanonicalValue {
    let CanonicalValue::Struct(fields) = values else {
        return struct_of([("parameters", CanonicalValue::Array(Vec::new()))]);
    };
    let parameters = fields
        .iter()
        .map(|(name, value)| {
            let declared = kinds.get(name).copied().unwrap_or(Kind::NotSet);
            let kind = match declared {
                Kind::NotSet => Kind::of(value),
                declared => declared,
            };
            struct_of([
                ("name", CanonicalValue::String(name.clone())),
                ("value", write_value(kind, value)),
            ])
        })
        .collect();
    struct_of([("parameters", CanonicalValue::Array(parameters))])
}

/// One `ParameterValue`, with every field a node expects to find.
///
/// All of them, not only the one in use: `rcl_interfaces/ParameterValue` is a
/// fixed message and a bridge that encodes it field by field will refuse one
/// with holes in it.
pub fn write_value(kind: Kind, value: &CanonicalValue) -> CanonicalValue {
    let mut fields = BTreeMap::from([
        ("type".to_string(), CanonicalValue::Uint(kind.code())),
        ("bool_value".to_string(), CanonicalValue::Bool(false)),
        ("integer_value".to_string(), CanonicalValue::Int(0)),
        ("double_value".to_string(), CanonicalValue::F64(0.)),
        (
            "string_value".to_string(),
            CanonicalValue::String(String::new()),
        ),
        (
            "byte_array_value".to_string(),
            CanonicalValue::Array(Vec::new()),
        ),
        (
            "bool_array_value".to_string(),
            CanonicalValue::Array(Vec::new()),
        ),
        (
            "integer_array_value".to_string(),
            CanonicalValue::Array(Vec::new()),
        ),
        (
            "double_array_value".to_string(),
            CanonicalValue::Array(Vec::new()),
        ),
        (
            "string_array_value".to_string(),
            CanonicalValue::Array(Vec::new()),
        ),
    ]);
    if kind != Kind::NotSet
        && let Some(coerced) = coerce(kind, value)
    {
        fields.insert(kind.field().to_string(), coerced);
    }
    CanonicalValue::Struct(fields)
}

/// A value written as the kind the node declared.
///
/// The form hands back what the text parsed into, and `2` typed into a
/// `double` parameter parses as an integer. Coercing here rather than refusing
/// is the difference between a parameter that can be typed at and one that
/// takes a trailing `.0` before a node will accept it.
fn coerce(kind: Kind, value: &CanonicalValue) -> Option<CanonicalValue> {
    Some(match (kind, value) {
        (Kind::Double, CanonicalValue::Int(number)) => CanonicalValue::F64(*number as f64),
        (Kind::Double, CanonicalValue::Uint(number)) => CanonicalValue::F64(*number as f64),
        (Kind::Integer, CanonicalValue::F64(number)) if number.fract() == 0. => {
            CanonicalValue::Int(*number as i64)
        }
        (Kind::Integer, CanonicalValue::Uint(number)) => {
            CanonicalValue::Int(i64::try_from(*number).ok()?)
        }
        (Kind::DoubleArray, CanonicalValue::Array(items)) => CanonicalValue::Array(
            items
                .iter()
                .map(|item| match item {
                    CanonicalValue::Int(number) => CanonicalValue::F64(*number as f64),
                    other => other.clone(),
                })
                .collect(),
        ),
        _ => value.clone(),
    })
}

/// What a `SetParameters` response says went wrong, by parameter name.
///
/// An empty map is every write accepted. A node that refuses one gives a
/// reason, and that reason is the whole value of asking.
pub fn decode_set_results(names: &[String], response: &CanonicalValue) -> Vec<(String, String)> {
    let Some(CanonicalValue::Array(results)) = response.get_path("results") else {
        return Vec::new();
    };
    names
        .iter()
        .zip(results)
        .filter_map(|(name, result)| {
            if matches!(
                result.get_path("successful"),
                Some(CanonicalValue::Bool(true))
            ) {
                return None;
            }
            let reason = match result.get_path("reason") {
                Some(CanonicalValue::String(reason)) if !reason.is_empty() => reason.clone(),
                _ => "the node refused it without saying why".to_string(),
            };
            Some((name.clone(), reason))
        })
        .collect()
}

/// A message definition describing these parameters, so the ordinary form
/// renderer can draw an editor of the right shape for each.
///
/// Parameters have no schema on the wire — their types are whatever the node
/// declared, discovered by reading them. Synthesising a `MessageDef` is what
/// lets `form::fields` do the rest rather than this growing a second form.
pub fn message_def(values: &CanonicalValue, kinds: &BTreeMap<String, Kind>) -> MessageDef {
    let CanonicalValue::Struct(fields) = values else {
        return MessageDef {
            fields: Vec::new(),
            constants: Vec::new(),
        };
    };
    MessageDef {
        fields: fields
            .iter()
            .map(|(name, value)| {
                let kind = kinds.get(name).copied().unwrap_or_else(|| Kind::of(value));
                FieldDef {
                    name: name.clone(),
                    field_type: field_type(kind),
                    default: None,
                    comment: None,
                }
            })
            .collect(),
        constants: Vec::new(),
    }
}

fn field_type(kind: Kind) -> FieldType {
    let array = |element| FieldType::Array {
        element: Box::new(element),
        length: ArrayLength::Unbounded,
    };
    match kind {
        Kind::Bool => FieldType::Primitive(PrimitiveType::Bool),
        Kind::Integer => FieldType::Primitive(PrimitiveType::Int64),
        Kind::Double => FieldType::Primitive(PrimitiveType::Float64),
        // A parameter nothing has set is still a parameter, and text is the one
        // editor that can carry whatever it is about to become.
        Kind::String | Kind::NotSet => FieldType::String { bound: None },
        Kind::ByteArray => array(FieldType::Primitive(PrimitiveType::Uint8)),
        Kind::BoolArray => array(FieldType::Primitive(PrimitiveType::Bool)),
        Kind::IntegerArray => array(FieldType::Primitive(PrimitiveType::Int64)),
        Kind::DoubleArray => array(FieldType::Primitive(PrimitiveType::Float64)),
        Kind::StringArray => array(FieldType::String { bound: None }),
    }
}

fn whole(value: &CanonicalValue) -> Option<i64> {
    match value {
        CanonicalValue::Int(inner) => Some(*inner),
        CanonicalValue::Uint(inner) => i64::try_from(*inner).ok(),
        _ => None,
    }
}

fn struct_of<const N: usize>(fields: [(&str, CanonicalValue); N]) -> CanonicalValue {
    CanonicalValue::Struct(
        fields
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect::<BTreeMap<_, _>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parameter_value<const N: usize>(
        code: u64,
        fields: [(&str, CanonicalValue); N],
    ) -> CanonicalValue {
        let mut map = BTreeMap::from([("type".to_string(), CanonicalValue::Uint(code))]);
        for (name, value) in fields {
            map.insert(name.to_string(), value);
        }
        CanonicalValue::Struct(map)
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn a_node_is_everything_before_the_service_name() {
        assert_eq!(
            node_of("/planner/get_parameters").as_deref(),
            Some("/planner")
        );
        assert_eq!(
            node_of("/robot/nav/planner/get_parameters").as_deref(),
            Some("/robot/nav/planner"),
            "node names are namespaced"
        );
        assert_eq!(node_of("/planner/set_parameters"), None);
        assert_eq!(node_of("/get_parameters"), None, "a node needs a name");
        assert_eq!(node_of("get_parameters"), None);
    }

    #[test]
    fn the_nodes_with_parameters_are_the_ones_that_answer_for_them() {
        let services = [
            "/planner/get_parameters",
            "/planner/set_parameters",
            "/planner/list_parameters",
            "/camera/get_parameters",
            "/dummy/add_two_ints",
        ];
        assert_eq!(nodes(services), vec!["/camera", "/planner"]);
    }

    #[test]
    fn a_service_name_is_built_off_the_node_without_doubling_its_slash() {
        assert_eq!(service("/planner", GET), "/planner/get_parameters");
        assert_eq!(service("/planner/", SET), "/planner/set_parameters");
    }

    #[test]
    fn listing_asks_for_everything_at_any_depth() {
        let request = list_request();
        assert_eq!(
            request.get_path("depth"),
            Some(&CanonicalValue::Uint(0)),
            "zero is DEPTH_RECURSIVE, not zero levels"
        );
        assert!(matches!(
            request.get_path("prefixes"),
            Some(CanonicalValue::Array(prefixes)) if prefixes.is_empty()
        ));
    }

    #[test]
    fn a_list_response_gives_back_its_names() {
        let response = struct_of([(
            "result",
            struct_of([(
                "names",
                CanonicalValue::Array(vec![
                    CanonicalValue::String("use_sim_time".into()),
                    CanonicalValue::String("max_speed".into()),
                ]),
            )]),
        )]);
        assert_eq!(
            decode_list(&response),
            Some(names(&["use_sim_time", "max_speed"]))
        );
        assert_eq!(decode_list(&struct_of([])), None);
    }

    #[test]
    fn every_parameter_type_reads_back_as_the_value_it_holds() {
        let cases = [
            (
                parameter_value(1, [("bool_value", CanonicalValue::Bool(true))]),
                CanonicalValue::Bool(true),
            ),
            (
                parameter_value(2, [("integer_value", CanonicalValue::Int(42))]),
                CanonicalValue::Int(42),
            ),
            (
                parameter_value(3, [("double_value", CanonicalValue::F64(1.5))]),
                CanonicalValue::F64(1.5),
            ),
            (
                parameter_value(4, [("string_value", CanonicalValue::String("hi".into()))]),
                CanonicalValue::String("hi".into()),
            ),
            (
                parameter_value(
                    7,
                    [(
                        "integer_array_value",
                        CanonicalValue::Array(vec![CanonicalValue::Int(1)]),
                    )],
                ),
                CanonicalValue::Array(vec![CanonicalValue::Int(1)]),
            ),
        ];
        for (wire, expected) in cases {
            assert_eq!(read_value(&wire), Some(expected), "{wire:?}");
        }
    }

    #[test]
    fn a_parameter_that_is_not_set_reads_as_null_rather_than_as_false() {
        // Type 0 leaves every field at its default, and `bool_value` would
        // otherwise be read as a parameter that is genuinely false.
        let wire = parameter_value(0, [("bool_value", CanonicalValue::Bool(false))]);
        assert_eq!(read_value(&wire), Some(CanonicalValue::Null));
    }

    #[test]
    fn values_are_paired_with_the_names_that_were_asked_for() {
        let response = struct_of([(
            "values",
            CanonicalValue::Array(vec![
                parameter_value(1, [("bool_value", CanonicalValue::Bool(true))]),
                parameter_value(3, [("double_value", CanonicalValue::F64(2.5))]),
            ]),
        )]);
        let values =
            decode_values(&names(&["use_sim_time", "max_speed"]), &response).expect("decodes");
        assert_eq!(
            values.get_path("use_sim_time"),
            Some(&CanonicalValue::Bool(true))
        );
        assert_eq!(
            values.get_path("max_speed"),
            Some(&CanonicalValue::F64(2.5))
        );
    }

    #[test]
    fn a_response_of_the_wrong_length_is_refused_rather_than_mispaired() {
        // The correspondence is positional, so a short response would silently
        // give one parameter another's value.
        let response = struct_of([(
            "values",
            CanonicalValue::Array(vec![parameter_value(
                1,
                [("bool_value", CanonicalValue::Bool(true))],
            )]),
        )]);
        assert_eq!(decode_values(&names(&["a", "b"]), &response), None);
    }

    #[test]
    fn a_write_carries_every_field_the_message_declares() {
        // A bridge that encodes ParameterValue field by field refuses one with
        // holes in it, however unused those fields are.
        let written = write_value(Kind::Integer, &CanonicalValue::Int(7));
        for field in [
            "type",
            "bool_value",
            "integer_value",
            "double_value",
            "string_value",
            "byte_array_value",
            "bool_array_value",
            "integer_array_value",
            "double_array_value",
            "string_array_value",
        ] {
            assert!(written.get_path(field).is_some(), "{field} is missing");
        }
        assert_eq!(written.get_path("type"), Some(&CanonicalValue::Uint(2)));
        assert_eq!(
            written.get_path("integer_value"),
            Some(&CanonicalValue::Int(7))
        );
    }

    #[test]
    fn a_whole_number_typed_into_a_double_is_written_as_a_double() {
        // `2` typed into a double parameter parses as an integer, and a node
        // refuses that — needing a trailing `.0` is the kind of thing that
        // makes a parameter editor useless.
        let written = write_value(Kind::Double, &CanonicalValue::Int(2));
        assert_eq!(
            written.get_path("double_value"),
            Some(&CanonicalValue::F64(2.))
        );
    }

    #[test]
    fn a_whole_double_typed_into_an_integer_is_written_as_an_integer() {
        let written = write_value(Kind::Integer, &CanonicalValue::F64(3.0));
        assert_eq!(
            written.get_path("integer_value"),
            Some(&CanonicalValue::Int(3))
        );
    }

    #[test]
    fn a_list_of_whole_numbers_typed_into_a_double_array_becomes_doubles() {
        let written = write_value(
            Kind::DoubleArray,
            &CanonicalValue::Array(vec![CanonicalValue::Int(1), CanonicalValue::Int(2)]),
        );
        assert_eq!(
            written.get_path("double_array_value"),
            Some(&CanonicalValue::Array(vec![
                CanonicalValue::F64(1.),
                CanonicalValue::F64(2.)
            ]))
        );
    }

    #[test]
    fn a_write_uses_the_kind_the_parameter_was_declared_with() {
        let values = struct_of([("gain", CanonicalValue::Int(2))]);
        let kinds = BTreeMap::from([("gain".to_string(), Kind::Double)]);
        let request = set_request(&values, &kinds);
        let CanonicalValue::Array(parameters) = request.get_path("parameters").unwrap() else {
            panic!("parameters is not an array")
        };
        assert_eq!(parameters.len(), 1);
        assert_eq!(
            parameters[0].get_path("name"),
            Some(&CanonicalValue::String("gain".into()))
        );
        assert_eq!(
            parameters[0].get_path("value.type"),
            Some(&CanonicalValue::Uint(3)),
            "declared a double, so written as one"
        );
        assert_eq!(
            parameters[0].get_path("value.double_value"),
            Some(&CanonicalValue::F64(2.))
        );
    }

    /// The commonest way to meet a parameter with no kind: the node declared it
    /// and nothing has ever given it a value. Writing back the `NOT_SET` it was
    /// read as would discard what was just typed into it.
    #[test]
    fn setting_a_parameter_the_node_had_no_value_for_sends_the_type_that_was_typed() {
        let values = struct_of([(
            "log_directory",
            CanonicalValue::String("/tmp/planner".into()),
        )]);
        let kinds = BTreeMap::from([("log_directory".to_string(), Kind::NotSet)]);

        let request = set_request(&values, &kinds);
        let CanonicalValue::Array(parameters) = request.get_path("parameters").unwrap() else {
            panic!("parameters is not an array")
        };

        assert_eq!(
            parameters[0].get_path("value.type"),
            Some(&CanonicalValue::Uint(Kind::String.code()))
        );
        assert_eq!(
            parameters[0].get_path("value.string_value"),
            Some(&CanonicalValue::String("/tmp/planner".into()))
        );
    }

    /// And one that is still empty stays empty: nothing was typed, so there is
    /// nothing to give it.
    #[test]
    fn a_parameter_left_unset_is_still_written_as_unset() {
        let values = struct_of([("log_directory", CanonicalValue::Null)]);
        let kinds = BTreeMap::from([("log_directory".to_string(), Kind::NotSet)]);

        let request = set_request(&values, &kinds);
        let CanonicalValue::Array(parameters) = request.get_path("parameters").unwrap() else {
            panic!("parameters is not an array")
        };

        assert_eq!(
            parameters[0].get_path("value.type"),
            Some(&CanonicalValue::Uint(Kind::NotSet.code()))
        );
    }

    #[test]
    fn a_parameter_with_no_declared_kind_is_written_as_what_it_looks_like() {
        let values = struct_of([
            ("flag", CanonicalValue::Bool(true)),
            ("name", CanonicalValue::String("arm".into())),
        ]);
        let request = set_request(&values, &BTreeMap::new());
        let CanonicalValue::Array(parameters) = request.get_path("parameters").unwrap() else {
            panic!("parameters is not an array")
        };
        // BTreeMap order: flag, then name.
        assert_eq!(
            parameters[0].get_path("value.type"),
            Some(&CanonicalValue::Uint(1))
        );
        assert_eq!(
            parameters[1].get_path("value.type"),
            Some(&CanonicalValue::Uint(4))
        );
    }

    #[test]
    fn a_refused_write_says_which_parameter_and_why() {
        let response = struct_of([(
            "results",
            CanonicalValue::Array(vec![
                struct_of([
                    ("successful", CanonicalValue::Bool(true)),
                    ("reason", CanonicalValue::String(String::new())),
                ]),
                struct_of([
                    ("successful", CanonicalValue::Bool(false)),
                    (
                        "reason",
                        CanonicalValue::String("must be between 0 and 1".into()),
                    ),
                ]),
            ]),
        )]);
        assert_eq!(
            decode_set_results(&names(&["gain", "ratio"]), &response),
            vec![("ratio".to_string(), "must be between 0 and 1".to_string())]
        );
    }

    #[test]
    fn a_refusal_with_no_reason_still_says_it_was_refused() {
        let response = struct_of([(
            "results",
            CanonicalValue::Array(vec![struct_of([(
                "successful",
                CanonicalValue::Bool(false),
            )])]),
        )]);
        let refused = decode_set_results(&names(&["gain"]), &response);
        assert_eq!(refused.len(), 1);
        assert!(refused[0].1.contains("without saying why"));
    }

    #[test]
    fn every_write_accepted_is_an_empty_list_rather_than_a_report() {
        let response = struct_of([(
            "results",
            CanonicalValue::Array(vec![struct_of([(
                "successful",
                CanonicalValue::Bool(true),
            )])]),
        )]);
        assert_eq!(decode_set_results(&names(&["gain"]), &response), Vec::new());
    }

    #[test]
    fn a_form_is_built_with_an_editor_of_the_right_shape_for_each_parameter() {
        let values = struct_of([
            ("use_sim_time", CanonicalValue::Bool(false)),
            ("max_speed", CanonicalValue::F64(1.5)),
            ("frames", CanonicalValue::Array(vec![])),
        ]);
        let kinds = BTreeMap::from([
            ("use_sim_time".to_string(), Kind::Bool),
            ("max_speed".to_string(), Kind::Double),
            ("frames".to_string(), Kind::StringArray),
        ]);
        let message = message_def(&values, &kinds);
        let by_name: BTreeMap<&str, &FieldType> = message
            .fields
            .iter()
            .map(|field| (field.name.as_str(), &field.field_type))
            .collect();
        assert_eq!(
            by_name["use_sim_time"],
            &FieldType::Primitive(PrimitiveType::Bool)
        );
        assert_eq!(
            by_name["max_speed"],
            &FieldType::Primitive(PrimitiveType::Float64)
        );
        assert!(matches!(
            by_name["frames"],
            FieldType::Array { element, .. } if matches!(**element, FieldType::String { .. })
        ));
    }

    #[test]
    fn an_unset_parameter_still_gets_an_editor() {
        // A declared-but-unset parameter is the one people most want to set.
        let values = struct_of([("target", CanonicalValue::Null)]);
        let kinds = BTreeMap::from([("target".to_string(), Kind::NotSet)]);
        let message = message_def(&values, &kinds);
        assert_eq!(message.fields.len(), 1);
        assert!(matches!(
            message.fields[0].field_type,
            FieldType::String { .. }
        ));
    }

    #[test]
    fn a_response_that_is_not_a_parameter_response_yields_nothing_rather_than_guessing() {
        let nonsense = struct_of([("data", CanonicalValue::Int(1))]);
        assert_eq!(decode_list(&nonsense), None);
        assert_eq!(decode_values(&names(&["a"]), &nonsense), None);
        assert!(read_kinds(&names(&["a"]), &nonsense).is_empty());
        assert_eq!(decode_set_results(&names(&["a"]), &nonsense), Vec::new());
    }

    #[test]
    fn the_kinds_come_back_beside_the_values() {
        let response = struct_of([(
            "values",
            CanonicalValue::Array(vec![
                parameter_value(3, [("double_value", CanonicalValue::F64(1.))]),
                parameter_value(1, [("bool_value", CanonicalValue::Bool(true))]),
            ]),
        )]);
        let kinds = read_kinds(&names(&["gain", "flag"]), &response);
        assert_eq!(kinds["gain"], Kind::Double);
        assert_eq!(kinds["flag"], Kind::Bool);
    }

    #[test]
    fn every_code_maps_to_a_kind_and_back() {
        for code in 0..=9 {
            let kind = Kind::from_code(code).expect("a real parameter type");
            assert_eq!(kind.code(), code as u64);
            assert!(!kind.field().is_empty());
            assert!(!kind.type_name().is_empty());
        }
        assert_eq!(Kind::from_code(10), None);
        assert_eq!(Kind::from_code(-1), None);
    }
}
