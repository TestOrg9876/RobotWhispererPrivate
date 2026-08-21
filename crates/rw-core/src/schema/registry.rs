use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::domain::SchemaRef;
use crate::schema::{hash, parser, ParsedSchema, SchemaDefinition, SchemaKind};
use crate::storage::Storage;
use crate::{CoreError, CoreResult};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchemaSummary {
    pub name: String,
    pub hash: String,
    pub kind: SchemaKind,
    pub dependency_count: usize,
}

pub struct SchemaRegistry {
    storage: Arc<dyn Storage>,
    cache: RwLock<RegistryCache>,
}

#[derive(Default)]
struct RegistryCache {
    by_hash: BTreeMap<String, SchemaDefinition>,
    by_name: BTreeMap<String, Vec<String>>,
}

impl SchemaRegistry {
    pub async fn new(storage: Arc<dyn Storage>) -> CoreResult<Self> {
        let registry = Self {
            storage,
            cache: RwLock::new(RegistryCache::default()),
        };
        registry.refresh().await?;
        Ok(registry)
    }

    pub async fn refresh(&self) -> CoreResult<()> {
        let definitions = self.storage.list_schemas().await?;
        let mut cache = self.cache.write().expect("schema cache poisoned");
        cache.by_hash.clear();
        cache.by_name.clear();
        for definition in definitions {
            install_into_cache(&mut cache, definition);
        }
        Ok(())
    }

    pub async fn ensure_defaults(&self) -> CoreResult<()> {
        super::defaults::install_into(self).await
    }

    pub async fn register(
        &self,
        name: &str,
        kind: SchemaKind,
        definition: &str,
    ) -> CoreResult<SchemaRef> {
        let package = name.split('/').next().filter(|segment| !segment.is_empty());
        let parsed = parser::parse_with_package(kind, definition, package)?;
        let dependencies = parser::collect_dependencies(&parsed);

        // What this definition's nested types mean, resolved the publisher's
        // way first.
        //
        // A concatenated definition carries the sender's own copy of every type
        // it references, and those are the only correct answer for it: a ROS 1
        // robot's `Header` has `seq` and a ROS 2 one does not, and the registry
        // holds both the moment two robots are connected at once. Falling back
        // to the cache — where a name maps to several hashes and this took
        // whichever sorted first — is for definitions that arrived with no
        // sections at all, which is the only case where there is nothing better
        // to go on.
        let mut resolved: BTreeMap<String, String> = {
            let cache = self.cache.read().expect("schema cache poisoned");
            cache
                .by_name
                .iter()
                .filter_map(|(name, hashes)| {
                    let hash = hashes.first()?;
                    let entry = cache.by_hash.get(hash)?;
                    Some((name.clone(), entry.definition.clone()))
                })
                .collect()
        };
        let (_, parts) = parser::split_bundle(definition);
        for part in &parts {
            resolved.insert(part.name.to_string(), part.body.to_string());
        }

        let hash_value =
            hash::canonical_hash_with_package(kind, definition, package, |dep_name| {
                resolved.get(dep_name).map(String::as_str)
            })?;

        let dependency_set = collect_transitive_dependencies(&dependencies, &resolved)?;

        let entry = SchemaDefinition {
            name: name.to_string(),
            kind,
            hash: hash_value.clone(),
            definition: definition.to_string(),
            parsed,
            dependencies: dependency_set,
        };

        self.storage.put_schema(&entry).await?;
        let mut cache = self.cache.write().expect("schema cache poisoned");
        install_into_cache(&mut cache, entry);

        Ok(SchemaRef {
            name: name.to_string(),
            hash: hash_value,
        })
    }

    pub fn get_by_hash(&self, hash: &str) -> Option<SchemaDefinition> {
        let cache = self.cache.read().expect("schema cache poisoned");
        cache.by_hash.get(hash).cloned()
    }

    pub fn get_by_name(&self, name: &str) -> Vec<SchemaDefinition> {
        let cache = self.cache.read().expect("schema cache poisoned");
        cache
            .by_name
            .get(name)
            .map(|hashes| {
                hashes
                    .iter()
                    .filter_map(|hash| cache.by_hash.get(hash).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn list_names(&self) -> Vec<String> {
        let cache = self.cache.read().expect("schema cache poisoned");
        cache.by_name.keys().cloned().collect()
    }

    pub fn list_summaries(&self) -> Vec<SchemaSummary> {
        let cache = self.cache.read().expect("schema cache poisoned");
        cache
            .by_hash
            .values()
            .map(|definition| SchemaSummary {
                name: definition.name.clone(),
                hash: definition.hash.clone(),
                kind: definition.kind,
                dependency_count: definition.dependencies.len(),
            })
            .collect()
    }

    pub fn require_by_name(&self, name: &str) -> CoreResult<SchemaDefinition> {
        let candidates = self.get_by_name(name);
        candidates
            .into_iter()
            .next()
            .ok_or_else(|| CoreError::NotFound(format!("schema '{name}' is not registered")))
    }

    pub fn parsed(&self, hash: &str) -> Option<ParsedSchema> {
        self.get_by_hash(hash).map(|definition| definition.parsed)
    }
}

fn install_into_cache(cache: &mut RegistryCache, definition: SchemaDefinition) {
    let hash = definition.hash.clone();
    let name = definition.name.clone();
    cache.by_hash.insert(hash.clone(), definition);
    let entries = cache.by_name.entry(name).or_default();
    if !entries.contains(&hash) {
        entries.insert(0, hash);
    }
}

fn collect_transitive_dependencies(
    direct: &[String],
    resolver: &BTreeMap<String, String>,
) -> CoreResult<Vec<String>> {
    let mut visited: BTreeMap<String, ()> = BTreeMap::new();
    let mut stack: Vec<String> = direct.to_vec();
    while let Some(name) = stack.pop() {
        if visited.insert(name.clone(), ()).is_some() {
            continue;
        }
        if let Some(text) = resolver.get(&name) {
            let dep_package = name.split('/').next().filter(|segment| !segment.is_empty());
            let parsed = parser::parse_with_package(SchemaKind::Message, text, dep_package)?;
            for nested in parser::collect_dependencies(&parsed) {
                if !visited.contains_key(&nested) {
                    stack.push(nested);
                }
            }
        }
    }
    let mut ordered: Vec<String> = visited.into_keys().collect();
    ordered.sort();
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::SqliteStorage;
    use crate::util::MockClock;
    use chrono::TimeZone;

    async fn make_registry() -> SchemaRegistry {
        let clock = Arc::new(MockClock::new(
            chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        ));
        let storage: Arc<dyn Storage> =
            Arc::new(SqliteStorage::open_in_memory(clock).expect("in-memory storage"));
        SchemaRegistry::new(storage).await.unwrap()
    }

    #[tokio::test]
    async fn registers_and_round_trips() {
        let registry = make_registry().await;
        let reference = registry
            .register(
                "std_msgs/Header",
                SchemaKind::Message,
                "uint32 seq\nbuiltin_interfaces/Time stamp\nstring frame_id\n",
            )
            .await;
        assert!(matches!(reference, Err(CoreError::Schema(_))));

        registry
            .register(
                "builtin_interfaces/Time",
                SchemaKind::Message,
                "int32 sec\nuint32 nanosec\n",
            )
            .await
            .unwrap();
        let reference = registry
            .register(
                "std_msgs/Header",
                SchemaKind::Message,
                "uint32 seq\nbuiltin_interfaces/Time stamp\nstring frame_id\n",
            )
            .await
            .unwrap();
        assert_eq!(reference.name, "std_msgs/Header");
        assert_eq!(reference.hash.len(), 64);

        let by_name = registry.get_by_name("std_msgs/Header");
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].dependencies, vec!["builtin_interfaces/Time"]);
    }

    #[tokio::test]
    async fn duplicate_registration_is_idempotent() {
        let registry = make_registry().await;
        let reference_one = registry
            .register(
                "builtin_interfaces/Time",
                SchemaKind::Message,
                "int32 sec\nuint32 nanosec\n",
            )
            .await
            .unwrap();
        let reference_two = registry
            .register(
                "builtin_interfaces/Time",
                SchemaKind::Message,
                "int32 sec\nuint32 nanosec\n",
            )
            .await
            .unwrap();
        assert_eq!(reference_one.hash, reference_two.hash);
        assert_eq!(registry.get_by_name("builtin_interfaces/Time").len(), 1);
    }

    #[tokio::test]
    async fn different_definition_under_same_name_creates_new_version() {
        let registry = make_registry().await;
        registry
            .register("custom/Type", SchemaKind::Message, "int32 a\n")
            .await
            .unwrap();
        registry
            .register("custom/Type", SchemaKind::Message, "int32 a\nint32 b\n")
            .await
            .unwrap();
        let versions = registry.get_by_name("custom/Type");
        assert_eq!(versions.len(), 2);
    }

    /// The defect this whole change exists for.
    ///
    /// Two robots are connected at once — a ROS 1 one and a ROS 2 one — and
    /// both publish a message whose header is `std_msgs/Header`. They do not
    /// mean the same thing by it: ROS 1's carries `seq` and ROS 2's does not.
    /// Each definition arrives with its own copy in its bundle, and each has to
    /// be answered with the one it brought, not with whichever landed first.
    #[tokio::test]
    async fn a_bundle_resolves_its_nested_types_against_its_own_sections() {
        // Deliberately an empty registry: a bundle carries everything it needs,
        // so it must register against nothing at all.
        let registry = make_registry().await;

        const ROS2: &str = concat!(
            "std_msgs/Header header\n",
            "float64 value\n",
            "================================\n",
            "MSG: std_msgs/Header\n",
            "builtin_interfaces/Time stamp\n",
            "string frame_id\n",
            "================================\n",
            "MSG: builtin_interfaces/Time\n",
            "int32 sec\n",
            "uint32 nanosec\n",
        );
        const ROS1: &str = concat!(
            "std_msgs/Header header\n",
            "float64 value\n",
            "================================\n",
            "MSG: std_msgs/Header\n",
            "uint32 seq\n",
            "time stamp\n",
            "string frame_id\n",
        );

        let modern = registry
            .register("pkg/Reading", SchemaKind::Message, ROS2)
            .await
            .expect("the ROS 2 definition registers");
        let legacy = registry
            .register("pkg/Reading", SchemaKind::Message, ROS1)
            .await
            .expect("the ROS 1 definition registers");

        // Same name, different meaning, so they must not collapse onto one
        // entry — the hash is what tells them apart.
        assert_ne!(
            modern.hash, legacy.hash,
            "two headers that disagree about `seq` are not the same schema"
        );
        assert_eq!(registry.get_by_name("pkg/Reading").len(), 2);

        // And each is still reachable as itself, which is what a lookup by id
        // relies on.
        assert_eq!(
            registry
                .get_by_hash(&legacy.hash)
                .expect("the ROS 1 entry is there")
                .definition,
            ROS1
        );
    }

    /// The single-definition path is unchanged: no sections, nothing to resolve
    /// against, and the cache is still what answers.
    #[tokio::test]
    async fn a_definition_with_no_sections_still_registers() {
        let registry = make_registry().await;
        let reference = registry
            .register("pkg/Plain", SchemaKind::Message, "float64 value\n")
            .await
            .expect("registers");
        assert_eq!(
            registry
                .get_by_hash(&reference.hash)
                .expect("is there")
                .name,
            "pkg/Plain"
        );
    }
}
