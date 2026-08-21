//! Turning a message schema into an editable form, and back into a value.
//!
//! Nested messages are flattened to dotted paths — `pose.position.x` — with one
//! editor per leaf. That is how ROS tooling presents these, and it keeps the
//! widget tree flat: a `geometry_msgs/PoseStamped` is eleven text fields rather
//! than four levels of nested boxes to collapse and expand.
//!
//! Everything here is pure. The rendering lives in [`crate::panels::request`],
//! so parsing, assembling and the shape of the form are testable without a
//! window.

use std::collections::BTreeMap;

use rw_core::domain::Value;
use rw_core::schema::{ArrayLength, FieldType, MessageDef, PrimitiveType};

/// How deep to follow nested messages.
///
/// ROS messages cannot be recursive, so this is a guard against a malformed
/// registry rather than a real limit; nothing legitimate approaches it.
const MAX_DEPTH: usize = 8;

/// How a leaf is edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Editor {
    Text,
    /// Whole numbers, signed or not.
    Integer,
    Decimal,
    Bool,
    /// Seconds and nanoseconds, entered as `sec.nanosec`.
    Time,
    /// Comma-separated values of the element type.
    List(Element),
}

/// The element type of a list, which is only ever a scalar: a list of nested
/// messages has no sensible single-line spelling and is left to the raw editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Element {
    Text,
    Integer,
    Decimal,
    Bool,
}

impl Editor {
    /// A hint shown in the empty field, naming what it will accept.
    pub fn placeholder(self) -> &'static str {
        match self {
            Editor::Text => "text",
            Editor::Integer => "0",
            Editor::Decimal => "0.0",
            Editor::Bool => "true or false",
            Editor::Time => "sec.nanosec",
            Editor::List(Element::Text) => "a, b, c",
            Editor::List(_) => "1, 2, 3",
        }
    }
}

/// One editable leaf of a message.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// Dotted path from the message root, and the form's key for this leaf.
    pub path: String,
    /// The leaf's own name, for the label.
    pub label: String,
    /// The type as the schema spells it, shown beside the label.
    pub type_name: String,
    pub editor: Editor,
    pub comment: Option<String>,
}

/// Flattens `message` into the leaves a form should show.
///
/// Nested messages are resolved through `lookup`, which is a function rather
/// than the schema registry so that this — the part with all the branching —
/// can be tested without a storage backend behind it. One that cannot be
/// resolved becomes a single text leaf rather than disappearing: an unknown
/// dependency should not silently drop a field the robot requires.
pub fn fields(message: &MessageDef, lookup: &impl Fn(&str) -> Option<MessageDef>) -> Vec<Field> {
    let mut leaves = Vec::new();
    flatten(message, "", 0, lookup, &mut leaves);
    leaves
}

fn flatten(
    message: &MessageDef,
    prefix: &str,
    depth: usize,
    lookup: &impl Fn(&str) -> Option<MessageDef>,
    out: &mut Vec<Field>,
) {
    for field in &message.fields {
        let path = if prefix.is_empty() {
            field.name.clone()
        } else {
            format!("{prefix}.{}", field.name)
        };

        match &field.field_type {
            FieldType::Complex { type_name } if depth < MAX_DEPTH => match lookup(type_name) {
                Some(nested) => flatten(&nested, &path, depth + 1, lookup, out),
                None => out.push(leaf(&path, field, type_name.clone(), Editor::Text)),
            },
            field_type => {
                let editor = editor_for(field_type);
                out.push(leaf(&path, field, describe(field_type), editor));
            }
        }
    }
}

fn leaf(path: &str, field: &rw_core::schema::FieldDef, type_name: String, editor: Editor) -> Field {
    Field {
        path: path.to_string(),
        label: field.name.clone(),
        type_name,
        editor,
        comment: field.comment.clone(),
    }
}

fn editor_for(field_type: &FieldType) -> Editor {
    match field_type {
        FieldType::Primitive(primitive) => primitive_editor(*primitive),
        FieldType::String { .. } | FieldType::WString { .. } => Editor::Text,
        FieldType::Time | FieldType::Duration => Editor::Time,
        FieldType::Array { element, .. } => match element.as_ref() {
            FieldType::Primitive(primitive) => match primitive_editor(*primitive) {
                Editor::Integer => Editor::List(Element::Integer),
                Editor::Decimal => Editor::List(Element::Decimal),
                Editor::Bool => Editor::List(Element::Bool),
                _ => Editor::List(Element::Text),
            },
            FieldType::String { .. } | FieldType::WString { .. } => Editor::List(Element::Text),
            // An array of messages has no single-line spelling worth inventing.
            _ => Editor::Text,
        },
        FieldType::Complex { .. } => Editor::Text,
    }
}

fn primitive_editor(primitive: PrimitiveType) -> Editor {
    match primitive {
        PrimitiveType::Bool => Editor::Bool,
        PrimitiveType::Float32 | PrimitiveType::Float64 => Editor::Decimal,
        _ => Editor::Integer,
    }
}

/// The type as a schema author would write it, for the label.
fn describe(field_type: &FieldType) -> String {
    match field_type {
        FieldType::Primitive(primitive) => primitive.as_str().to_string(),
        FieldType::String { .. } => "string".into(),
        FieldType::WString { .. } => "wstring".into(),
        FieldType::Time => "time".into(),
        FieldType::Duration => "duration".into(),
        FieldType::Complex { type_name } => type_name.clone(),
        FieldType::Array { element, length } => {
            let inner = describe(element);
            match length {
                ArrayLength::Unbounded => format!("{inner}[]"),
                ArrayLength::Bounded(bound) => format!("{inner}[<={bound}]"),
                ArrayLength::Fixed(size) => format!("{inner}[{size}]"),
            }
        }
    }
}

/// How many rows a list is ever given.
///
/// Past this the field stays the one comma-separated box it used to be:
/// nobody edits ten thousand numbers a row at a time, and building an editor
/// per element would cost more than the message did to arrive. It is a
/// fallback for data, not a second way to edit a list.
pub const MAX_ROWS: usize = 128;

/// Reads a list's rows into an array.
///
/// A blank row is dropped rather than becoming an empty string: a row added
/// and not filled in is a row the user is still thinking about. All rows blank
/// means the same as an empty field — leave it at its default.
pub fn parse_list(element: Element, rows: &[String]) -> Result<Option<Value>, String> {
    let mut items = Vec::new();
    for row in rows {
        let row = row.trim();
        if row.is_empty() {
            continue;
        }
        items.push(parse_element(element, row)?);
    }
    Ok((!items.is_empty()).then_some(Value::Array(items)))
}

/// The rows a stored value fills a list with, one per element.
///
/// `None` when the path holds something that is not a list, which is what
/// tells the caller to fall back to the single box.
pub fn rows_at(value: &Value, path: &str, element: Element) -> Option<Vec<String>> {
    let mut current = value;
    for segment in path.split('.') {
        let Value::Struct(fields) = current else {
            return None;
        };
        current = fields.get(segment)?;
    }
    let Value::Array(items) = current else {
        return None;
    };
    Some(
        items
            .iter()
            .map(|item| show(item, element_editor(element)))
            .collect(),
    )
}

/// Reads one leaf's text into a value.
///
/// An empty field is not an error: it means "leave this at its default", which
/// is what makes a twenty-field service callable without filling in twenty
/// fields. The caller drops the `None`s.
pub fn parse(editor: Editor, text: &str) -> Result<Option<Value>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }

    let value = match editor {
        Editor::Text => Value::String(text.to_string()),
        Editor::Integer => Value::Int(
            text.parse()
                .map_err(|_| format!("{text:?} is not a whole number"))?,
        ),
        Editor::Decimal => Value::F64(
            text.parse()
                .map_err(|_| format!("{text:?} is not a number"))?,
        ),
        Editor::Bool => Value::Bool(parse_bool(text)?),
        Editor::Time => {
            let (sec, nanosec) = match text.split_once('.') {
                Some((sec, nanos)) => (sec, nanos),
                None => (text, "0"),
            };
            Value::Time {
                sec: sec
                    .parse()
                    .map_err(|_| format!("{sec:?} is not a number of seconds"))?,
                nanosec: nanosec
                    .parse()
                    .map_err(|_| format!("{nanosec:?} is not a number of nanoseconds"))?,
            }
        }
        Editor::List(element) => Value::Array(
            text.split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(|entry| parse_element(element, entry))
                .collect::<Result<_, _>>()?,
        ),
    };
    Ok(Some(value))
}

fn parse_element(element: Element, text: &str) -> Result<Value, String> {
    Ok(match element {
        Element::Text => Value::String(text.to_string()),
        Element::Integer => Value::Int(
            text.parse()
                .map_err(|_| format!("{text:?} is not a whole number"))?,
        ),
        Element::Decimal => Value::F64(
            text.parse()
                .map_err(|_| format!("{text:?} is not a number"))?,
        ),
        Element::Bool => Value::Bool(parse_bool(text)?),
    })
}

fn parse_bool(text: &str) -> Result<bool, String> {
    match text.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(format!("{text:?} is not true or false")),
    }
}

/// Rebuilds the nested message from the flat, dotted leaves.
///
/// Paths are re-nested here rather than kept nested throughout, so the form's
/// state is a flat map of path to text — the only shape that survives a schema
/// changing under an already-filled form.
pub fn assemble(leaves: impl IntoIterator<Item = (String, Value)>) -> Value {
    let mut root = BTreeMap::new();
    for (path, value) in leaves {
        insert(&mut root, &path, value);
    }
    Value::Struct(root)
}

fn insert(parent: &mut BTreeMap<String, Value>, path: &str, value: Value) {
    match path.split_once('.') {
        None => {
            parent.insert(path.to_string(), value);
        }
        Some((head, rest)) => {
            // A leaf and a struct cannot share a name; the struct wins, because
            // it is the one with children still to place.
            let entry = parent
                .entry(head.to_string())
                .or_insert_with(|| Value::Struct(BTreeMap::new()));
            if !matches!(entry, Value::Struct(_)) {
                *entry = Value::Struct(BTreeMap::new());
            }
            let Value::Struct(children) = entry else {
                unreachable!("just replaced with a struct");
            };
            insert(children, rest, value);
        }
    }
}

/// Reads the leaf at `path` back out as the text its editor would show.
///
/// The inverse of [`assemble`], used to refill a form from a saved request: a
/// request is stored as a nested value, and the form is flat.
pub fn text_at(value: &Value, path: &str, editor: Editor) -> Option<String> {
    let mut current = value;
    for segment in path.split('.') {
        let Value::Struct(fields) = current else {
            return None;
        };
        current = fields.get(segment)?;
    }
    Some(show(current, editor))
}

fn show(value: &Value, editor: Editor) -> String {
    match (value, editor) {
        (Value::Array(items), Editor::List(element)) => items
            .iter()
            .map(|item| show(item, element_editor(element)))
            .collect::<Vec<_>>()
            .join(", "),
        (Value::Time { sec, nanosec }, _) | (Value::Duration { sec, nanosec }, _) => {
            format!("{sec}.{nanosec}")
        }
        (Value::Null, _) => String::new(),
        (Value::Bool(inner), _) => inner.to_string(),
        (Value::Int(inner), _) => inner.to_string(),
        (Value::Uint(inner), _) => inner.to_string(),
        (Value::F32(inner), _) => inner.to_string(),
        (Value::F64(inner), _) => inner.to_string(),
        (Value::String(inner), _) => inner.clone(),
        // A structure or a blob has no single-line spelling; showing a
        // truncated one would invite editing it into nonsense.
        (Value::Bytes(_) | Value::Array(_) | Value::Struct(_), _) => String::new(),
    }
}

/// One element of a list, as the editor a single row of it uses.
pub fn element_editor(element: Element) -> Editor {
    match element {
        Element::Text => Editor::Text,
        Element::Integer => Editor::Integer,
        Element::Decimal => Editor::Decimal,
        Element::Bool => Editor::Bool,
    }
}

#[cfg(test)]
mod list_tests {
    use super::*;

    fn rows(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| text.to_string()).collect()
    }

    #[test]
    fn every_row_becomes_an_element() {
        assert_eq!(
            parse_list(Element::Decimal, &rows(&["0.2", "-0.2"])),
            Ok(Some(Value::Array(vec![Value::F64(0.2), Value::F64(-0.2)])))
        );
    }

    /// A row added and not filled in is a row someone is still thinking about,
    /// not an empty string bound for the robot.
    #[test]
    fn a_blank_row_is_left_out() {
        assert_eq!(
            parse_list(Element::Text, &rows(&["spin", "  ", "back_up"])),
            Ok(Some(Value::Array(vec![
                Value::String("spin".into()),
                Value::String("back_up".into()),
            ])))
        );
    }

    #[test]
    fn a_list_of_nothing_but_blanks_is_left_at_its_default() {
        assert_eq!(parse_list(Element::Integer, &rows(&["", " "])), Ok(None));
        assert_eq!(parse_list(Element::Integer, &[]), Ok(None));
    }

    #[test]
    fn a_row_that_is_not_a_number_says_so_rather_than_being_dropped() {
        assert!(parse_list(Element::Integer, &rows(&["1", "two"])).is_err());
    }

    #[test]
    fn a_stored_array_comes_back_a_row_at_a_time() {
        let stored = assemble([(
            "footprint".to_string(),
            Value::Array(vec![Value::F64(0.2), Value::F64(-0.2)]),
        )]);

        assert_eq!(
            rows_at(&stored, "footprint", Element::Decimal),
            Some(rows(&["0.2", "-0.2"]))
        );
    }

    /// What tells the caller to fall back to the single box: the stored value
    /// is not a list at all, so there are no rows to lay out.
    #[test]
    fn something_that_is_not_a_list_has_no_rows() {
        let stored = assemble([("name".to_string(), Value::String("navfn".into()))]);

        assert_eq!(rows_at(&stored, "name", Element::Text), None);
        assert_eq!(rows_at(&stored, "missing", Element::Text), None);
    }

    #[test]
    fn an_empty_stored_array_is_no_rows_rather_than_no_list() {
        let stored = assemble([("footprint".to_string(), Value::Array(Vec::new()))]);

        assert_eq!(
            rows_at(&stored, "footprint", Element::Decimal),
            Some(vec![])
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rw_core::schema::FieldDef;

    fn field(name: &str, field_type: FieldType) -> FieldDef {
        FieldDef {
            name: name.into(),
            field_type,
            default: None,
            comment: None,
        }
    }

    fn message(fields: Vec<FieldDef>) -> MessageDef {
        MessageDef {
            fields,
            constants: Vec::new(),
        }
    }

    #[test]
    fn primitives_pick_the_editor_that_matches_them() {
        assert_eq!(
            editor_for(&FieldType::Primitive(PrimitiveType::Bool)),
            Editor::Bool
        );
        assert_eq!(
            editor_for(&FieldType::Primitive(PrimitiveType::Int32)),
            Editor::Integer
        );
        assert_eq!(
            editor_for(&FieldType::Primitive(PrimitiveType::Float64)),
            Editor::Decimal
        );
        assert_eq!(editor_for(&FieldType::String { bound: None }), Editor::Text);
        assert_eq!(editor_for(&FieldType::Time), Editor::Time);
    }

    #[test]
    fn an_array_of_primitives_edits_as_a_list() {
        let field_type = FieldType::Array {
            element: Box::new(FieldType::Primitive(PrimitiveType::Float64)),
            length: ArrayLength::Unbounded,
        };
        assert_eq!(editor_for(&field_type), Editor::List(Element::Decimal));
    }

    #[test]
    fn an_array_of_messages_falls_back_to_text() {
        // There is no honest single-line spelling for a list of structs, so it
        // is left alone rather than half-supported.
        let field_type = FieldType::Array {
            element: Box::new(FieldType::Complex {
                type_name: "geometry_msgs/Point".into(),
            }),
            length: ArrayLength::Unbounded,
        };
        assert_eq!(editor_for(&field_type), Editor::Text);
    }

    #[test]
    fn type_names_read_the_way_a_schema_author_writes_them() {
        assert_eq!(
            describe(&FieldType::Array {
                element: Box::new(FieldType::Primitive(PrimitiveType::Uint8)),
                length: ArrayLength::Fixed(3),
            }),
            "uint8[3]"
        );
        assert_eq!(
            describe(&FieldType::Array {
                element: Box::new(FieldType::String { bound: None }),
                length: ArrayLength::Bounded(4),
            }),
            "string[<=4]"
        );
        assert_eq!(
            describe(&FieldType::Array {
                element: Box::new(FieldType::Primitive(PrimitiveType::Int32)),
                length: ArrayLength::Unbounded,
            }),
            "int32[]"
        );
    }

    #[test]
    fn an_empty_field_means_leave_it_at_its_default() {
        assert_eq!(parse(Editor::Integer, "   "), Ok(None));
        assert_eq!(parse(Editor::Text, ""), Ok(None));
    }

    #[test]
    fn scalars_parse_to_their_own_kind() {
        assert_eq!(parse(Editor::Integer, "-4"), Ok(Some(Value::Int(-4))));
        assert_eq!(parse(Editor::Decimal, "1.5"), Ok(Some(Value::F64(1.5))));
        assert_eq!(parse(Editor::Bool, "TRUE"), Ok(Some(Value::Bool(true))));
        assert_eq!(parse(Editor::Bool, "no"), Ok(Some(Value::Bool(false))));
        assert_eq!(
            parse(Editor::Text, " hello "),
            Ok(Some(Value::String("hello".into())))
        );
    }

    #[test]
    fn time_takes_seconds_and_optional_nanoseconds() {
        assert_eq!(
            parse(Editor::Time, "12.500"),
            Ok(Some(Value::Time {
                sec: 12,
                nanosec: 500
            }))
        );
        assert_eq!(
            parse(Editor::Time, "7"),
            Ok(Some(Value::Time { sec: 7, nanosec: 0 }))
        );
    }

    #[test]
    fn lists_split_on_commas_and_ignore_the_gaps() {
        assert_eq!(
            parse(Editor::List(Element::Integer), "1, 2 ,3, "),
            Ok(Some(Value::Array(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3)
            ])))
        );
    }

    #[test]
    fn a_bad_entry_names_itself() {
        let error = parse(Editor::Integer, "twelve").expect_err("not a number");
        assert!(error.contains("twelve"), "{error}");
        let error = parse(Editor::List(Element::Decimal), "1, two").expect_err("not a number");
        assert!(error.contains("two"), "{error}");
        let error = parse(Editor::Bool, "maybe").expect_err("not a bool");
        assert!(error.contains("maybe"), "{error}");
    }

    #[test]
    fn dotted_paths_rebuild_the_nesting() {
        let value = assemble([
            ("pose.position.x".to_string(), Value::F64(1.0)),
            ("pose.position.y".to_string(), Value::F64(2.0)),
            ("header.frame_id".to_string(), Value::String("map".into())),
        ]);

        let Value::Struct(root) = &value else {
            panic!("expected a struct, got {value:?}");
        };
        assert_eq!(root.len(), 2);
        assert_eq!(
            value,
            Value::Struct(BTreeMap::from([
                (
                    "header".to_string(),
                    Value::Struct(BTreeMap::from([(
                        "frame_id".to_string(),
                        Value::String("map".into())
                    )]))
                ),
                (
                    "pose".to_string(),
                    Value::Struct(BTreeMap::from([(
                        "position".to_string(),
                        Value::Struct(BTreeMap::from([
                            ("x".to_string(), Value::F64(1.0)),
                            ("y".to_string(), Value::F64(2.0)),
                        ]))
                    )]))
                ),
            ]))
        );
    }

    #[test]
    fn a_saved_value_reads_back_into_the_form() {
        let value = assemble([
            ("pose.position.x".to_string(), Value::F64(1.5)),
            ("name".to_string(), Value::String("arm".into())),
            (
                "joints".to_string(),
                Value::Array(vec![Value::F64(0.1), Value::F64(0.2)]),
            ),
        ]);

        assert_eq!(
            text_at(&value, "pose.position.x", Editor::Decimal),
            Some("1.5".to_string())
        );
        assert_eq!(
            text_at(&value, "name", Editor::Text),
            Some("arm".to_string())
        );
        assert_eq!(
            text_at(&value, "joints", Editor::List(Element::Decimal)),
            Some("0.1, 0.2".to_string())
        );
    }

    #[test]
    fn a_leaf_that_is_not_there_reads_as_nothing() {
        let value = assemble([("a".to_string(), Value::Int(1))]);
        assert_eq!(text_at(&value, "b", Editor::Integer), None);
        // Walking *through* a leaf is not a path either.
        assert_eq!(text_at(&value, "a.b", Editor::Integer), None);
    }

    #[test]
    fn text_and_parse_are_inverses_for_every_editor() {
        for (editor, text) in [
            (Editor::Text, "hello"),
            (Editor::Integer, "-12"),
            (Editor::Decimal, "1.25"),
            (Editor::Bool, "true"),
            (Editor::Time, "3.400"),
            (Editor::List(Element::Integer), "1, 2, 3"),
        ] {
            let value = parse(editor, text)
                .unwrap_or_else(|error| panic!("{text:?} should parse: {error}"))
                .expect("not empty");
            let assembled = assemble([("leaf".to_string(), value)]);
            let back = text_at(&assembled, "leaf", editor).expect("the leaf is there");
            let reparsed = parse(editor, &back).expect("round trip parses");
            assert_eq!(
                reparsed,
                parse(editor, text).expect("original parses"),
                "{editor:?} did not round trip through {back:?}"
            );
        }
    }

    #[test]
    fn assembling_nothing_yields_an_empty_message() {
        assert_eq!(assemble([]), Value::Struct(BTreeMap::new()));
    }

    /// A lookup that knows nothing, for the cases that do not need one.
    fn nothing(_: &str) -> Option<MessageDef> {
        None
    }

    #[test]
    fn a_flat_message_keeps_its_field_names() {
        let leaves = fields(
            &message(vec![
                field("a", FieldType::Primitive(PrimitiveType::Int32)),
                field("b", FieldType::String { bound: None }),
            ]),
            &nothing,
        );
        let paths: Vec<_> = leaves.iter().map(|leaf| leaf.path.as_str()).collect();
        assert_eq!(paths, ["a", "b"]);
        assert_eq!(leaves[0].type_name, "int32");
        assert_eq!(leaves[1].editor, Editor::Text);
    }

    #[test]
    fn an_unresolvable_message_stays_as_one_editable_field() {
        // Dropping it would silently lose a field the robot requires.
        let leaves = fields(
            &message(vec![field(
                "header",
                FieldType::Complex {
                    type_name: "std_msgs/Header".into(),
                },
            )]),
            &nothing,
        );
        assert_eq!(leaves.len(), 1);
        assert_eq!(leaves[0].path, "header");
        assert_eq!(leaves[0].type_name, "std_msgs/Header");
    }

    #[test]
    fn nested_messages_flatten_to_dotted_paths() {
        let point = message(vec![
            field("x", FieldType::Primitive(PrimitiveType::Float64)),
            field("y", FieldType::Primitive(PrimitiveType::Float64)),
        ]);
        let pose = message(vec![field(
            "position",
            FieldType::Complex {
                type_name: "geometry_msgs/Point".into(),
            },
        )]);
        let lookup = move |name: &str| (name == "geometry_msgs/Point").then(|| point.clone());

        let leaves = fields(&pose, &lookup);
        let paths: Vec<_> = leaves.iter().map(|leaf| leaf.path.as_str()).collect();
        assert_eq!(paths, ["position.x", "position.y"]);
        // The label is the leaf's own name; the path is what identifies it.
        assert_eq!(leaves[0].label, "x");
    }

    #[test]
    fn a_cycle_in_the_schemas_terminates() {
        // ROS messages cannot be recursive, but a malformed registry can say
        // they are, and that must not be a hang.
        let looping = message(vec![field(
            "next",
            FieldType::Complex {
                type_name: "loop/Node".into(),
            },
        )]);
        let lookup = move |name: &str| (name == "loop/Node").then(|| looping.clone());

        let leaves = fields(&fields_root(), &lookup);
        assert_eq!(leaves.len(), 1);
        assert_eq!(leaves[0].path.matches('.').count(), MAX_DEPTH);
    }

    fn fields_root() -> MessageDef {
        message(vec![field(
            "next",
            FieldType::Complex {
                type_name: "loop/Node".into(),
            },
        )])
    }

    /// What a saved request has to survive: fill a form, store it, and get the
    /// same text back in the same boxes.
    #[test]
    fn a_filled_form_round_trips_through_a_stored_value() {
        let leaves = vec![
            ("a".to_string(), Value::Int(20)),
            ("b".to_string(), Value::Int(22)),
            ("pose.position.x".to_string(), Value::F64(1.5)),
        ];
        let stored = assemble(leaves);

        assert_eq!(
            text_at(&stored, "a", Editor::Integer).as_deref(),
            Some("20")
        );
        assert_eq!(
            text_at(&stored, "b", Editor::Integer).as_deref(),
            Some("22")
        );
        assert_eq!(
            text_at(&stored, "pose.position.x", Editor::Decimal).as_deref(),
            Some("1.5")
        );
    }

    /// An emptied box is an absent field, and absent is what comes back — so
    /// clearing a value and saving really does clear it, rather than the old
    /// one reappearing on reload.
    #[test]
    fn a_cleared_field_stays_cleared() {
        assert_eq!(parse(Editor::Integer, ""), Ok(None));

        // The form that produced it had `a` and `b`; `b` was then emptied.
        let stored = assemble(vec![("a".to_string(), Value::Int(20))]);
        assert_eq!(
            text_at(&stored, "a", Editor::Integer).as_deref(),
            Some("20")
        );
        assert_eq!(text_at(&stored, "b", Editor::Integer), None);
    }

    /// A field the schema no longer has is simply not found, rather than
    /// poisoning the rest of the form.
    #[test]
    fn a_path_the_schema_dropped_is_not_found() {
        let stored = assemble(vec![("kept".to_string(), Value::Bool(true))]);
        assert_eq!(text_at(&stored, "gone", Editor::Text), None);
        assert_eq!(text_at(&stored, "kept.deeper", Editor::Text), None);
        assert_eq!(
            text_at(&stored, "kept", Editor::Bool).as_deref(),
            Some("true")
        );
    }

    /// A list round-trips as its rows, which is how the per-element editor
    /// re-fills itself.
    #[test]
    fn a_list_round_trips_as_rows() {
        let stored = assemble(vec![(
            "footprint".to_string(),
            Value::Array(vec![Value::F64(0.2), Value::F64(-0.2)]),
        )]);
        assert_eq!(
            rows_at(&stored, "footprint", Element::Decimal),
            Some(vec!["0.2".to_string(), "-0.2".to_string()])
        );
    }
}
