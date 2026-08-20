#![deny(missing_debug_implementations)]

mod params;
pub mod world;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use rw_canonical::{
    canonical_schema_id, ArrayLength, CanonicalSchema, CanonicalValue, Dialect, FieldDef,
    FieldType, MessageDef, ParsedSchema, PrimitiveType, SchemaKind, VisualizationRole,
};
use rw_transport::{
    ActionCancelToken, ActionGoalStream, ConnectionStatus, Discovery, Frame, Subscription,
    TargetDescriptor, TopicDescriptor, Transport, TransportError, TransportResult,
};
use tokio::sync::{mpsc, oneshot, watch, Mutex};

const ADD_TWO_INTS: &str = "/dummy/add_two_ints";
const ADD_TWO_INTS_SCHEMA: &str = "example_interfaces/AddTwoInts";
const ADD_TWO_INTS_DEF: &str = "int64 a\nint64 b\n---\nint64 sum\n";

/// The world topics. Named as a real graph names them — `/tf` is subscribed by
/// name, so a simulator spelling it `/dummy/tf` would exercise nothing.
const TF: &str = "/tf";
const TF_STATIC: &str = "/tf_static";
const SCAN: &str = "/scan";
const PATH: &str = "/path";
const POSE: &str = "/pose";
const ROSOUT: &str = "/rosout";

const TF_DEF: &str = "geometry_msgs/TransformStamped[] transforms\n";
const SCAN_DEF: &str = "std_msgs/Header header\nfloat32 angle_min\nfloat32 angle_max\nfloat32 angle_increment\nfloat32 time_increment\nfloat32 scan_time\nfloat32 range_min\nfloat32 range_max\nfloat32[] ranges\nfloat32[] intensities\n";
const PATH_DEF: &str = "std_msgs/Header header\ngeometry_msgs/PoseStamped[] poses\n";
const POSE_DEF: &str = "std_msgs/Header header\ngeometry_msgs/Pose pose\n";
const ROSOUT_DEF: &str = "builtin_interfaces/Time stamp\nuint8 level\nstring name\nstring msg\nstring file\nstring function\nuint32 line\n";

/// The services a node answers for its parameters. Named exactly as ROS 2
/// names them, because that is how the editor finds nodes that have any.
const LIST_PARAMETERS: &str = "list_parameters";
const GET_PARAMETERS: &str = "get_parameters";
const SET_PARAMETERS: &str = "set_parameters";

const FIBONACCI: &str = "/dummy/fibonacci";
const FIBONACCI_SCHEMA: &str = "example_interfaces/Fibonacci";
const FIBONACCI_DEF: &str = "int32 order\n---\nint32[] sequence\n---\nint32[] sequence\n";

#[cfg(not(target_family = "wasm"))]
use tokio::task::JoinHandle;
#[cfg(target_family = "wasm")]
use wasm_bindgen_futures as _;

#[cfg(not(target_family = "wasm"))]
type SpawnedTask = JoinHandle<()>;
#[cfg(target_family = "wasm")]
type SpawnedTask = ();

#[cfg(not(target_family = "wasm"))]
fn spawn_task<F>(future: F) -> SpawnedTask
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(future)
}

#[cfg(target_family = "wasm")]
fn spawn_task<F>(future: F) -> SpawnedTask
where
    F: std::future::Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(future);
}

async fn sleep_ms(ms: u64) {
    #[cfg(not(target_family = "wasm"))]
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    #[cfg(target_family = "wasm")]
    gloo_timers::future::TimeoutFuture::new(ms.min(i32::MAX as u64) as u32).await;
}

#[derive(Debug)]
pub struct DummyTransport {
    inner: Arc<Inner>,
}

struct Inner {
    status_tx: watch::Sender<ConnectionStatus>,
    status_rx: watch::Receiver<ConnectionStatus>,
    discovery_tx: watch::Sender<Discovery>,
    discovery_rx: watch::Receiver<Discovery>,
    schemas: HashMap<String, Arc<CanonicalSchema>>,
    subscribers: Mutex<HashMap<String, Vec<mpsc::Sender<Frame>>>>,
    publisher: Mutex<Option<SpawnedTask>>,
    /// The parameters `/dummy/planner` declares. Held here rather than rebuilt
    /// per call so a value that was written stays written.
    params: params::Params,
    /// The simulated clock, in ticks of 100 ms.
    ///
    /// Every message is stamped from it, which is what lets the transform
    /// buffer interpolate: a tree of transforms all stamped zero can only ever
    /// answer for time zero.
    tick: AtomicI64,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DummyInner").finish_non_exhaustive()
    }
}

impl Default for DummyTransport {
    fn default() -> Self {
        DummyTransport::new()
    }
}

impl DummyTransport {
    pub fn new() -> Self {
        let counter_schema = build_schema("std_msgs/Int64", "int64 data\n", PrimitiveType::Int64);
        let string_schema = build_string_schema();
        let wave_schema =
            build_schema("std_msgs/Float64", "float64 data\n", PrimitiveType::Float64);
        let image_schema = build_image_schema();
        let markers_schema = build_markers_schema();
        let points_schema = build_points_schema();
        let tf_schema = build_tf_schema();
        let scan_schema = build_scan_schema();
        let path_schema = build_path_schema();
        let pose_schema = build_pose_schema();
        let rosout_schema = build_rosout_schema();

        let mut schemas = HashMap::new();
        schemas.insert("/dummy/counter".to_string(), counter_schema.clone());
        schemas.insert("/dummy/string".to_string(), string_schema.clone());
        schemas.insert("/dummy/wave".to_string(), wave_schema.clone());
        schemas.insert("/dummy/image".to_string(), image_schema.clone());
        schemas.insert("/dummy/markers".to_string(), markers_schema.clone());
        schemas.insert("/dummy/points".to_string(), points_schema.clone());
        schemas.insert(TF.to_string(), tf_schema.clone());
        schemas.insert(TF_STATIC.to_string(), tf_schema.clone());
        schemas.insert(SCAN.to_string(), scan_schema.clone());
        schemas.insert(PATH.to_string(), path_schema.clone());
        schemas.insert(POSE.to_string(), pose_schema.clone());
        schemas.insert(ROSOUT.to_string(), rosout_schema.clone());

        let discovery = Discovery {
            topics: vec![
                TopicDescriptor {
                    name: "/dummy/counter".into(),
                    schema_name: counter_schema.name.clone(),
                    schema_id: Some(counter_schema.id.clone()),
                    schema_definition: Some(counter_schema.definition.clone()),
                },
                TopicDescriptor {
                    name: "/dummy/string".into(),
                    schema_name: string_schema.name.clone(),
                    schema_id: Some(string_schema.id.clone()),
                    schema_definition: Some(string_schema.definition.clone()),
                },
                TopicDescriptor {
                    name: "/dummy/wave".into(),
                    schema_name: wave_schema.name.clone(),
                    schema_id: Some(wave_schema.id.clone()),
                    schema_definition: Some(wave_schema.definition.clone()),
                },
                TopicDescriptor {
                    name: "/dummy/image".into(),
                    schema_name: image_schema.name.clone(),
                    schema_id: Some(image_schema.id.clone()),
                    schema_definition: Some(image_schema.definition.clone()),
                },
                TopicDescriptor {
                    name: "/dummy/markers".into(),
                    schema_name: markers_schema.name.clone(),
                    schema_id: Some(markers_schema.id.clone()),
                    schema_definition: Some(markers_schema.definition.clone()),
                },
                TopicDescriptor {
                    name: "/dummy/points".into(),
                    schema_name: points_schema.name.clone(),
                    schema_id: Some(points_schema.id.clone()),
                    schema_definition: Some(points_schema.definition.clone()),
                },
                // The world topics keep their real names rather than a
                // `/dummy/` prefix: `/tf` is subscribed by name, and a
                // simulator that spelled it differently would exercise nothing.
                advertise(TF, &tf_schema),
                advertise(TF_STATIC, &tf_schema),
                advertise(SCAN, &scan_schema),
                advertise(PATH, &path_schema),
                advertise(POSE, &pose_schema),
                advertise(ROSOUT, &rosout_schema),
            ],
            services: vec![
                TargetDescriptor {
                    name: ADD_TWO_INTS.into(),
                    schema_name: ADD_TWO_INTS_SCHEMA.into(),
                    schema_id: Some(canonical_schema_id(ADD_TWO_INTS_DEF)),
                    schema_definition: Some(ADD_TWO_INTS_DEF.into()),
                },
                // No definitions: a node advertises the parameter services by
                // type and nothing reads a schema for them — the editor learns
                // what the node has by asking it, which is the only way, since
                // two nodes of the same type can declare different parameters.
                parameter_service(LIST_PARAMETERS, "rcl_interfaces/srv/ListParameters"),
                parameter_service(GET_PARAMETERS, "rcl_interfaces/srv/GetParameters"),
                parameter_service(SET_PARAMETERS, "rcl_interfaces/srv/SetParameters"),
            ],
            actions: vec![TargetDescriptor {
                name: FIBONACCI.into(),
                schema_name: FIBONACCI_SCHEMA.into(),
                schema_id: Some(canonical_schema_id(FIBONACCI_DEF)),
                schema_definition: Some(FIBONACCI_DEF.into()),
            }],
            ..Default::default()
        };

        let (status_tx, status_rx) = watch::channel(ConnectionStatus::Disconnected);
        let (discovery_tx, discovery_rx) = watch::channel(discovery);

        DummyTransport {
            inner: Arc::new(Inner {
                status_tx,
                status_rx,
                discovery_tx,
                discovery_rx,
                schemas,
                subscribers: Mutex::new(HashMap::new()),
                publisher: Mutex::new(None),
                params: params::Params::new(),
                tick: AtomicI64::new(0),
            }),
        }
    }
}

/// One of the three services the parameter node answers.
fn parameter_service(which: &str, schema: &str) -> TargetDescriptor {
    TargetDescriptor {
        name: format!("{}/{which}", params::NODE),
        schema_name: schema.into(),
        schema_id: None,
        schema_definition: None,
    }
}

fn build_schema(name: &str, definition: &str, prim: PrimitiveType) -> Arc<CanonicalSchema> {
    let parsed = ParsedSchema::Message(MessageDef {
        fields: vec![FieldDef {
            name: "data".into(),
            field_type: FieldType::Primitive(prim),
            default: None,
            comment: None,
        }],
        constants: vec![],
    });
    Arc::new(CanonicalSchema {
        id: canonical_schema_id(definition),
        name: name.into(),
        kind: SchemaKind::Message,
        dialect: Dialect::Custom("dummy".into()),
        definition: definition.into(),
        parsed,
        dependencies: vec![],
        viz_role: VisualizationRole::default(),
    })
}

fn build_string_schema() -> Arc<CanonicalSchema> {
    let parsed = ParsedSchema::Message(MessageDef {
        fields: vec![FieldDef {
            name: "data".into(),
            field_type: FieldType::String { bound: None },
            default: None,
            comment: None,
        }],
        constants: vec![],
    });
    Arc::new(CanonicalSchema {
        id: canonical_schema_id("string data\n"),
        name: "std_msgs/String".into(),
        kind: SchemaKind::Message,
        dialect: Dialect::Custom("dummy".into()),
        definition: "string data\n".into(),
        parsed,
        dependencies: vec![],
        viz_role: VisualizationRole::Text,
    })
}

const IMAGE_DEF: &str = "std_msgs/Header header\nuint32 height\nuint32 width\nstring encoding\nuint8 is_bigendian\nuint32 step\nuint8[] data\n";
const MARKERS_DEF: &str = "visualization_msgs/Marker[] markers\n";
const POINTS_DEF: &str = "std_msgs/Header header\nuint32 height\nuint32 width\nsensor_msgs/PointField[] fields\nbool is_bigendian\nuint32 point_step\nuint32 row_step\nuint8[] data\nbool is_dense\n";

fn build_image_schema() -> Arc<CanonicalSchema> {
    let fields = vec![
        primitive_field("height", PrimitiveType::Uint32),
        primitive_field("width", PrimitiveType::Uint32),
        FieldDef {
            name: "encoding".into(),
            field_type: FieldType::String { bound: None },
            default: None,
            comment: None,
        },
        primitive_field("is_bigendian", PrimitiveType::Uint8),
        primitive_field("step", PrimitiveType::Uint32),
        FieldDef {
            name: "data".into(),
            field_type: FieldType::Array {
                element: Box::new(FieldType::Primitive(PrimitiveType::Uint8)),
                length: ArrayLength::Unbounded,
            },
            default: None,
            comment: None,
        },
    ];
    build_viz_schema(
        "sensor_msgs/Image",
        IMAGE_DEF,
        fields,
        VisualizationRole::Image,
    )
}

fn build_points_schema() -> Arc<CanonicalSchema> {
    let fields = vec![
        primitive_field("height", PrimitiveType::Uint32),
        primitive_field("width", PrimitiveType::Uint32),
        FieldDef {
            name: "fields".into(),
            field_type: FieldType::Array {
                element: Box::new(FieldType::Complex {
                    type_name: "sensor_msgs/PointField".into(),
                }),
                length: ArrayLength::Unbounded,
            },
            default: None,
            comment: None,
        },
        primitive_field("is_bigendian", PrimitiveType::Bool),
        primitive_field("point_step", PrimitiveType::Uint32),
        primitive_field("row_step", PrimitiveType::Uint32),
        FieldDef {
            name: "data".into(),
            field_type: FieldType::Array {
                element: Box::new(FieldType::Primitive(PrimitiveType::Uint8)),
                length: ArrayLength::Unbounded,
            },
            default: None,
            comment: None,
        },
        primitive_field("is_dense", PrimitiveType::Bool),
    ];
    build_viz_schema(
        "sensor_msgs/PointCloud2",
        POINTS_DEF,
        fields,
        VisualizationRole::PointCloud2,
    )
}

fn build_markers_schema() -> Arc<CanonicalSchema> {
    let fields = vec![FieldDef {
        name: "markers".into(),
        field_type: FieldType::Array {
            element: Box::new(FieldType::Complex {
                type_name: "visualization_msgs/Marker".into(),
            }),
            length: ArrayLength::Unbounded,
        },
        default: None,
        comment: None,
    }];
    build_viz_schema(
        "visualization_msgs/MarkerArray",
        MARKERS_DEF,
        fields,
        VisualizationRole::MarkerArray,
    )
}

fn build_tf_schema() -> Arc<CanonicalSchema> {
    build_viz_schema(
        "tf2_msgs/TFMessage",
        TF_DEF,
        vec![complex_array(
            "transforms",
            "geometry_msgs/TransformStamped",
        )],
        VisualizationRole::Tf,
    )
}

fn build_scan_schema() -> Arc<CanonicalSchema> {
    let mut fields = vec![complex_field("header", "std_msgs/Header")];
    for name in [
        "angle_min",
        "angle_max",
        "angle_increment",
        "time_increment",
        "scan_time",
        "range_min",
        "range_max",
    ] {
        fields.push(primitive_field(name, PrimitiveType::Float32));
    }
    for name in ["ranges", "intensities"] {
        fields.push(FieldDef {
            name: name.into(),
            field_type: FieldType::Array {
                element: Box::new(FieldType::Primitive(PrimitiveType::Float32)),
                length: ArrayLength::Unbounded,
            },
            default: None,
            comment: None,
        });
    }
    build_viz_schema(
        "sensor_msgs/LaserScan",
        SCAN_DEF,
        fields,
        VisualizationRole::LaserScan,
    )
}

fn build_path_schema() -> Arc<CanonicalSchema> {
    build_viz_schema(
        "nav_msgs/Path",
        PATH_DEF,
        vec![
            complex_field("header", "std_msgs/Header"),
            complex_array("poses", "geometry_msgs/PoseStamped"),
        ],
        VisualizationRole::Path,
    )
}

fn build_pose_schema() -> Arc<CanonicalSchema> {
    build_viz_schema(
        "geometry_msgs/PoseStamped",
        POSE_DEF,
        vec![
            complex_field("header", "std_msgs/Header"),
            complex_field("pose", "geometry_msgs/Pose"),
        ],
        VisualizationRole::PoseStamped,
    )
}

fn build_rosout_schema() -> Arc<CanonicalSchema> {
    let mut fields = vec![
        complex_field("stamp", "builtin_interfaces/Time"),
        primitive_field("level", PrimitiveType::Uint8),
    ];
    for name in ["name", "msg", "file", "function"] {
        fields.push(FieldDef {
            name: name.into(),
            field_type: FieldType::String { bound: None },
            default: None,
            comment: None,
        });
    }
    fields.push(primitive_field("line", PrimitiveType::Uint32));
    build_viz_schema(
        "rcl_interfaces/Log",
        ROSOUT_DEF,
        fields,
        VisualizationRole::default(),
    )
}

fn complex_field(name: &str, type_name: &str) -> FieldDef {
    FieldDef {
        name: name.into(),
        field_type: FieldType::Complex {
            type_name: type_name.into(),
        },
        default: None,
        comment: None,
    }
}

fn complex_array(name: &str, type_name: &str) -> FieldDef {
    FieldDef {
        name: name.into(),
        field_type: FieldType::Array {
            element: Box::new(FieldType::Complex {
                type_name: type_name.into(),
            }),
            length: ArrayLength::Unbounded,
        },
        default: None,
        comment: None,
    }
}

/// One entry for the discovery list.
fn advertise(name: &str, schema: &Arc<CanonicalSchema>) -> TopicDescriptor {
    TopicDescriptor {
        name: name.into(),
        schema_name: schema.name.clone(),
        schema_id: Some(schema.id.clone()),
        schema_definition: Some(schema.definition.clone()),
    }
}

fn primitive_field(name: &str, prim: PrimitiveType) -> FieldDef {
    FieldDef {
        name: name.into(),
        field_type: FieldType::Primitive(prim),
        default: None,
        comment: None,
    }
}

fn build_viz_schema(
    name: &str,
    definition: &str,
    fields: Vec<FieldDef>,
    viz_role: VisualizationRole,
) -> Arc<CanonicalSchema> {
    let parsed = ParsedSchema::Message(MessageDef {
        fields,
        constants: vec![],
    });
    Arc::new(CanonicalSchema {
        id: canonical_schema_id(definition),
        name: name.into(),
        kind: SchemaKind::Message,
        dialect: Dialect::Custom("dummy".into()),
        definition: definition.into(),
        parsed,
        dependencies: vec![],
        viz_role,
    })
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl Transport for DummyTransport {
    async fn connect(&self) -> TransportResult<()> {
        let _ = self.inner.status_tx.send(ConnectionStatus::Connecting);
        let _ = self.inner.status_tx.send(ConnectionStatus::Connected);
        let mut publisher = self.inner.publisher.lock().await;
        if publisher.is_some() {
            return Ok(());
        }
        let inner = self.inner.clone();
        let task = spawn_task(async move {
            loop {
                sleep_ms(100).await;
                let tick = inner.tick.fetch_add(1, Ordering::Relaxed) + 1;
                publish_tick(&inner, tick).await;
            }
        });
        *publisher = Some(task);
        Ok(())
    }

    async fn disconnect(&self) -> TransportResult<()> {
        let mut publisher = self.inner.publisher.lock().await;
        #[cfg(not(target_family = "wasm"))]
        if let Some(handle) = publisher.take() {
            handle.abort();
        }
        #[cfg(target_family = "wasm")]
        {
            *publisher = None;
        }
        self.inner.subscribers.lock().await.clear();
        let _ = self.inner.status_tx.send(ConnectionStatus::Disconnected);
        Ok(())
    }

    fn status(&self) -> watch::Receiver<ConnectionStatus> {
        self.inner.status_rx.clone()
    }

    fn discovery(&self) -> watch::Receiver<Discovery> {
        self.inner.discovery_rx.clone()
    }

    async fn subscribe_topic(&self, topic: &str) -> TransportResult<Subscription> {
        let schema = self
            .inner
            .schemas
            .get(topic)
            .cloned()
            .ok_or_else(|| TransportError::Other(format!("unknown dummy topic {topic}")))?;
        let (sender, receiver) = mpsc::channel(64);
        self.inner
            .subscribers
            .lock()
            .await
            .entry(topic.to_string())
            .or_insert_with(Vec::new)
            .push(sender);
        Ok(Subscription {
            frames: receiver,
            schema,
        })
    }

    /// Accepts a publish and delivers it straight back to that topic's
    /// subscribers.
    ///
    /// A loopback rather than a no-op: publishing into a system with no robot in
    /// it should still be something you can watch happen, and it makes the whole
    /// publish path testable without one.
    async fn publish(&self, topic: &str, value: CanonicalValue) -> TransportResult<()> {
        publish_one(&self.inner, topic, value).await;
        Ok(())
    }

    async fn call_service(
        &self,
        service: &str,
        request: CanonicalValue,
    ) -> TransportResult<CanonicalValue> {
        if let Some(which) = service.strip_prefix(&format!("{}/", params::NODE)) {
            return match which {
                LIST_PARAMETERS => Ok(self.inner.params.list()),
                GET_PARAMETERS => Ok(self.inner.params.get(&request)),
                SET_PARAMETERS => Ok(self.inner.params.set(&request)),
                _ => Err(TransportError::Other(format!(
                    "unknown dummy service {service}"
                ))),
            };
        }
        if service != ADD_TWO_INTS {
            return Err(TransportError::Other(format!(
                "unknown dummy service {service}"
            )));
        }
        let a = int_field(&request, "a");
        let b = int_field(&request, "b");
        Ok(struct_one("sum", CanonicalValue::Int(a + b)))
    }

    async fn send_action_goal(
        &self,
        action: &str,
        goal: CanonicalValue,
    ) -> TransportResult<ActionGoalStream> {
        if action != FIBONACCI {
            return Err(TransportError::Other(format!(
                "unknown dummy action {action}"
            )));
        }
        let order = int_field(&goal, "order").clamp(0, 25);
        let (feedback_tx, feedback_rx) = mpsc::channel(16);
        let (result_tx, result_rx) = oneshot::channel();

        spawn_task(async move {
            let mut sequence: Vec<i64> = vec![0, 1];
            for _ in 0..order {
                let len = sequence.len();
                sequence.push(sequence[len - 1] + sequence[len - 2]);
                let feedback = struct_one("sequence", int_array(&sequence));
                if feedback_tx.send(feedback).await.is_err() {
                    return;
                }
                sleep_ms(150).await;
            }
            let _ = result_tx.send(Ok(struct_one("sequence", int_array(&sequence))));
        });

        Ok(ActionGoalStream {
            feedback: feedback_rx,
            result: result_rx,
            cancel_token: ActionCancelToken {
                action: action.to_string(),
                goal_id: format!("dummy-fibonacci-{order}"),
            },
        })
    }

    async fn cancel_action_goal(&self, _token: &ActionCancelToken) -> TransportResult<()> {
        Ok(())
    }
}

fn int_field(value: &CanonicalValue, key: &str) -> i64 {
    let CanonicalValue::Struct(map) = value else {
        return 0;
    };
    match map.get(key) {
        Some(CanonicalValue::Int(v)) => *v,
        Some(CanonicalValue::Uint(v)) => *v as i64,
        _ => 0,
    }
}

fn int_array(values: &[i64]) -> CanonicalValue {
    CanonicalValue::Array(values.iter().copied().map(CanonicalValue::Int).collect())
}

/// One tick of 100 ms, on the simulated clock every message is stamped from.
fn clock_ns(tick: i64) -> u64 {
    (tick.max(0) as u64).saturating_mul(100_000_000)
}

async fn publish_tick(inner: &Arc<Inner>, tick: i64) {
    let at_ns = clock_ns(tick);

    // The transform tree first, so a subscriber that reads the two in the order
    // they arrive can already place the scan that follows.
    publish_one(inner, TF, world::tf(tick, at_ns)).await;
    // Statics are republished rather than sent once: a pane opened a minute in
    // would otherwise never learn where the sensor is bolted, which is exactly
    // what latching solves on a real graph and what this stands in for.
    publish_one(inner, TF_STATIC, world::tf_static(at_ns)).await;
    publish_one(inner, SCAN, world::scan(tick, at_ns)).await;
    publish_one(inner, PATH, world::path(tick, at_ns)).await;
    publish_one(inner, POSE, world::pose(tick, at_ns)).await;
    if let Some(line) = world::log(tick, at_ns) {
        publish_one(inner, ROSOUT, line).await;
    }

    publish_one(
        inner,
        "/dummy/counter",
        struct_one("data", CanonicalValue::Int(tick)),
    )
    .await;
    publish_one(
        inner,
        "/dummy/string",
        struct_one("data", CanonicalValue::String(format!("frame #{tick}"))),
    )
    .await;
    let v = (tick as f64 / 10.0).sin();
    publish_one(
        inner,
        "/dummy/wave",
        struct_one("data", CanonicalValue::F64(v)),
    )
    .await;
    publish_one(inner, "/dummy/image", make_image_value(tick, 96, 64)).await;
    publish_one(inner, "/dummy/markers", make_markers_value(tick)).await;
    publish_one(inner, "/dummy/points", world::cloud(tick, at_ns)).await;
}

fn make_image_value(tick: i64, width: u32, height: u32) -> CanonicalValue {
    let shift = (tick.wrapping_mul(4) & 0xff) as u32;
    let mut data = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            data.push(((x * 255 / width + shift) % 256) as u8);
            data.push((y * 255 / height % 256) as u8);
            data.push((shift % 256) as u8);
        }
    }
    let mut map = BTreeMap::new();
    map.insert("height".into(), CanonicalValue::Uint(height as u64));
    map.insert("width".into(), CanonicalValue::Uint(width as u64));
    map.insert("encoding".into(), CanonicalValue::String("rgb8".into()));
    map.insert("is_bigendian".into(), CanonicalValue::Uint(0));
    map.insert("step".into(), CanonicalValue::Uint((width * 3) as u64));
    map.insert("data".into(), CanonicalValue::Bytes(data));
    CanonicalValue::Struct(map)
}

fn make_markers_value(tick: i64) -> CanonicalValue {
    const COUNT: usize = 240;
    let phase = tick as f64 * 0.05;
    let mut points = Vec::with_capacity(COUNT);
    for i in 0..COUNT {
        let t = i as f64 / COUNT as f64 * std::f64::consts::TAU;
        let radius = 1.0 + 0.3 * (3.0 * t + phase).sin();
        let mut point = BTreeMap::new();
        point.insert("x".into(), CanonicalValue::F64(radius * t.cos()));
        point.insert("y".into(), CanonicalValue::F64(radius * t.sin()));
        point.insert(
            "z".into(),
            CanonicalValue::F64(0.3 * (2.0 * t + phase).sin()),
        );
        points.push(CanonicalValue::Struct(point));
    }
    let mut color = BTreeMap::new();
    color.insert("r".into(), CanonicalValue::F64(0.92));
    color.insert("g".into(), CanonicalValue::F64(0.28));
    color.insert("b".into(), CanonicalValue::F64(0.6));
    color.insert("a".into(), CanonicalValue::F64(1.0));
    let mut marker = BTreeMap::new();
    marker.insert("points".into(), CanonicalValue::Array(points));
    marker.insert("color".into(), CanonicalValue::Struct(color));
    struct_one(
        "markers",
        CanonicalValue::Array(vec![CanonicalValue::Struct(marker)]),
    )
}

fn struct_one(field: &str, value: CanonicalValue) -> CanonicalValue {
    let mut map = BTreeMap::new();
    map.insert(field.to_string(), value);
    CanonicalValue::Struct(map)
}

async fn publish_one(inner: &Arc<Inner>, topic: &str, value: CanonicalValue) {
    let schema = match inner.schemas.get(topic).cloned() {
        Some(s) => s,
        None => return,
    };
    let frame = Frame {
        timestamp_ns: clock_ns(inner.tick.load(Ordering::Relaxed)),
        schema,
        value,
        raw: None,
        perf: None,
    };
    let mut subs = inner.subscribers.lock().await;
    let Some(slot) = subs.get_mut(topic) else {
        return;
    };
    slot.retain(|sender| sender.try_send(frame.clone()).is_ok() || !sender.is_closed());
    let snapshot = inner.discovery_rx.borrow().clone();
    let _ = inner.discovery_tx.send(snapshot);
}
