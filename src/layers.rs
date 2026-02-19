use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Debug, Display};

pub type Packages = BTreeMap<Name, Package>;
pub type Layers = BTreeMap<Name, Layer>;
/// records how often a package is a dependency
pub type PackagePopularity = BTreeMap<Name, u32>;

/// a colcon `package.xml` description
#[derive(Deserialize, Debug, Clone)]
pub struct ColconPackage {
    name: String,
    depend: Option<Vec<String>>,
    build_depend: Option<Vec<String>>,
    exec_depend: Option<Vec<String>>,
}

/// The name of a local dependency or system dependency
#[derive(PartialEq, PartialOrd, Eq, Ord, Clone, Serialize, Deserialize, Hash)]
#[repr(transparent)]
pub struct Name(String);

impl From<String> for Name {
    fn from(value: String) -> Self {
        Name(value)
    }
}

impl Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl AsRef<str> for Name {
    fn as_ref(&self) -> &str {
        self.0.as_ref()
    }
}

// Name is mostly redundant when printing
impl Debug for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl Name {
    fn as_str(&self) -> &str {
        self.as_ref()
    }
}

#[derive(Debug)]
pub struct Package {
    path: Utf8PathBuf,
    build: BTreeSet<Name>,
    exec: BTreeSet<Name>,
}

impl Package {
    fn from_colcon_package(path: Utf8PathBuf, colcon_package: ColconPackage) -> Self {
        let mut build = colcon_package.build_depend.unwrap_or_default();
        let mut exec = colcon_package.exec_depend.unwrap_or_default();
        if let Some(depend) = colcon_package.depend {
            build.extend(depend.clone());
            exec.extend(depend);
        }

        Self {
            path,
            build: build.into_iter().map(Into::into).collect(),
            exec: exec.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Layer {
    name: Name,
    source: Source,
    dependencies: Dependencies,
}

/// ensure correct ordering of layers such that they respect Docker rules
impl PartialOrd for Layer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Layer {
    fn cmp(&self, other: &Self) -> Ordering {
        self.name.cmp(&other.name)
    }
}

/// a layer either depends on a path, because it uses something from the file system, or a
/// previous layer
#[derive(Debug, Eq, PartialEq)]
pub enum Source {
    Path(Utf8PathBuf),
    LayerName(Name),
}

/// a layer has either no system dependencies or a list of packages
#[derive(Eq, PartialEq, Debug)]
pub struct Dependencies {
    system_dependencies: BTreeSet<Name>,
    local_dependencies: BTreeSet<Name>,
}
