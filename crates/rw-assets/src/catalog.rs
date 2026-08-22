//! Finding the shipped robots, and loading one into drawable parts.
//!
//! `assets/manifest.json` lists the robots and where their descriptions are;
//! `assets/robots.config.json` adds a display name and the material presets the
//! old viewer used. Mesh paths inside a URDF are written `package://<dir>/…`,
//! which resolves against the assets directory rather than a ROS workspace.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::math::{self, Mat4};
use crate::mesh::{Mesh, Part};
use crate::urdf::{self, Geometry, Robot};
use crate::{collada, obj, shapes};

/// One robot as the manifest lists it, plus whatever the config adds.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The directory under `assets/`, and the key `robots.config.json` uses.
    pub id: String,
    /// What to call it on screen.
    pub name: String,
    pub brand: Option<String>,
    /// The description file, relative to the robot's directory.
    pub urdf: String,
    /// Degrees about x, y and z, from the config: these models were authored in
    /// several conventions and the old viewer corrected each one here.
    pub orientation: [f32; 3],
}

impl Entry {
    /// The correction that puts this model the right way up.
    pub fn correction(&self) -> Mat4 {
        math::from_rpy([
            self.orientation[0].to_radians(),
            self.orientation[1].to_radians(),
            self.orientation[2].to_radians(),
        ])
    }
}

#[derive(Debug, Deserialize)]
struct ManifestEntry {
    name: String,
    directory: String,
    urdf: String,
}

#[derive(Debug, Deserialize, Default)]
struct Config {
    #[serde(default)]
    robots: HashMap<String, ConfigEntry>,
}

#[derive(Debug, Deserialize, Default)]
struct ConfigEntry {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    brand: Option<String>,
    #[serde(default)]
    orientation: Option<[f32; 3]>,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("could not read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("{path} is not valid JSON: {source}")]
    Json {
        path: String,
        source: serde_json::Error,
    },
    #[error("no robot called `{0}`")]
    Unknown(String),
    #[error("{path}: {source}")]
    Urdf {
        path: String,
        source: urdf::UrdfError,
    },
}

/// The shipped robots, rooted at an assets directory.
#[derive(Debug, Clone)]
pub struct Catalog {
    root: PathBuf,
    entries: Vec<Entry>,
}

impl Catalog {
    /// Reads the manifest and the config from an assets directory.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, CatalogError> {
        let root = root.as_ref().to_path_buf();
        let manifest: Vec<ManifestEntry> = read_json(&root.join("manifest.json"))?;
        // A missing or broken config costs display names, not the catalog.
        let config: Config = read_json(&root.join("robots.config.json")).unwrap_or_default();

        let entries = manifest
            .into_iter()
            .map(|entry| {
                let extra = config.robots.get(&entry.directory);
                Entry {
                    name: extra
                        .and_then(|extra| extra.display_name.clone())
                        .unwrap_or(entry.name),
                    brand: extra.and_then(|extra| extra.brand.clone()),
                    orientation: extra.and_then(|extra| extra.orientation).unwrap_or([0.; 3]),
                    id: entry.directory,
                    urdf: entry.urdf,
                }
            })
            .collect();

        Ok(Self { root, entries })
    }

    /// Looks for the assets directory in the places it can actually be.
    ///
    /// `RW_ASSETS` first, so a packaged build can say outright; then beside the
    /// working directory and beside the executable, which covers running from a
    /// checkout and running an installed copy.
    pub fn discover() -> Result<Self, CatalogError> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(named) = std::env::var_os("RW_ASSETS") {
            candidates.push(PathBuf::from(named));
        }
        candidates.push(PathBuf::from("assets"));
        if let Ok(executable) = std::env::current_exe()
            && let Some(directory) = executable.parent()
        {
            candidates.push(directory.join("assets"));
            // A cargo target directory is three levels below the workspace.
            candidates.push(directory.join("../../../assets"));
        }

        let mut last = None;
        for candidate in candidates {
            match Self::open(&candidate) {
                Ok(catalog) => return Ok(catalog),
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or_else(|| CatalogError::Unknown("assets".into())))
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// Reads one robot's description and every mesh it names.
    ///
    /// A mesh that will not load leaves its link empty rather than failing the
    /// whole robot: an arm missing one cover is still an arm.
    pub fn load(&self, id: &str) -> Result<Loaded, CatalogError> {
        let entry = self
            .entry(id)
            .ok_or_else(|| CatalogError::Unknown(id.to_string()))?
            .clone();
        let path = self.root.join(&entry.id).join(&entry.urdf);
        let source = std::fs::read_to_string(&path).map_err(|source| CatalogError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let robot = urdf::parse(&source).map_err(|source| CatalogError::Urdf {
            path: path.display().to_string(),
            source,
        })?;

        Ok(self.assemble(entry, robot))
    }

    /// Loads a description the robot itself published, rather than one of ours.
    ///
    /// This is what `/robot_description` carries, and reading it is the
    /// difference between drawing the seven robots that ship with this app and
    /// drawing the one actually in front of you.
    ///
    /// The meshes are the catch. A live description names
    /// `package://my_robot/meshes/base.dae`, and that file is on the robot, not
    /// here — so a mesh resolves only when a package of that name happens to be
    /// under the assets root. Everything the description draws with primitives
    /// works regardless, which for a collision description is usually all of
    /// it. What could not be found comes back in `missing` rather than failing
    /// the load: a robot with no covers is still a robot, and its joint
    /// structure is worth seeing on its own.
    pub fn load_description(&self, name: &str, source: &str) -> Result<Loaded, CatalogError> {
        let robot = urdf::parse(source).map_err(|source| CatalogError::Urdf {
            path: name.to_string(),
            source,
        })?;

        // No orientation correction: the catalog's angles exist because those
        // models were authored in several conventions, and a description read
        // off a running robot is already in the convention its own TF tree
        // uses. Turning it would put it at odds with every frame around it.
        let entry = Entry {
            id: String::new(),
            name: name.to_string(),
            brand: None,
            urdf: String::new(),
            orientation: [0., 0., 0.],
        };
        Ok(self.assemble(entry, robot))
    }

    /// Turns a parsed description and its meshes into something drawable.
    fn assemble(&self, entry: Entry, robot: Robot) -> Loaded {
        let mut meshes: HashMap<String, Vec<Part>> = HashMap::new();
        let mut missing = Vec::new();
        for link in &robot.links {
            let mut parts = Vec::new();
            for visual in &link.visuals {
                match self.geometry(&entry, &visual.geometry) {
                    Some(mesh) => {
                        for mut part in mesh.parts {
                            part = part.transformed(visual.origin);
                            // A colour on the link overrides the mesh's own,
                            // which is how a description recolours a shared part.
                            if let Some(color) = visual.color {
                                part.color = Some(color);
                            }
                            parts.push(part);
                        }
                    }
                    None => {
                        if let Geometry::Mesh { filename, .. } = &visual.geometry {
                            missing.push(filename.clone());
                        }
                    }
                }
            }
            if !parts.is_empty() {
                meshes.insert(link.name.clone(), parts);
            }
        }

        Loaded {
            entry,
            robot,
            meshes,
            missing,
        }
    }

    fn geometry(&self, entry: &Entry, geometry: &Geometry) -> Option<Mesh> {
        match geometry {
            Geometry::Mesh { filename, scale } => {
                let path = self.resolve(entry, filename)?;
                let source = std::fs::read_to_string(&path).ok()?;
                let mesh = match path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(str::to_ascii_lowercase)
                    .as_deref()
                {
                    Some("dae") => collada::parse(&source).ok()?,
                    Some("obj") => {
                        // The `.mtl` is named inside the file, but every robot
                        // here uses the sibling of the same stem when it has one
                        // at all, and none is not an error.
                        let materials = std::fs::read_to_string(path.with_extension("mtl"))
                            .map(|source| obj::parse_materials(&source))
                            .unwrap_or_default();
                        obj::parse(&source, &materials)
                    }
                    _ => return None,
                };
                Some(Mesh {
                    parts: mesh
                        .parts
                        .into_iter()
                        .map(|part| part.transformed(math::scale(*scale)))
                        .collect(),
                })
            }
            Geometry::Box { size } => Some(shapes::cuboid(*size)),
            Geometry::Cylinder { radius, length } => Some(shapes::cylinder(*radius, *length)),
            Geometry::Sphere { radius } => Some(shapes::sphere(*radius)),
        }
    }

    /// Turns a URDF mesh path into a file path.
    ///
    /// `package://ur10e/meshes/visual/base.dae` names a package, which here is
    /// the directory of that name under the assets root. A bare relative path —
    /// which the Dexee descriptions use — is relative to the robot's own
    /// directory instead. Both are tried either way, because a description that
    /// writes `package://` and means "next to me" is not unusual.
    ///
    /// Anything absolute or climbing out of the assets directory is refused: a
    /// description is a file from outside, and it does not get to name
    /// `/etc/passwd`.
    fn resolve(&self, entry: &Entry, filename: &str) -> Option<PathBuf> {
        let relative = filename
            .strip_prefix("package://")
            .or_else(|| filename.strip_prefix("model://"))
            .or_else(|| filename.strip_prefix("file://"))
            .unwrap_or(filename);
        if relative.starts_with('/') || relative.split('/').any(|segment| segment == "..") {
            return None;
        }
        let from_package = self.root.join(relative);
        let from_robot = self.root.join(&entry.id).join(relative);
        if from_package.is_file() {
            return Some(from_package);
        }
        from_robot.is_file().then_some(from_robot)
    }
}

/// A robot, ready to be drawn.
pub struct Loaded {
    pub entry: Entry,
    pub robot: Robot,
    /// Drawable parts per link name, already in the link's own frame.
    pub meshes: HashMap<String, Vec<Part>>,
    /// Meshes the description named that could not be read, for the log.
    pub missing: Vec<String>,
}

impl Loaded {
    pub fn triangle_count(&self) -> usize {
        self.meshes
            .values()
            .flatten()
            .map(Part::triangle_count)
            .sum()
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, CatalogError> {
    let source = std::fs::read_to_string(path).map_err(|source| CatalogError::Read {
        path: path.display().to_string(),
        source,
    })?;
    serde_json::from_str(&source).map_err(|source| CatalogError::Json {
        path: path.display().to_string(),
        source,
    })
}
